use std::{
   collections::HashMap,
   sync::{Arc, atomic::AtomicU8},
   time::Instant,
};

use anyhow::Context;
use bitvec::vec::BitVec;
use bytes::Bytes;
use dashmap::DashSet;
use kameo::{
   Actor,
   actor::{ActorRef, WeakActorRef},
   mailbox::Signal,
   prelude::{Context as KameoContext, MailboxReceiver, Message},
};
use messages::{ExtendedMessage, ExtendedMessageType, PeerMessages};
use stream::{PeerSend, PeerStream};
use tracing::{debug, info, instrument, trace, warn};

use crate::{
   errors::PeerActorError,
   hashes::InfoHash,
   peer::Peer,
   protocol::{stream::PeerRecv, *},
   torrent::{TorrentActor, TorrentMessage, TorrentRequest, TorrentResponse},
};

const PEER_KEEPALIVE_TIMEOUT: u64 = 10;
const PEER_DISCONNECT_TIMEOUT: u64 = 20;

/// The actor that handles all communications with a given peer.
pub(crate) struct PeerActor {
   /// The peers state and statistics
   peer: Peer,
   /// The stream connecting to the peer -- either TCP or uTP
   stream: PeerStream,
   /// The [TorrentActor] that manages this peer
   supervisor: ActorRef<TorrentActor>,

   pending_block_requests: Arc<DashSet<(usize, usize, usize)>>,
   pending_message_requests: Vec<PeerMessages>,
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

