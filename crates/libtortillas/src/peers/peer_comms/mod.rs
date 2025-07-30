use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::{
   sync::{Mutex, broadcast, mpsc},
   time::timeout,
};
use tracing::{debug, error, info, trace, warn};

use crate::{
   hashes::InfoHash,
   peers::{
      Peer,
      peer_comms::{
         commands::{PeerCommand, PeerResponse},
         messages::{ExtendedMessage, ExtendedMessageType, PeerMessages},
         stream::{PeerSend, PeerWriter},
      },
   },
};

pub mod commands;
pub mod messages;
pub mod stream;

impl Peer {
   /// Request piece from peer
   pub(crate) async fn request_piece(&mut self, stream: &mut PeerWriter, piece_num: u32) {
      let peer_addr = self.socket_addr();

      // If the peer does not have the piece, don't request it.
      if self.pieces.get(piece_num as usize).unwrap() == false {
         trace!(%peer_addr, piece_num, "Peer does not have requested piece");
         return;
      }

      // https://github.com/vimpunk/cratetorrent/blob/master/PEER_MESSAGES.md#6-request
      //
      // > All current implementations use 2^14 (16 kiB)
      // - BEP 0003
      //
      // For now, we are assuming that the offset is 0. This may need to be changed in
      // the future.
      let request = PeerMessages::Request(piece_num, 0, 16384);
      const REQUEST_TIMEOUT: u64 = 5;

      let request_result =
         timeout(Duration::from_secs(REQUEST_TIMEOUT), stream.send(request)).await;

      match request_result {
         Ok(Ok(())) => {
            trace!(%peer_addr, piece_num, "Piece request sent successfully");
         }
         Ok(Err(send_err)) => {
            error!(%peer_addr, piece_num, error = %send_err, "Failed to send piece request");
         }
         Err(_) => {
            warn!(%peer_addr, piece_num, timeout_secs = REQUEST_TIMEOUT, "Piece request timed out");
         }
      }
   }

   /// A helper function for [handle_peer](Peer::handle_peer). This is a very
   /// beefy function -- refactors that reduce its size are welcome.
   pub(crate) async fn recv_from_peer(
      &mut self, message: PeerMessages, to_engine_tx: broadcast::Sender<PeerResponse>,
      from_engine_tx: mpsc::Sender<PeerCommand>, inner_send_tx: mpsc::Sender<PeerCommand>,
      info_hash: InfoHash,
   ) {
      let peer_addr = self.socket_addr();
      self.update_last_message_received();
      match &message {
         PeerMessages::Piece(index, offset, data) => {
            trace!(%peer_addr, piece_index = index, offset, data_len = data.len(), "Received piece data");
            self.increment_bytes_downloaded(data.len());
            Self::send_to_engine(
               to_engine_tx,
               PeerResponse::Receive {
                  message,
                  peer_key: peer_addr,
               },
               peer_addr,
            );
         }
         PeerMessages::Choke => {
            self.set_am_choked(true);
            debug!(%peer_addr, "Peer choked us");
         }
         PeerMessages::Unchoke => {
            self.update_last_optimistic_unchoke();
            self.set_am_choked(false);
            debug!(%peer_addr, "Peer unchoked us");
            Self::send_to_engine(
               to_engine_tx,
               PeerResponse::Unchoke {
                  from_engine_tx,
                  peer_key: peer_addr,
               },
               peer_addr,
            );
         }
         PeerMessages::Interested => {
            self.set_interested(true);
            debug!(%peer_addr, "Peer is interested in our pieces");
         }
         PeerMessages::NotInterested => {
            self.set_interested(false);
            debug!(%peer_addr, "Peer is not interested in our pieces");
         }
         PeerMessages::KeepAlive => {
            trace!(%peer_addr, "Received keep alive");
         }
         PeerMessages::Have(piece_index) => {
            trace!(%peer_addr, piece_index, "Peer has piece");
         }
         PeerMessages::Request(index, offset, length) => {
            trace!(%peer_addr, piece_index = index, offset, length, "Peer requested piece data");
            Self::send_to_engine(
               to_engine_tx,
               PeerResponse::Receive {
                  message,
                  peer_key: peer_addr,
               },
               peer_addr,
            );
         }
         PeerMessages::Extended(extended_id, extended_message, metadata) => {
            self
               .handle_extended_message(
                  *extended_id,
                  extended_message,
                  metadata,
                  &to_engine_tx,
                  &from_engine_tx,
                  &inner_send_tx,
                  info_hash,
               )
               .await;
         }
         PeerMessages::Cancel(index, offset, length) => {
            trace!(%peer_addr, piece_index = index, offset, length, "Peer cancelled piece request");
            todo!()
         }
         PeerMessages::Bitfield(bitfield) => {
            let piece_count = bitfield.len();
            debug!(%peer_addr, piece_count, "Received bitfield from peer");
            self.pieces = bitfield.clone();

            Self::send_to_engine(
               to_engine_tx,
               PeerResponse::Receive {
                  message,
                  peer_key: peer_addr,
               },
               peer_addr,
            );
         }
         PeerMessages::Handshake(_) => {
            warn!(%peer_addr, "Received unexpected handshake from peer");
         }
      }
   }

