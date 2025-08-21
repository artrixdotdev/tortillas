use std::{collections::HashMap, time::Instant};

use anyhow::Context;
use bytes::Bytes;
use kameo::{
   Actor,
   actor::{ActorRef, WeakActorRef},
   mailbox::Signal,
   prelude::{Context as KameoContext, MailboxReceiver, Message},
};
use messages::{ExtendedMessage, ExtendedMessageType, PeerMessages};
use stream::{PeerSend, PeerStream};
use tracing::{debug, error, instrument, trace, warn};

use crate::{
   errors::PeerActorError,
   peer::Peer,
   protocol::{stream::PeerRecv, *},
   torrent::{Torrent, TorrentMessage, TorrentRequest, TorrentResponse},
};

const PEER_KEEPALIVE_TIMEOUT: u64 = 10;
const PEER_DISCONNECT_TIMEOUT: u64 = 20;

pub(crate) struct PeerActor {
   peer: Peer,
   stream: PeerStream,
   supervisor: ActorRef<Torrent>,
}

impl PeerActor {
   /// Sends an extended handshake in return if the received extended message
   /// was a handshake.
   async fn send_extended_handshake(&mut self, extended_id: u8) {
      // If this is an Extended handshake, send a handshake in response.
      if extended_id == 0 {
         let mut supported_extensions = HashMap::new();
         supported_extensions.insert("ut_metadata".into(), 2);
         let mut extended_message = ExtendedMessage::new();
         extended_message.supported_extensions = Some(supported_extensions);
         let message = PeerMessages::Extended(extended_id, Box::new(Some(extended_message)), None);

         trace!("Sending extended handshake response");

         self
            .stream
            .send(message)
            .await
            .expect("Something went wrong when sending the Extended Message handshake");
      }
   }

   async fn handle_extended_message(
      &mut self, extended_id: u8, extended_message: Option<ExtendedMessage>,
      metadata: &Option<Bytes>,
   ) {
      trace!(extended_id, "Received extended message");

      if let Some(extended_message) = extended_message {
         if let Some(size) = extended_message.metadata_size {
            trace!(metadata_size = size, "Received metadata size from peer");
         }

         // Save metadata to Peer
         if extended_message.is_bep_0009_data().unwrap_or(false) {
            if let Some(inner_metadata) = metadata {
               if let Err(e) = self.peer.info.append_to_bytes(inner_metadata) {
                  warn!(error = %e, "Failed to append metadata bytes");
               } else {
                  trace!(metadata_len = inner_metadata.len(), "Appended metadata");
               }
            }
         }

         if let Ok(id) = extended_message.supports_bep_0009() {
            self.peer.set_bep_0009(id);

            // If peer has metadata and we don't already have it, request metadata from peer
            if let Some(metadata_size) = extended_message.metadata_size {
               let piece_num = extended_message.piece.unwrap_or(0);
               self.request_metadata(metadata_size, piece_num).await;
            }
         }
      }

      if self.peer.info.have_all_bytes() {
         debug!("Peer has all info bytes, sending them to supervisor...");
         self
            .supervisor
            .tell(TorrentMessage::InfoBytes(self.peer.info.info_bytes()))
            .await
            .unwrap();
      }
   }

   #[instrument(skip(self), fields(addr = %self.stream, id = %self.peer.id.unwrap()))]
   async fn request_metadata(&mut self, metadata_size: usize, piece: usize) {
      trace!(metadata_size, piece, "Requesting metadata");

      self.peer.info.set_info_size(metadata_size);

      if self.peer.info.info_size() > 0 && !self.peer.info.have_all_bytes() {
         trace!(piece, "Preparing metadata request");

         let mut extended_message = ExtendedMessage::new();
         extended_message.piece = Some(piece);
         extended_message.msg_type = Some(ExtendedMessageType::Request);

         let message = PeerMessages::Extended(
            self.peer.bep_0009_id(),
            Box::new(Some(extended_message)),
            None,
         );

         debug!(
            bep_0009_id = self.peer.bep_0009_id(),
            piece, "Sending metadata request"
         );

         if let Err(e) = self.stream.send(message).await {
            error!(error = %e, piece, "Failed to send metadata request");
         }
      } else {
         warn!(
            info_size = self.peer.info.info_size(),
            "Peer already has all metadata bytes or info size is invalid"
         );
      }
   }

