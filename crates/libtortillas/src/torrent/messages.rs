use std::{
   path::PathBuf,
   sync::{Arc, atomic::AtomicU8},
};

use bitvec::vec::BitVec;
use bytes::Bytes;
use kameo::{Reply, messages};
use sha1::{Digest, Sha1};
use tokio::sync::oneshot;
use tracing::{info, instrument, trace, warn};

use super::{
   AnnounceFrom, BLOCK_SIZE, ConfiguredTracker, ConnectedPeer, PeerDisconnect,
   PieceStorageStrategy, TorrentActor, TorrentSnapshot, TorrentState, ValidatedTorrentState,
   actor::{PieceManagerProxy, ReadyHookSender},
   util,
};
#[cfg(feature = "live")]
use crate::live::TorrentView;
use crate::{
   errors::{TorrentError, map_torrent_communication_error},
   hashes::InfoHash,
   metainfo::Info,
   peer::{PeerId, WirePeer, commands::HaveInfoDict},
   pieces::{PieceManager, PieceScheduler},
   protocol::stream::PeerStream,
   tracker::{Announce, Tracker},
};

#[derive(Debug, Reply)]
pub(crate) struct SnapshotRestoreResult(pub(crate) Result<(), TorrentError>);

pub(crate) mod events {
   use super::*;

