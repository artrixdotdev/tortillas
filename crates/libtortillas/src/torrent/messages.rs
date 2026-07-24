use std::{
   path::PathBuf,
   sync::{Arc, atomic::AtomicU8},
};

use bitvec::vec::BitVec;
use bytes::Bytes;
use kameo::{Reply, messages};
use sha1::{Digest, Sha1};
use tracing::{info, instrument, trace, warn};

use super::{
   AnnounceFrom, BLOCK_SIZE, PieceStorageStrategy, TORRENT_SNAPSHOT_VERSION, TorrentActor,
   TorrentSnapshot, TorrentState,
   actor::{PieceManagerProxy, ReadyHookSender},
   util,
};
use crate::{
   errors::TorrentError,
   frontend::TorrentView,
   hashes::InfoHash,
   metainfo::Info,
   peer::{Peer, PeerId, commands::HaveInfoDict},
   pieces::{PieceManager, PieceScheduler},
   protocol::stream::PeerStream,
   tracker::Tracker,
};

#[derive(Debug, Reply)]
pub(crate) struct SnapshotRestoreResult(pub(crate) Result<bool, TorrentError>);

pub(crate) mod events {
   use super::*;

   #[messages]
   impl TorrentActor {
      /// A message from an announce actor containing new peers.
      #[message(derive(Debug))]
      #[instrument(skip(self, peers, from), fields(torrent_id = %self.info_hash(), announce_from = from.kind()))]
      pub(crate) fn announce(&mut self, peers: Vec<Peer>, from: AnnounceFrom) {
         trace!(peer_count = peers.len(), "Received announce message");
         for peer in peers {
            self.append_peer(peer, None);
         }
      }

      /// Sent after an incoming peer initializes a handshake.
      /// The handshake will be preverified and routed to this torrent instance.
      ///
      /// We as the instance are expected to reply to said handshake, this is
      /// not the responsibility of the engine.
      #[message]
      #[instrument(skip(self, stream), fields(torrent_id = %self.info_hash()))]
      pub(crate) fn incoming_peer(&mut self, peer: Peer, stream: PeerStream) {
         self.append_peer(peer, Some(stream));
      }

      /// Used to manually add a peer. This is primarily used for testing but
      /// can be used to initiate a peer connection without it having to
      /// come from an announce.
      #[message(derive(Debug))]
      #[instrument(skip(self), fields(torrent_id = %self.info_hash()))]
      pub(crate) fn add_peer(&mut self, peer: Peer) {
         self.append_peer(peer, None);
      }

      /// Sent by a connection task after peer handshaking completes.
      #[message]
      #[instrument(skip(self, stream), fields(torrent_id = %self.info_hash()))]
      pub(crate) fn peer_connected(&mut self, peer: Peer, stream: PeerStream) {
         self.insert_peer(peer, stream);
      }

      /// Index, offset, and data for a received peer `Piece` message.
      #[message(derive(Debug))]
      #[instrument(skip(self, block), fields(torrent_id = %self.info_hash()))]
      pub(crate) async fn incoming_piece(
         &mut self, peer_id: PeerId, index: usize, offset: usize, block: Bytes,
      ) {
         self
            .handle_incoming_piece(peer_id, index, offset, block)
            .await;
      }

      /// Release a scheduler entry for a request a peer could not accept.
      #[message(derive(Debug, Clone, Copy))]
      #[instrument(skip(self), fields(torrent_id = %self.info_hash()))]
      pub(crate) fn peer_rejected_request(&mut self, index: usize, offset: usize) {
         self.piece_scheduler.release_request(index, offset);
      }

      /// Bytes for the [`Info`] dict from a peer. These info bytes are expected
      /// to be verified by the torrent before being used.
      #[message(derive(Debug))]
      #[instrument(skip(self, bytes), fields(torrent_id = %self.info_hash()))]
      pub(crate) async fn info_bytes(&mut self, bytes: Bytes) {
         if self.info.is_some() {
            trace!(
               dict = %String::from_utf8_lossy(&bytes),
               "Received info dict when we already have one"
            );
            return;
         }
         let mut hasher = Sha1::new();

         hasher.update(&bytes);
         let hash = hex::encode(hasher.finalize());
         if hash == self.info_hash().to_hex() {
            info!("Received valid info dict, starting torrent process...");
            let info: Info = serde_bencode::from_bytes(&bytes).expect("Failed to parse info dict");
            self.bitfield = BitVec::repeat(false, info.piece_count());
            self.info = Some(info);
            if self.state == TorrentState::ResolvingMetadata {
               self.transition_state(TorrentState::Added);
            }
            self.frontend.metadata_resolved(self.live_view());
            self
               .broadcast_to_peers(HaveInfoDict {
                  bitfield: Arc::new(self.bitfield.clone()),
               })
               .await;
         } else {
            warn!(
               dict = %String::from_utf8_lossy(&bytes),
               "Received invalid info hash"
            );
         }
      }

