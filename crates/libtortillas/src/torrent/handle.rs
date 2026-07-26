use std::{
   fmt,
   path::PathBuf,
   sync::{Arc, Weak},
};

use kameo::actor::ActorRef;
use tokio::sync::oneshot;
use tracing::error;

use super::{
   PieceStorageStrategy, TorrentActor, TorrentSnapshot, TorrentState,
   commands::{
      GetState, ReadyHook, SetAutoStart, SetOutputPath, SetPieceManager, SetPieceStorage, SetState,
      SetSufficientPeers, SnapshotState,
   },
};
use crate::{
   errors::{TorrentError, map_torrent_send_error},
   frontend::{
      EventSubscription, Hub, HubInner, LivePublisher, PeerHandle, TorrentEventKind,
      TorrentListener, TorrentView, TrackerHandle,
   },
   hashes::InfoHash,
   pieces::PieceManager,
};

#[derive(Debug)]
pub(crate) struct TorrentInner {
   pub(crate) info_hash: InfoHash,
   pub(crate) actor: ActorRef<TorrentActor>,
   pub(crate) hub: Weak<HubInner>,
   pub(crate) live: Arc<LivePublisher<Option<TorrentView>, TorrentEventKind>>,
}

/// A handle to a torrent managed by the engine.
///
/// This struct acts as the primary interface for controlling, observing, and
/// configuring a torrent after it has been added to the
/// [`Engine`](crate::engine::Engine).
#[derive(Clone)]
pub struct Torrent {
   pub(crate) inner: Arc<TorrentInner>,
}

impl fmt::Debug for Torrent {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter
         .debug_struct("Torrent")
         .field("info_hash", &self.info_hash())
         .finish_non_exhaustive()
   }
}

impl Torrent {
   /// Creates a new [`Torrent`] handle from an [`InfoHash`] and a reference
   /// to its underlying [`TorrentActor`].
   #[cfg(test)]
   pub(crate) fn new(info_hash: InfoHash, actor_ref: ActorRef<TorrentActor>) -> Self {
      Self::new_with_frontend(info_hash, actor_ref, &Hub::default(), None)
   }

   pub(crate) fn new_with_frontend(
      info_hash: InfoHash, actor: ActorRef<TorrentActor>, frontend: &Hub,
      initial_view: Option<TorrentView>,
   ) -> Self {
      let scope = frontend.ensure_torrent_scope(info_hash);
      if let Some(view) = initial_view {
         let _ = scope.live.replace_view(Some(view));
      }
      let inner = Arc::new(TorrentInner {
         info_hash,
         actor,
         hub: frontend.downgrade(),
         live: Arc::clone(&scope.live),
      });
      Self { inner }
   }

   pub(crate) fn actor(&self) -> &ActorRef<TorrentActor> {
      &self.inner.actor
   }

   /// Returns the [`InfoHash`] that uniquely identifies this torrent.
   pub fn info_hash(&self) -> InfoHash {
      self.inner.info_hash
   }

   pub async fn set_piece_storage(
      &self, piece_storage: PieceStorageStrategy,
   ) -> Result<(), TorrentError> {
      self
         .actor()
         .ask(SetPieceStorage {
            strategy: piece_storage,
         })
         .await
         .map_err(|error| map_torrent_send_error("set piece storage", error))?;
      Ok(())
   }

   /// Sets the output folder used by the default file piece manager.
   pub async fn set_output_folder(&self, folder: impl Into<PathBuf>) -> Result<(), TorrentError> {
      self
         .actor()
         .ask(SetOutputPath {
            path: folder.into(),
         })
         .await
         .map_err(|error| map_torrent_send_error("set output folder", error))?;
      Ok(())
   }