         self
            .stream
            .send(message)
            .await
            .expect("Something went wrong when sending the Extended Message handshake");
      }
   }

   #[instrument(skip(self, actor_ref, signal), fields(peer_addr = %self.stream, peer_id = %self.peer.id.unwrap()))]
   fn check_message_signal(
      &mut self, actor_ref: WeakActorRef<Self>, signal: Result<PeerMessages, PeerActorError>,
   ) -> Option<Signal<Self>> {
      match signal {
         Ok(msg) => Some(Signal::Message {
            message: Box::new(msg),
            actor_ref: actor_ref.upgrade()?.clone(),
            reply: None,
            sent_within_actor: true,
         }),
         Err(e) => {
            use std::io::ErrorKind::*;
            debug!(error = ?e, "Peer errored");
            match e {
               PeerActorError::Io(e) | PeerActorError::ReceiveFailed(e) => {
                  if e.kind() == UnexpectedEof || e.kind() == ConnectionReset {
                     Some(Signal::Stop)
                  } else {
                     None
                  }
               }
               _ => None,
            }
         }
      }
   }

   /// Handles an incoming extended message.
   ///
   /// This message will handle Extended handshakes as specified in [BEP
   /// 0010](https://www.bittorrent.org/beps/bep_0010.html), and the 3 extension messages as specified
   /// in [BEP 0009](https://www.bittorrent.org/beps/bep_0009.html), with the exception of
   /// `Reject`.
   #[instrument(skip(self, _extended_id, extended_message, metadata), fields(peer_addr = %self.stream, peer_id = %self.peer.id.unwrap()))]
   async fn handle_extended_message(
      &mut self, _extended_id: u8, extended_message: Option<ExtendedMessage>,
      metadata: &Option<Bytes>,
   ) {
      if let Some(extended_message) = extended_message {
         // Save metadata to Peer
         if extended_message.is_bep_0009_data().unwrap_or_default()
            && let Some(metadata) = metadata
         {
            if let Err(e) = self.peer.info.append_to_bytes(metadata) {
               trace!(error = %e, "Failed to append metadata bytes");
            } else {
               // Request next piece if we don't have all the piece bytes
               if !self.peer.info.have_all_bytes() {
                  let next_piece = extended_message.piece.expect("Should always be Some") + 1;

                  self
                     .request_metadata(extended_message.metadata_size, next_piece)
                     .await;
               }
            }
         }

         if let Ok(id) = extended_message.supports_bep_0009() {
            self.peer.set_bep_0009(id);

            // If peer has metadata and we don't already have it, request metadata from peer
            if let Some(metadata_size) = extended_message.metadata_size {
               let needs_info_dict = match self.supervisor.ask(TorrentRequest::HasInfoDict).await {
                  Ok(TorrentResponse::HasInfoDict(r)) => r.is_none(),
                  _ => unreachable!(),
               };

               let piece_num = extended_message.piece.unwrap_or(0);

               // Sends initial metadata request (we have no metadata pieces yet)
               if piece_num == 0 && needs_info_dict {
                  trace!(needs_info_dict, "Sending initial metadata request");
                  self.request_metadata(Some(metadata_size), piece_num).await;
               }
            }
         }
      }

      if self.peer.info.have_all_bytes() {
         trace!("Peer has all info bytes, sending them to supervisor...");
         self
            .supervisor
            .tell(TorrentMessage::InfoBytes(self.peer.info.info_bytes()))
            .await
            .unwrap();
      }
   }

   /// Contains the logic for requesting a piece from a peer under [BEP 0009](https://www.bittorrent.org/beps/bep_0009.html)/[BEP 0010](https://www.bittorrent.org/beps/bep_0010.html). This function expects the exact piece to be specified -- in other words, when requesting the next piece of metadata, this function will not automatically increment the `piece` field. The caller of the function is expected to handle this.
   #[instrument(skip(self), fields(peer_addr = %self.stream, peer_id = %self.peer.id.unwrap()))]
   async fn request_metadata(&mut self, metadata_size: Option<usize>, piece: usize) {
      if let Some(size) = metadata_size {
         self.peer.info.set_info_size(size);
      }

      if self.peer.info.info_size() > 0 && !self.peer.info.have_all_bytes() {
         let mut extended_message = ExtendedMessage::new();
         extended_message.piece = Some(piece);
         extended_message.msg_type = Some(ExtendedMessageType::Request);

         let message = PeerMessages::Extended(
            self.peer.bep_0009_id(),
            Box::new(Some(extended_message)),
            None,
         );

         if let Err(e) = self.stream.send(message).await {
            trace!(error = %e, piece, "Failed to send metadata request");
         }
      } else {
         trace!(
            info_size = self.peer.info.info_size(),
            "Peer already has all metadata bytes or info size is invalid"
         );
      }
   }

   /// Checks if the peer has any bits in the bitfield that we don't have, and
   /// sends an interested message if so.
   #[instrument(skip(self), fields(peer_addr = %self.stream, peer_id = %self.peer.id.unwrap()))]
   async fn determine_interest(&mut self) {
      let msg = TorrentRequest::Bitfield;

      let their_bitfield = self.peer.pieces.clone();
      let our_bitfield = match self.supervisor.ask(msg).await.unwrap() {
         TorrentResponse::Bitfield(bitfield) => bitfield,
         _ => unreachable!("Unexpected response from supervisor"),
      };

      let is_our_bitfield_empty = our_bitfield.is_empty();

      // Find pieces the peer has that we don't have
      let their_bitfield = (*their_bitfield).clone();
      let our_bitfield = (*our_bitfield).clone();
      let peer_has_we_dont = their_bitfield & !our_bitfield;

      let has_interesting_pieces = peer_has_we_dont.any();

      if is_our_bitfield_empty || has_interesting_pieces {
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

   /// Sends all queued messages to the peer. This sends synchronously, and will
   /// not return until each message has been sent. This is because most of
   /// the time we want the messages to be sent in their original order.
   #[instrument(skip(self), fields(peer_addr = %self.stream, peer_id = %self.peer.id.unwrap()))]
   async fn flush_queue(&mut self) {
      let queued_messages = self.pending_message_requests.len();

      while let Some(msg) = self.pending_message_requests.pop() {
         self
            .stream
            .send(msg)
            .await
            .expect("Failed to send message to peer");
      }

      trace!(amount = queued_messages, "Flushed queued messages to peer");
   }

}

impl Actor for PeerActor {
   type Args = (Peer, PeerStream, ActorRef<TorrentActor>, InfoHash);
   type Error = PeerActorError;

   /// At this point, the peer has already been handshaked with. No other
   /// messages have been sent or received from the peer.
   async fn on_start(args: Self::Args, _: ActorRef<Self>) -> Result<Self, Self::Error> {
      let (peer, mut stream, supervisor, info_hash) = args;

      info!(peer_id = %peer.id.unwrap(),  peer_addr = %stream, torrent_id = %info_hash, "Peer connected");
      let msg = TorrentRequest::Bitfield;
      let bitfield = match supervisor.ask(msg).await.unwrap() {
         TorrentResponse::Bitfield(bitfield) => bitfield,
         _ => unreachable!("Unexpected response from supervisor"),
      };

      // Dont send `BitVec::EMPTY`
      if !bitfield.is_empty() {
         stream.send(PeerMessages::Bitfield(bitfield)).await?;
      }

      Ok(Self {
         peer,
         stream,
         supervisor,
         pending_block_requests: Arc::new(DashSet::new()),
         pending_message_requests: Vec::with_capacity(MAX_PENDING_MESSAGES),
      })
   }

   /// Coerces messages from the [PeerStream] to a [Message]
   #[instrument(skip(self, actor_ref, mailbox_rx), fields(peer_addr = %self.stream, peer_id = %self.peer.id.unwrap()))]
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
         msg = self.stream.recv() =>  self.check_message_signal(actor_ref, msg)
      }
   }
}