      /// Sent after `PeerActor::on_start` runs.
      #[message(derive(Debug, Clone, Copy))]
      #[instrument(skip(self), fields(torrent_id = %self.info_hash()))]
      pub(crate) async fn peer_ready(&mut self, id: PeerId) {
         if let Some(actor) = self.peers.get(&id)
            && actor.is_alive()
            && self.state == TorrentState::Downloading
            && self.is_ready()
         {
            self
               .request_blocks_from_peer(id, self.settings.torrent.max_in_flight_per_peer)
               .await;
            trace!(peer_id = %id, "Filled peer request window");
         } else {
            trace!(peer_id = %id, state = ?self.state, ready = self.is_ready(), "Ignoring PeerReady: peer unknown, dead, or torrent not in download state");
         }
      }
   }
}

pub(crate) mod commands {
   use super::*;

   #[messages]
   impl TorrentActor {
      #[message]
      pub(crate) fn kill_peer(&mut self, id: PeerId, frontend: crate::frontend::PeerHandle) {
         self.piece_scheduler.peer_disconnected(id);
         // Kill the actor quietly.
         if let Some(actor) = self.peers.remove(&id) {
            actor.kill();
         }
         frontend.disconnected(Some(self.live_view()));
      }

      #[message]
      pub(crate) fn kill_tracker(&mut self, tracker: Tracker) {
         // Kill the actor quietly.
         if let Some(actor) = self.trackers.get(&tracker) {
            actor.kill();
            self.trackers.remove(&tracker);
            self.frontend.update_torrent(self.live_view());
         } else {
            warn!("Received kill tracker message for unknown tracker");
         }
      }

      #[message]
      pub(crate) async fn set_piece_storage(&mut self, strategy: PieceStorageStrategy) {
         if !self.is_empty() {
            // Intentional panic because this is unintended behavior.
            panic!("Cannot change piece storage strategy after we've already received pieces");
         }
         if let PieceStorageStrategy::Disk(dir) = &strategy {
            util::create_dir(dir).await.unwrap(); // Intended panic
         }
         self.piece_storage = strategy;
         self.frontend.update_torrent(self.live_view());
      }

      /// Sets the current piece manager to a custom implementation.
      #[message]
      pub(crate) async fn set_piece_manager(&mut self, manager: Box<dyn PieceManager>) {
         // Intentional panic, the program should not run if this is not the case.
         assert!(
            matches!(self.piece_storage, PieceStorageStrategy::Disk(_)),
            "Storage strategy **must** be set to disk before the piece manager is changed",
         );

         self.piece_manager = PieceManagerProxy::Custom(manager);
         // If we already have metadata, initialize the replacement manager now.
         if let Some(info) = self.info.clone()
            && let Err(err) = self.piece_manager.pre_start(info).await
         {
            warn!(?err, "Failed to pre-start custom piece manager");
         }
         self.frontend.update_torrent(self.live_view());
      }

      /// Sets the output path, should only be used when the `FilePieceManager`
      /// is used.
      #[message]
      pub(crate) fn set_output_path(&mut self, path: PathBuf) {
         match &mut self.piece_manager {
            PieceManagerProxy::Default(manager) => manager.set_path(path),
            _ => {
               warn!(path = ?path, "Cannot set output path when using a custom piece manager; ignoring.")
            }
         }
         self.frontend.update_torrent(self.live_view());
      }

      /// Start the torrenting process & actually start downloading
      /// pieces/seeding.
      #[message]
      pub(crate) async fn set_state(&mut self, state: TorrentState) {
         match state {
            TorrentState::Downloading | TorrentState::Seeding => self.start().await,
            TorrentState::Paused => self.stop_transfer().await,
            state => self.transition_state(state),
         }
      }

      #[message]
      pub(crate) async fn set_auto_start(&mut self, auto: bool) {
         self.autostart = auto;
         if !self.pending_start {
            self.autostart().await;
         }
         self.frontend.update_torrent(self.live_view());
      }