   pub async fn set_piece_manager<'a>(
      &'a self, piece_manager: impl PieceManager + 'a + 'static,
   ) -> Result<(), TorrentError> {
      self
         .actor()
         .ask(SetPieceManager {
            manager: Box::new(piece_manager),
         })
         .await
         .map_err(|error| map_torrent_send_error("set piece manager", error))?;
      Ok(())
   }

   pub async fn start(&self) -> Result<(), TorrentError> {
      self.set_state(TorrentState::Downloading, "start").await
   }

   /// Resumes downloading or seeding this torrent.
   pub async fn resume(&self) -> Result<(), TorrentError> {
      self.start().await
   }

   /// Pauses this torrent while preserving its downloaded data and metadata.
   pub async fn pause(&self) -> Result<(), TorrentError> {
      self.set_state(TorrentState::Paused, "pause").await
   }

   /// Stops this torrent's active transfers.
   pub async fn stop(&self) -> Result<(), TorrentError> {
      self.set_state(TorrentState::Paused, "stop").await
   }

   async fn set_state(
      &self, state: TorrentState, operation: &'static str,
   ) -> Result<(), TorrentError> {
      self
         .actor()
         .ask(SetState { state })
         .await
         .inspect_err(|error| {
            error!(%error, operation, "Failed to change torrent state");
         })
         .map_err(|error| map_torrent_send_error(operation, error))?;
      Ok(())
   }

   pub async fn state(&self) -> Result<TorrentState, TorrentError> {
      self
         .actor()
         .ask(GetState)
         .await
         .map_err(|error| map_torrent_send_error("get state", error))
   }

   /// Captures this torrent's metadata, storage configuration, and verified or
   /// partial piece state in a Serde-compatible persistence snapshot.
   ///
   /// Use [`Self::listener`] for live frontend state.
   pub async fn snapshot(&self) -> Result<TorrentSnapshot, TorrentError> {
      self
         .actor()
         .ask(SnapshotState)
         .await
         .map(|snapshot| *snapshot)
         .map_err(|error| map_torrent_send_error("snapshot torrent", error))
   }

   pub async fn set_auto_start(&self, auto: bool) -> Result<(), TorrentError> {
      self
         .actor()
         .ask(SetAutoStart { auto })
         .await
         .map_err(|error| map_torrent_send_error("set auto start", error))?;
      Ok(())
   }

   pub async fn set_sufficient_peers(&self, peers: usize) -> Result<(), TorrentError> {
      self
         .actor()
         .ask(SetSufficientPeers { peers })
         .await
         .map_err(|error| map_torrent_send_error("set sufficient peers", error))?;
      Ok(())
   }

   pub async fn poll_ready(&self) -> Result<(), TorrentError> {
      let (hook, hook_rx) = oneshot::channel();
      self
         .actor()
         .ask(ReadyHook { hook })
         .await
         .map_err(|error| map_torrent_send_error("register ready hook", error))?;
      hook_rx
         .await
         .map_err(|error| TorrentError::ActorCommunicationFailed {
            operation: "wait for readiness",
            reason: error.to_string(),
         })?;
      Ok(())
   }

   /// Subscribes to live events for this torrent only.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription<TorrentEventKind> {
      self.inner.live.subscribe()
   }

   /// Creates a live listener scoped to this torrent.
   #[must_use]
   pub fn listener(&self) -> TorrentListener {
      self.inner.live.listener()
   }

   /// Returns the latest display-oriented state maintained for this torrent.
   ///
   /// This returns `None` after the torrent has been removed from its engine.
   #[must_use]
   pub fn view(&self) -> Option<TorrentView> {
      self.inner.live.view()
   }

   /// Returns handles for this torrent's currently connected peers.
   #[must_use]
   pub fn peers(&self) -> Vec<PeerHandle> {
      self
         .frontend()
         .map_or_else(Vec::new, |frontend| frontend.peer_handles(self.info_hash()))
   }

   /// Returns handles for this torrent's configured trackers.
   #[must_use]
   pub fn trackers(&self) -> Vec<TrackerHandle> {
      self.frontend().map_or_else(Vec::new, |frontend| {
         frontend.tracker_handles(self.info_hash())
      })
   }

   fn frontend(&self) -> Option<Hub> {
      self.inner.hub.upgrade().map(Hub::from_inner)
   }
}