   #[messages]
   impl TorrentActor {
      /// A message from an announce actor containing new peers.
      #[message(derive(Debug))]
      #[instrument(skip(self, peers, from), fields(torrent_id = %self.info_hash(), announce_from = from.kind()))]
      pub(crate) fn announce(&mut self, peers: Vec<WirePeer>, from: AnnounceFrom) {
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
      pub(crate) fn incoming_peer(
         &mut self, peer: WirePeer, reserved: [u8; 8], stream: PeerStream,
      ) {
         self.append_peer(peer, Some((stream, reserved)));
      }

      /// Sent by a connection task after peer handshaking completes.
      #[message]
      #[instrument(skip(self, stream), fields(torrent_id = %self.info_hash()))]
      pub(crate) fn peer_connected(
         &mut self, peer: WirePeer, reserved: [u8; 8], stream: PeerStream,
      ) -> Result<ConnectedPeer, TorrentError> {
         self.insert_peer(peer, reserved, stream)
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
      pub(crate) fn peer_rejected_request(&mut self, peer_id: PeerId, index: usize, offset: usize) {
         self
            .piece_scheduler
            .release_peer_request(peer_id, index, offset);
         self.fill_peer_request_window(peer_id);
      }

      /// Bytes for the [`Info`] dict from a peer. These info bytes are expected
      /// to be verified by the torrent before being used.
      #[message(derive(Debug))]
      #[instrument(skip(self, bytes), fields(torrent_id = %self.info_hash()))]
      pub(crate) async fn info_bytes(&mut self, bytes: Bytes) {
         if self.info_dict().is_some() {
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
            let info: Info = match serde_bencode::from_bytes(&bytes) {
               Ok(info) => info,
               Err(error) => {
                  warn!(%error, "Peer supplied an invalid info dictionary");
                  return;
               }
            };
            self.bitfield = BitVec::repeat(false, info.piece_count());
            self.resolved_magnet_info = Some(info);
            if self.state == TorrentState::ResolvingMetadata {
               self.transition_state(TorrentState::Added);
            }
            self.publish_metadata_resolved();
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
      #[message(derive(Debug, Clone))]
      #[instrument(skip(self), fields(torrent_id = %self.info_hash()))]
      pub(crate) fn peer_ready(&mut self, id: PeerId, available_pieces: Arc<BitVec<AtomicU8>>) {
         self
            .piece_scheduler
            .update_peer_availability(id, available_pieces);
         if let Some(actor) = self.peers.get(&id)
            && actor.is_alive()
            && self.state == TorrentState::Downloading
            && self.is_ready()
         {
            self.fill_peer_request_window(id);
            trace!(peer_id = %id, "Filled peer request window");
         } else {
            trace!(peer_id = %id, state = ?self.state, ready = self.is_ready(), "Ignoring PeerReady: peer unknown, dead, or torrent not in download state");
         }
      }
   }

   #[cfg(feature = "live")]
   #[messages]
   impl TorrentActor {
      /// Publishes tracker traffic after an announce attempt.
      #[message]
      pub(crate) fn tracker_metrics_changed(&self) {
         self.publish_metrics_changed();
      }
   }
}

pub(crate) mod commands {
   use super::*;

   #[messages]
   impl TorrentActor {
      #[message(derive(Debug))]
      pub(crate) fn add_peer(
         &mut self, peer: WirePeer,
      ) -> oneshot::Receiver<Result<ConnectedPeer, TorrentError>> {
         let (result_tx, result_rx) = oneshot::channel();
         self.spawn_peer_connection(peer, None, Some(result_tx));
         result_rx
      }

      #[message]
      pub(crate) fn disconnect_peer(
         &mut self, id: PeerId, handle: PeerDisconnect,
      ) -> Result<(), TorrentError> {
         #[cfg(not(feature = "live"))]
         let () = handle;
         if !self.remove_peer(id) {
            return Err(TorrentError::PeerNotFound { peer_id: id });
         }
         crate::live_only!(handle.disconnected());
         self.publish_updated();
         self.fill_all_peer_request_windows();
         Ok(())
      }

      #[message(derive(Debug))]
      pub(crate) async fn add_tracker(
         &mut self, tracker: Tracker,
      ) -> Result<ConfiguredTracker, TorrentError> {
         self.register_tracker_actor(tracker).await
      }

      #[message(derive(Debug))]
      pub(crate) async fn remove_tracker(&mut self, tracker: Tracker) -> Result<(), TorrentError> {
         let endpoint = tracker.redacted_endpoint();
         let actor = self
            .trackers
            .remove(&tracker)
            .ok_or(TorrentError::TrackerNotFound { endpoint })?;
         #[cfg(feature = "live")]
         let tracker_handle = self.hub.tracker_handle(self.info_hash(), &tracker);

         self.actor_ref.unlink(&actor).await;
         // Tracker shutdown reports final metrics through this actor's
         // mailbox. Remove it now and let that best-effort cleanup finish
         // asynchronously instead of waiting on our own mailbox.
         if actor.is_alive() {
            actor.stop_gracefully().await.map_err(|error| {
               TorrentError::ActorCommunicationFailed {
                  operation: "remove tracker",
                  reason: error.to_string(),
               }
            })?;
         }
         crate::live_only! {
            if let Some(tracker_handle) = tracker_handle {
               tracker_handle.stopped();
            }
         }
         self.publish_updated();
         Ok(())
      }

      #[message(derive(Debug))]
      pub(crate) async fn reannounce_tracker(
         &mut self, tracker: Tracker,
      ) -> Result<(), TorrentError> {
         let endpoint = tracker.redacted_endpoint();
         let actor = self
            .trackers
            .get(&tracker)
            .ok_or(TorrentError::TrackerNotFound { endpoint })?;
         actor
            .tell(Announce)
            .await
            .map_err(|error| map_torrent_communication_error("reannounce tracker", error))
      }

      #[message(derive(Debug, Clone, Copy))]
      pub(crate) async fn force_reannounce(&mut self) -> Result<usize, TorrentError> {
         if self.trackers.is_empty() {
            return Err(TorrentError::InvalidOperation {
               operation: "force reannounce",
               reason: "torrent has no configured trackers".to_string(),
            });
         }

         let mut queued = 0usize;
         let mut first_error = None;
         for (tracker, actor) in &self.trackers {
            match actor.tell(Announce).await {
               Ok(()) => queued = queued.saturating_add(1),
               Err(error) => {
                  first_error.get_or_insert_with(|| TorrentError::ActorCommunicationFailed {
                     operation: "force tracker reannounce",
                     reason: format!("{}: {error}", tracker.redacted_endpoint()),
                  });
               }
            }
         }
         first_error.map_or(Ok(queued), Err)
      }

      #[message]
      pub(crate) async fn set_piece_storage(
         &mut self, strategy: PieceStorageStrategy,
      ) -> Result<(), TorrentError> {
         if !self.is_empty() {
            return Err(TorrentError::InvalidOperation {
               operation: "set piece storage",
               reason: "piece storage cannot change after data has been received".to_string(),
            });
         }
         if matches!(&self.piece_manager, PieceManagerProxy::Custom(_))
            && !matches!(&strategy, PieceStorageStrategy::Disk(_))
         {
            return Err(TorrentError::InvalidOperation {
               operation: "set piece storage",
               reason: "custom piece managers require disk piece storage".to_string(),
            });
         }
         if let PieceStorageStrategy::Disk(dir) = &strategy {
            util::create_dir(dir)
               .await
               .map_err(|error| TorrentError::FileIoError {
                  operation: "create piece storage directory".to_string(),
                  reason: error.to_string(),
               })?;
         }
         self.piece_storage = strategy;
         if self.state == TorrentState::Failed {
            self.transition_state(TorrentState::Paused);
         }
         self.publish_updated();
         Ok(())
      }

      /// Sets the current piece manager to a custom implementation.
      #[message]
      pub(crate) async fn set_piece_manager(
         &mut self, mut manager: Box<dyn PieceManager>,
      ) -> Result<(), TorrentError> {
         if !self.is_empty() {
            return Err(TorrentError::InvalidOperation {
               operation: "set piece manager",
               reason: "piece manager cannot change after data has been received".to_string(),
            });
         }
         if !matches!(self.piece_storage, PieceStorageStrategy::Disk(_)) {
            return Err(TorrentError::InvalidOperation {
               operation: "set piece manager",
               reason: "custom piece managers require disk piece storage".to_string(),
            });
         }
         // If we already have metadata, initialize the replacement manager now.
         if let Some(info) = self.info_dict().cloned()
            && let Err(error) = manager.pre_start(info).await
         {
            return Err(TorrentError::InvalidOperation {
               operation: "set piece manager",
               reason: format!("custom piece manager initialization failed: {error}"),
            });
         }
         self.piece_manager = PieceManagerProxy::Custom(manager);
         self.publish_updated();
         Ok(())
      }

      /// Sets the output path, should only be used when the `FilePieceManager`
      /// is used.
      #[message]
      pub(crate) async fn set_output_path(&mut self, path: PathBuf) -> Result<(), TorrentError> {
         if !self.is_empty() {
            return Err(TorrentError::InvalidOperation {
               operation: "set output folder",
               reason: "output folder cannot change after data has been received".to_string(),
            });
         }
         if matches!(&self.piece_manager, PieceManagerProxy::Custom(_)) {
            return Err(TorrentError::InvalidOperation {
               operation: "set output folder",
               reason: "a custom piece manager owns its output paths".to_string(),
            });
         }
         util::create_dir(&path)
            .await
            .map_err(|error| TorrentError::FileIoError {
               operation: "create output folder".to_string(),
               reason: error.to_string(),
            })?;
         if let PieceManagerProxy::Default(manager) = &mut self.piece_manager {
            manager.set_path(path);
         }
         if self.state == TorrentState::Failed {
            self.transition_state(TorrentState::Paused);
         }
         self.publish_updated();
         Ok(())
      }

      /// Start the torrenting process & actually start downloading
      /// pieces/seeding.
      #[message]
      pub(crate) async fn set_state(&mut self, state: TorrentState) -> Result<(), TorrentError> {
         match state {
            TorrentState::Downloading | TorrentState::Seeding => self.start().await,
            TorrentState::Paused => self.stop_transfer().await,
            state => self.transition_state(state),
         }
         Ok(())
      }

      #[message]
      pub(crate) async fn set_auto_start(&mut self, auto: bool) -> Result<(), TorrentError> {
         self.autostart = auto;
         if !self.pending_start {
            self.autostart().await;
         }
         self.publish_updated();
         Ok(())
      }

      #[message]
      pub(crate) async fn set_sufficient_peers(
         &mut self, peers: usize,
      ) -> Result<(), TorrentError> {
         self.sufficient_peers = peers;
         if !self.pending_start {
            self.autostart().await;
         }
         self.publish_updated();
         Ok(())
      }

      /// Restores persisted piece and lifecycle state before exposing a resumed
      /// torrent to callers.
      #[message]
      pub(crate) fn restore_snapshot(
         &mut self, snapshot: ValidatedTorrentState,
      ) -> SnapshotRestoreResult {
         let result = (|| -> Result<(), TorrentError> {
            self.resolved_magnet_info = snapshot.resolved_magnet_info;
            let piece_count = self
               .info_dict()
               .map_or(0, crate::metainfo::Info::piece_count);

            let restored_state = match snapshot.state {
               TorrentState::Downloading
               | TorrentState::Seeding
               | TorrentState::Restarting
               | TorrentState::Stopping
               | TorrentState::Stopped => TorrentState::Paused,
               state => state,
            };
            let mut scheduler = PieceScheduler::new(piece_count);
            for (index, complete) in snapshot.bitfield.iter().copied().enumerate() {
               if !complete {
                  continue;
               }
               scheduler.mark_piece_complete(index);
            }
            for entry in &snapshot.block_map {
               let index = usize::try_from(entry.piece_index).map_err(|_| {
                  TorrentError::InvalidSnapshot {
                     reason: "partial piece index cannot be represented on this platform"
                        .to_string(),
                  }
               })?;
               scheduler.restore_piece_blocks(index, entry.blocks.iter().copied().collect());
            }

            self.bitfield = snapshot.bitfield.iter().copied().collect();
            self.piece_scheduler = scheduler;
            self.autostart = snapshot.auto_start;
            self.sufficient_peers = usize::try_from(snapshot.sufficient_peers).map_err(|_| {
               TorrentError::InvalidSnapshot {
                  reason: "sufficient peer count cannot be represented on this platform"
                     .to_string(),
               }
            })?;
            self.transition_state(restored_state);
            self.publish_updated();

            Ok(())
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
      pub(crate) async fn ready_hook(&mut self, hook: ReadyHookSender) -> Result<(), TorrentError> {
         if self.state == TorrentState::Ready || self.state.is_transfer_active() {
            let _ = hook.send(());
            return Ok(());
         }

         let is_ready = self.is_ready_to_start();
         if is_ready && !self.autostart {
            let _ = hook.send(());
         } else {
            self.ready_hook.push(hook);
            self.autostart().await;
         }
         Ok(())
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
         self.info_dict().cloned()
      }

      /// Requests a piece from the torrent.
      #[message]
      pub(crate) async fn request_piece(
         &mut self, index: usize, offset: usize, length: usize,
      ) -> (usize, usize, Option<Bytes>) {
         let Some(info) = self.info_dict() else {
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
      pub(crate) fn get_state(&self) -> Result<TorrentState, TorrentError> {
         Ok(self.state)
      }

      #[message]
      pub(crate) fn snapshot_state(&self) -> Result<Box<TorrentSnapshot>, TorrentError> {
         self.snapshot().map(Box::new)
      }
   }

   #[cfg(feature = "live")]
   #[messages]
   impl TorrentActor {
      #[message]
      pub(crate) fn kill_peer(&mut self, id: PeerId, handle: crate::live::PeerHandle) {
         self.remove_peer(id);
         handle.disconnected();
         self.publish_updated();
         self.fill_all_peer_request_windows();
      }

      #[message]
      pub(crate) fn get_live_view(&self) -> Box<TorrentView> {
         Box::new(self.live_view())
      }
   }

   #[cfg(not(feature = "live"))]
   #[messages]
   impl TorrentActor {
      #[message]
      pub(crate) fn kill_peer(&mut self, id: PeerId) {
         self.remove_peer(id);
         self.fill_all_peer_request_windows();
      }
   }
}