   /// Sends an extended handshake in return if the received extended message
   /// was a handshake.
   async fn send_extended_handshake(
      &self, extended_id: u8, inner_send_tx: mpsc::Sender<PeerCommand>,
   ) {
      let peer_addr = self.socket_addr();

      // If this is an Extended handshake, send a handshake in response.
      if extended_id == 0 {
         let mut m = HashMap::new();
         m.insert("ut_metadata".into(), 2);
         let mut extended_message = ExtendedMessage::new();
         extended_message.supported_extensions = Some(m);
         let command = PeerCommand::Extended(0, Some(extended_message));

         trace!(%peer_addr, "Sending extended handshake response");

         if let Err(e) = inner_send_tx.send(command).await {
            error!(%peer_addr, error = %e, "Failed to send extended handshake response");
         }
      }
   }
   #[allow(clippy::too_many_arguments)] // Refactor later
   async fn handle_extended_message(
      &mut self, extended_id: u8, extended_message: &Option<ExtendedMessage>,
      metadata: &Option<Vec<u8>>, to_engine_tx: &broadcast::Sender<PeerResponse>,
      from_engine_tx: &mpsc::Sender<PeerCommand>, inner_send_tx: &mpsc::Sender<PeerCommand>,
      info_hash: InfoHash,
   ) {
      let peer_addr = self.socket_addr();
      trace!(%peer_addr, extended_id, "Received extended message");

      self
         .send_extended_handshake(extended_id, inner_send_tx.clone())
         .await;

      // Save to Peer.
      if let Some(inner_metadata) = metadata
         && let Err(e) = self.info.append_to_bytes(inner_metadata.to_vec())
      {
         warn!(%peer_addr, error = %e, "Failed to append metadata bytes");
      }

      if let Some(extended_message) = extended_message {
         if let Some(size) = extended_message.metadata_size {
            debug!(%peer_addr, metadata_size = size, "Received metadata size from peer");
         }

         if let Ok(id) = extended_message.supports_bep_0009() {
            self.set_bep_0009(id);

            // If peer has metadata and we don't already have it, request metadata from peer
            if let Some(metadata_size) = extended_message.metadata_size {
               let piece_num = extended_message.piece.unwrap_or(0);
               self
                  .request_metadata(metadata_size, inner_send_tx, piece_num)
                  .await;
            }
         }
      }
      self
         .validate_metadata(info_hash, to_engine_tx.clone(), from_engine_tx.clone())
         .await;
   }