      #[message]
      pub(crate) async fn set_sufficient_peers(&mut self, peers: usize) {
         self.sufficient_peers = peers;
         if !self.pending_start {
            self.autostart().await;
         }
         self.frontend.update_torrent(self.live_view());
      }

      /// Restores persisted piece and lifecycle state before exposing a resumed
      /// torrent to callers.
      #[message]
      pub(crate) fn restore_snapshot(
         &mut self, snapshot: TorrentSnapshot,
      ) -> SnapshotRestoreResult {
         let result = (|| -> Result<bool, TorrentError> {
            if snapshot.version != TORRENT_SNAPSHOT_VERSION {
               return Err(TorrentError::InvalidSnapshot {
                  reason: format!(
                     "unsupported version {}; expected {}",
                     snapshot.version, TORRENT_SNAPSHOT_VERSION
                  ),
               });
            }
            if snapshot.info_hash != self.info_hash() {
               return Err(TorrentError::InvalidSnapshot {
                  reason: "info hash does not match metainfo".to_string(),
               });
            }

            if let Some(info) = &snapshot.info_dict {
               let restored_hash = info.hash().map_err(|error| TorrentError::InvalidSnapshot {
                  reason: format!("failed to hash restored info dictionary: {error}"),
               })?;
               if restored_hash != snapshot.info_hash {
                  return Err(TorrentError::InvalidSnapshot {
                     reason: "restored info dictionary does not match the info hash".to_string(),
                  });
               }
            }

            let info = snapshot.info_dict.as_ref().or_else(|| self.info_dict());
            let piece_count = info.map_or(0, Info::piece_count);
            if snapshot.bitfield.len() != piece_count {
               return Err(TorrentError::InvalidSnapshot {
                  reason: format!(
                     "bitfield has {} pieces but metadata declares {piece_count}",
                     snapshot.bitfield.len()
                  ),
               });
            }
            for entry in &snapshot.block_map {
               let index = *entry.key();
               if index >= piece_count {
                  return Err(TorrentError::InvalidSnapshot {
                     reason: "partial piece index is outside the metadata piece range".to_string(),
                  });
               }
               if snapshot.bitfield[index] {
                  return Err(TorrentError::InvalidSnapshot {
                     reason: "completed piece also contains partial block state".to_string(),
                  });
               }

               let Some(info) = info else {
                  return Err(TorrentError::InvalidSnapshot {
                     reason: "partial block state requires resolved metadata".to_string(),
                  });
               };
               let piece_length = usize::try_from(info.piece_length).map_err(|_| {
                  TorrentError::InvalidSnapshot {
                     reason: "piece length cannot be represented on this platform".to_string(),
                  }
               })?;
               if piece_length == 0 {
                  return Err(TorrentError::InvalidSnapshot {
                     reason: "piece length must be greater than zero".to_string(),
                  });
               }
               let last_piece = piece_count.saturating_sub(1);
               let concrete_length = if index == last_piece {
                  let remainder = info.total_length() % piece_length;
                  if remainder == 0 {
                     piece_length
                  } else {
                     remainder
                  }
               } else {
                  piece_length
               };
               let expected_blocks = concrete_length.div_ceil(BLOCK_SIZE);
               if entry.value().len() != expected_blocks {
                  return Err(TorrentError::InvalidSnapshot {
                     reason: format!(
                        "partial piece {index} has {} blocks; expected {expected_blocks}",
                        entry.value().len()
                     ),
                  });
               }
            }

            let resume = snapshot.state.is_transfer_active();
            let restored_state = match snapshot.state {
               TorrentState::Downloading
               | TorrentState::Seeding
               | TorrentState::Stopping
               | TorrentState::Stopped => TorrentState::Paused,
               state => state,
            };
            let mut scheduler = PieceScheduler::new(piece_count);
            for index in snapshot.bitfield.iter_ones() {
               scheduler.mark_piece_complete(index);
            }
            for entry in &snapshot.block_map {
               scheduler.restore_piece_blocks(*entry.key(), entry.value().clone());
            }

            self.info = snapshot.info_dict;
            self.bitfield = snapshot.bitfield;
            self.piece_scheduler = scheduler;
            self.autostart = snapshot.auto_start;
            self.sufficient_peers = snapshot.sufficient_peers;
            self.transition_state(restored_state);
            self.frontend.update_torrent(self.live_view());

            Ok(resume)
         })();

         SnapshotRestoreResult(result)
      }

      #[message(derive(Debug, Clone, Copy))]
      pub(crate) async fn rechoke(&mut self) {
         self.rechoke_peers().await;
         self.schedule_next_rechoke().await;
      }