   /// Checks if the peer has any bits in the bitfield that we don't have, and
   /// sends an interested message if so.
   #[instrument(skip(self), fields(addr = %self.stream, id = %self.peer.id.unwrap()))]
   async fn determine_interest(&mut self) {
      let msg = TorrentRequest::Bitfield;
      let our_bitfield = match self.supervisor.ask(msg).await.unwrap() {
         TorrentResponse::Bitfield(bitfield) => bitfield,
         _ => unreachable!("Unexpected response from supervisor"),
      };
      let their_bitfield = self.peer.pieces.clone();

      // Find pieces the peer has that we don't have
      let peer_has_we_dont = their_bitfield & !our_bitfield.clone();

      let has_interesting_pieces = peer_has_we_dont.any();

      if our_bitfield.is_empty() || has_interesting_pieces {
         self
            .stream
            .send(PeerMessages::Interested)
            .await
            .expect("Failed to send Interested message to peer");

         debug!(
            "Peer has {} pieces we are interested in",
            peer_has_we_dont.count_ones()
         );
      } else {
         self
            .stream
            .send(PeerMessages::NotInterested)
            .await
            .expect("Failed to send NotInterested message to peer");

         debug!("Peer has no pieces we need - not interested");
      }

      self.peer.set_am_interested(has_interesting_pieces);
   }
}

impl Actor for PeerActor {
   type Args = (Peer, PeerStream, ActorRef<Torrent>);
   type Error = PeerActorError;

   /// At this point, the peer has already been handshaked with. No other
   /// messages have been sent or received from the peer.
   async fn on_start(args: Self::Args, _: ActorRef<Self>) -> Result<Self, Self::Error> {
      let (peer, mut stream, supervisor) = args;
      let msg = TorrentRequest::Bitfield;
      let bitfield = match supervisor.ask(msg).await.unwrap() {
         TorrentResponse::Bitfield(bitfield) => bitfield,
         _ => unreachable!("Unexpected response from supervisor"),
      };

      stream.send(PeerMessages::Bitfield(bitfield)).await?;

      Ok(Self {
         peer,
         stream,
         supervisor,
      })
   }
   /// Coerces messages from the [PeerStream] to a [Message]
   async fn next(
      &mut self, actor_ref: WeakActorRef<Self>, mailbox_rx: &mut MailboxReceiver<Self>,
   ) -> Option<Signal<Self>> {
      // Disconnects the peer by killing the actor if a message has not been received
      // within the last 15 seconds, *only after* sending a KeepAlive message when a
      // message has not been received with the last 10 seconds.
      let last_message = self
         .peer
         .last_message_received()
         .unwrap_or_else(Instant::now)
         .elapsed()
         .as_secs();

      if last_message > PEER_KEEPALIVE_TIMEOUT {
         self
            .stream
            .send(PeerMessages::KeepAlive)
            .await
            .context("Peer connection closed")
            .expect("Failed to send Keep Alive message to peer");
      }

      if last_message > PEER_DISCONNECT_TIMEOUT {
         let id = self.peer.id.expect("Peer ID should exist");
         self
            .supervisor
            .tell(TorrentMessage::KillPeer(id))
            .await
            .expect("Failed to tell supervisor to kill peer");
         // The supervisor will kill the peer manually, no need to do it
         // ourselves.
      }

      tokio::select! {
         signal = mailbox_rx.recv() => signal,
         msg = self.stream.recv() => {
            Some(Signal::Message {
               message: Box::new(msg.expect("PeerStream closed")),
               actor_ref: actor_ref.upgrade().unwrap(),
               reply: None,
               sent_within_actor: true,
            })
         }
      }
   }
}