impl Message<PeerMessages> for PeerActor {
   type Reply = ();

   #[instrument(skip(self, msg), fields(peer_addr = %self.stream, peer_id = %self.peer.id.unwrap(), message = %msg))]
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
            let key = (index as usize, offset as usize, data.len());
            // Only send the piece to the supervisor if they request it or it hasn't been
            // cancelled
            if self.pending_block_requests.contains(&key) {
               self.pending_block_requests.remove(&key);

               let supervisor_msg =
                  TorrentMessage::IncomingPiece(index as usize, offset as usize, data);

               let _ = self
                  .supervisor
                  .tell(supervisor_msg)
                  .await
                  .context("Failed to send piece to tracker");
            }
         }
         PeerMessages::Choke => {
            self.peer.set_am_choked(true);
            trace!("Peer choked us");
         }
         PeerMessages::Unchoke => {
            self.peer.update_last_optimistic_unchoke();
            self.peer.set_am_choked(false);
            trace!("Peer unchoked us");
         }
         PeerMessages::Interested => {
            self.peer.set_interested(true);
            trace!("Peer is interested in our pieces");
         }
         PeerMessages::NotInterested => {
            self.peer.set_interested(false);
            trace!("Peer is not interested in our pieces");
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
            trace!(piece_index, "Peer has a new piece");
            let idx = piece_index as usize;
            if idx < self.peer.pieces.len() {
               let was_set = self.peer.pieces[idx];
               self.peer.pieces.set_aliased(idx, true);
               // If this is a new piece, our interest may change.
               if !was_set {
                  self.determine_interest().await;
               }
            } else {
               warn!(
                  piece_index,
                  len = self.peer.pieces.len(),
                  "Have index out of bounds; ignoring"
               );
            }
         }
         PeerMessages::Request(index, offset, length) => {
            debug!(
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
            // If the message is a handshake, send a handshake in return
            if extended_id == 0 {
               self.send_extended_handshake(0).await;
            }
            self
               .handle_extended_message(extended_id, *extended_message, &metadata)
               .await;
         }
         PeerMessages::Cancel(index, offset, length) => {
            debug!(
               piece_index = index,
               offset, length, "Peer cancelled piece request"
            );
            todo!()
         }
         PeerMessages::Bitfield(bitfield) => {
            self.peer.pieces = bitfield;
            self.determine_interest().await;
         }
         PeerMessages::Handshake(_) => {
            warn!("Received unexpected handshake from peer");
         }
      }
   }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum PeerTell {
   NeedPiece(usize, usize, usize),
   CancelPiece(usize, usize, usize),
   HaveInfoDict(Arc<BitVec<AtomicU8>>),
   Have(usize),
}

impl Message<PeerTell> for PeerActor {
   type Reply = ();

   #[instrument(skip(self), fields(peer_addr = %self.stream, peer_id = %self.peer.id.unwrap()))]
   async fn handle(&mut self, msg: PeerTell, _: &mut KameoContext<Self, Self::Reply>) {
      match msg {
         PeerTell::NeedPiece(index, begin, length) => {
            let piece_exists = matches!(self.peer.pieces.get(index).as_deref(), Some(true));

            if !piece_exists {
               trace!(
                  piece_index = index,
                  "Peer does not have piece, not sending request"
               );
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
            self.pending_block_requests.insert((index, begin, length));
            trace!(piece_index = index, "Sent piece request to peer");
         }
         PeerTell::CancelPiece(index, begin, length) => {
            if !self
               .pending_block_requests
               .contains(&(index, begin, length))
            {
               return; // Silently ignore if we don't have the request
            }
            // TODO: Refactor PeerStream to allow for cancelling requests
            // This can't be done yet because it would require a refactor of PeerStream, for
            // now we'll just ignore the request
            // self
            //   .stream
            //   .send(PeerMessages::Cancel(
            //      index as u32,
            //      begin as u32,
            //      length as u32,
            //   ))
            //   .await
            //   .expect("Failed to send piece request");
            self.pending_block_requests.remove(&(index, begin, length));
         }
         PeerTell::HaveInfoDict(bitfield) => {
            self
               .stream
               .send(PeerMessages::Bitfield(bitfield))
               .await
               .expect("Failed to send bitfield");
            trace!("Sent bitfield to peer");
         }
         PeerTell::Have(piece) => {
            if let Err(e) = self.stream.send(PeerMessages::Have(piece as u32)).await {
               trace!(piece_num = piece, error = %e, "Failed to send Have message to peer");
            }
         }
      }
   }
}