      /// A hook that is called when the torrent is ready to start downloading.
      /// This is used to implement
      /// [`Torrent::poll_ready`](crate::torrent::Torrent::poll_ready).
      ///
      /// Only should be used internally.
      #[message]
      pub(crate) async fn ready_hook(&mut self, hook: ReadyHookSender) {
         if self.state == TorrentState::Ready || self.state.is_transfer_active() {
            let _ = hook.send(());
            return;
         }

         let is_ready = self.is_ready_to_start();
         if is_ready && !self.autostart {
            let _ = hook.send(());
         } else {
            self.ready_hook.push(hook);
            self.autostart().await;
         }
      }

      /// Bitfield of the torrent.
      #[message]
      pub(crate) fn get_bitfield(&self) -> Arc<BitVec<AtomicU8>> {
         Arc::new(self.bitfield.clone())
      }

      /// Whether a peer has pieces this torrent still needs, plus the number of
      /// interesting pieces.
      #[message]
      pub(crate) fn interesting_pieces(
         &self, peer_bitfield: Arc<BitVec<AtomicU8>>,
      ) -> (bool, usize) {
         let interesting_count = if self.bitfield.is_empty() {
            peer_bitfield.count_ones()
         } else {
            peer_bitfield
               .iter()
               .by_vals()
               .zip(self.bitfield.iter().by_vals())
               .filter(|(peer_has_piece, we_have_piece)| *peer_has_piece && !*we_have_piece)
               .count()
         };
         (interesting_count > 0, interesting_count)
      }

      #[message]
      pub(crate) fn peer_count(&self) -> usize {
         self.peers.len()
      }

      /// Info hash of the torrent.
      #[message]
      pub(crate) fn get_info_hash(&self) -> InfoHash {
         self.info_hash()
      }

      /// Sends the current info dict if we have it.
      #[message]
      pub(crate) fn has_info_dict(&self) -> Option<Info> {
         self.info.clone()
      }

      /// Requests a piece from the torrent.
      #[message]
      pub(crate) async fn request_piece(
         &mut self, index: usize, offset: usize, length: usize,
      ) -> (usize, usize, Option<Bytes>) {
         let Some(info) = self.info.as_ref() else {
            warn!(
               index,
               offset, length, "Peer requested block before info dict was available"
            );
            return (index, offset, None);
         };

         if length == 0 {
            warn!(
               index,
               offset, length, "Peer requested block with zero length"
            );
            return (index, offset, None);
         }

         if length > BLOCK_SIZE {
            warn!(
               index,
               offset, length, "Peer requested block larger than maximum size"
            );
            return (index, offset, None);
         }

         let piece_count = info.piece_count();
         if index >= piece_count {
            warn!(
               index,
               offset, length, piece_count, "Peer requested out-of-bounds piece"
            );
            return (index, offset, None);
         }

         let piece_length = info.piece_length as usize;
         let total_length = info.total_length();
         let last_piece_index = piece_count.saturating_sub(1);
         let concrete_piece_len = if index == last_piece_index {
            let remainder = total_length % piece_length;
            if remainder == 0 {
               piece_length
            } else {
               remainder
            }
         } else {
            piece_length
         };

         let Some(end) = offset.checked_add(length) else {
            warn!(
               index,
               offset, length, "Peer requested block with overflowing bounds"
            );
            return (index, offset, None);
         };
         if offset >= concrete_piece_len || end > concrete_piece_len {
            warn!(
               index,
               offset, length, concrete_piece_len, "Peer requested block outside piece bounds"
            );
            return (index, offset, None);
         }

         let data = if self
            .bitfield
            .get(index)
            .as_deref()
            .copied()
            .unwrap_or(false)
         {
            match self.read_piece_block(index, offset, length).await {
               Ok(data) => Some(data),
               Err(err) => {
                  warn!(
                     ?err,
                     index, offset, length, "Failed to read requested piece block"
                  );
                  None
               }
            }
         } else {
            warn!(index, offset, length, "Peer requested piece we do not have");
            None
         };
         (index, offset, data)
      }

      #[message]
      pub(crate) fn get_state(&self) -> TorrentState {
         self.state
      }

      #[message]
      pub(crate) fn get_live_view(&self) -> Box<TorrentView> {
         Box::new(self.live_view())
      }

      #[message]
      pub(crate) fn snapshot_state(&self) -> Box<TorrentSnapshot> {
         Box::new(self.snapshot())
      }
   }
}