   /// Requests metadata, given a piece number.
   async fn request_metadata(
      &mut self, metadata_size: usize, inner_send_tx: &mpsc::Sender<PeerCommand>, piece: usize,
   ) {
      let peer_addr = self.socket_addr();
      self.info.set_info_size(metadata_size);

      if self.info.info_size() > 0 && !self.info.have_all_bytes() {
         let mut extended_message_command = ExtendedMessage::new();
         extended_message_command.piece = Some(piece);
         extended_message_command.msg_type = Some(ExtendedMessageType::Request);

         // The Extended ID as specified in BEP 0009 is the ID from the m dictionary
         // -- in this case the ID listed under ut_metadata
         let command =
            PeerCommand::Extended(self.bep_0009_id() as usize, Some(extended_message_command));

         if let Err(e) = inner_send_tx.send(command).await {
            error!(%peer_addr, error = %e, "Failed to send metadata request");
         } else {
            trace!(%peer_addr, piece, "Requested metadata piece");
         }
      }
   }

   /// If we have all of the metadata as bytes, validate it and send it back to
   /// the engine.
   ///
   /// If the info dicts hash is not correct, there is currently no error
   /// handling (only an error message is shown)
   async fn validate_metadata(
      &self, info_hash: InfoHash, to_engine_tx: broadcast::Sender<PeerResponse>,
      from_engine_tx: mpsc::Sender<PeerCommand>,
   ) {
      let peer_addr = self.socket_addr();

      if self.info.have_all_bytes() {
         debug!(%peer_addr, "Received complete metadata, validating");
         let info = self.info.generate_info_from_bytes(info_hash).await;
         if let Err(e) = info {
            error!(%peer_addr, error = %e, "Metadata validation failed");
         } else {
            // We have to convert back to bytes due to issues with deriving Clone on the
            // Info struct.
            if let Err(e) = to_engine_tx.send(PeerResponse::Info {
               bytes: self.info.info_bytes(),
               peer_key: peer_addr,
               from_engine_tx: from_engine_tx.clone(),
            }) {
               error!(%peer_addr, error = %e, "Failed to send validated metadata to engine");
            } else {
               info!(%peer_addr, "Successfully received and validated metadata from peer");
            }
         }
      }
   }

   /// A helper function for [handle_peer](Peer::handle_peer). This is a very
   /// beefy function -- refactors that reduce its size are welcome.
   pub(crate) async fn send_to_peer(
      &mut self, message: PeerCommand, writer: Arc<Mutex<PeerWriter>>,
      to_engine_tx: broadcast::Sender<PeerResponse>, from_engine_tx: mpsc::Sender<PeerCommand>,
   ) {
      let peer_addr = self.socket_addr();
      trace!(%peer_addr, "Processing command from engine");

      match message {
         PeerCommand::Piece(piece_num) => {
            // If we're choking or the peer isn't interested, we can't do anything.
            if !self.am_choked() && self.interested() {
               let mut writer_guard = writer.lock().await;

               self
                  .request_piece(&mut writer_guard, piece_num as u32)
                  .await;
            } else {
               let am_choked = self.am_choked();
               let interested = self.am_interested();

               debug!(%peer_addr, piece_num, am_choked, interested, "Cannot request piece - peer state prevents it");

               Self::send_to_engine(
                  to_engine_tx,
                  PeerResponse::Choking {
                     peer_key: peer_addr,
                     from_engine_tx,
                  },
                  peer_addr,
               );
            }
         }
         PeerCommand::Extended(id, extended_message) => {
            // Request metadata with an Extended message (if peer supports BEP 0010 and BEP
            // 0009)
            //
            // Note that when we receive the info-dictionary from a peer, we absolutely must
            // compare the hash of it to our info hash.
            if self.supports_bep_0009() && self.supports_bep_0010() {
               let message = PeerMessages::Extended(id as u8, Box::new(extended_message), None);

               {
                  let mut writer_guard = writer.lock().await;
                  if let Err(e) = writer_guard.send(message).await {
                     error!(%peer_addr, error = %e, "Failed to send extended message");
                     return;
                  }
               }

               trace!(%peer_addr, extended_id = id, "Sent extended message to peer");
            }

            trace!("Peer does not support BEP 0009 or BEP 0010");
         }
      }
   }
}