impl Message<PeerMessages> for PeerActor {
   type Reply = ();

   #[instrument(skip(self), fields(addr = %self.stream, id = %self.peer.id.unwrap()))]
   async fn handle(
      &mut self, msg: PeerMessages, _: &mut KameoContext<Self, Self::Reply>,
   ) -> Self::Reply {
      self.peer.update_last_message_received();
      match msg {
         PeerMessages::Piece(index, offset, data) => {
            trace!(
               piece_index = index,
               offset,
               data_len = data.len(),
               "Received piece data"
            );
            self.peer.increment_bytes_downloaded(data.len());
            let supervisor_msg =
               TorrentMessage::IncomingPiece(index as usize, offset as usize, data);

            let _ = self
               .supervisor
               .tell(supervisor_msg)
               .await
               .context("Failed to send piece to tracker");
         }
         PeerMessages::Choke => {
            self.peer.set_am_choked(true);
            debug!("Peer choked us");
         }
         PeerMessages::Unchoke => {
            self.peer.update_last_optimistic_unchoke();
            self.peer.set_am_choked(false);
            debug!("Peer unchoked us");
         }
         PeerMessages::Interested => {
            self.peer.set_interested(true);
            debug!("Peer is interested in our pieces");
         }
         PeerMessages::NotInterested => {
            self.peer.set_interested(false);
            debug!("Peer is not interested in our pieces");
         }
         PeerMessages::KeepAlive => {
            trace!("Received keep alive");
            self
               .stream
               .send(PeerMessages::KeepAlive)
               .await
               .expect("Failed to send keep alive");
         }
         PeerMessages::Have(piece_index) => {
            trace!(piece_index, "Peer has a piece");
            self.peer.pieces.set(piece_index as usize, true);
         }
         PeerMessages::Request(index, offset, length) => {
            trace!(
               piece_index = index,
               offset, length, "Peer requested piece data"
            );
            let supervisor_message =
               TorrentRequest::Request(index as usize, offset as usize, length as usize);
            let res = self
               .supervisor
               .ask(supervisor_message)
               .await
               .context("Failed to send piece to tracker")
               .expect("Failed to get piece");

            match res {
               TorrentResponse::Request(index, offset, data) => {
                  self
                     .stream
                     .send(PeerMessages::Piece(index as u32, offset as u32, data))
                     .await
                     .expect("Failed to send piece");
               }
               _ => unreachable!(),
            };
         }
         PeerMessages::Extended(extended_id, extended_message, metadata) => {
            self
               .handle_extended_message(extended_id, *extended_message, &metadata)
               .await;

            // If the message is a handshake, send a handshake in return
            if extended_id == 0 {
               self.send_extended_handshake(0).await;
            }
         }
         PeerMessages::Cancel(index, offset, length) => {
            trace!(
               piece_index = index,
               offset, length, "Peer cancelled piece request"
            );
            todo!()
         }
         PeerMessages::Bitfield(bitfield) => {
            let piece_count = bitfield.len();
            debug!(piece_count, "Received bitfield from peer");
            self.peer.pieces = bitfield;
            self.determine_interest().await;
         }
         PeerMessages::Handshake(_) => {
            warn!("Received unexpected handshake from peer");
         }
      }
   }
}

#[allow(dead_code)]
pub(crate) enum PeerTell {
   NeedPiece(usize, usize, usize),
}

impl Message<PeerTell> for PeerActor {
   type Reply = ();
   async fn handle(&mut self, msg: PeerTell, _: &mut KameoContext<Self, Self::Reply>) {
      match msg {
         PeerTell::NeedPiece(index, begin, length) => {
            let piece_exists = self.peer.pieces[index];

            if !piece_exists {
               return;
            }

            self
               .stream
               .send(PeerMessages::Request(
                  index as u32,
                  begin as u32,
                  length as u32,
               ))
               .await
               .expect("Failed to send piece request");
         }
      }
   }
}
