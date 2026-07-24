use std::path::PathBuf;

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
   errors::TorrentError,
   frontend::{EventSubscription, FrontendPublisher, TorrentCommand, TorrentListener, TorrentView},
   hashes::InfoHash,
   pieces::PieceManager,
};

/// A handle to a torrent managed by the engine.
///
/// This struct acts as the primary interface for controlling and configuring
/// a torrent after it has been added to the [`Engine`](crate::engine::Engine).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Torrent {
   info_hash: InfoHash,
   actor: ActorRef<TorrentActor>,
   frontend: FrontendPublisher,
}

impl Torrent {
   /// Creates a new [`Torrent`] handle from an [`InfoHash`] and a reference
   /// to its underlying [`TorrentActor`].
   pub(crate) fn new(info_hash: InfoHash, actor_ref: ActorRef<TorrentActor>) -> Self {
      Self::new_with_frontend(info_hash, actor_ref, FrontendPublisher::default())
   }

   pub(crate) fn new_with_frontend(
      info_hash: InfoHash, actor: ActorRef<TorrentActor>, frontend: FrontendPublisher,
   ) -> Self {
      Self {
         info_hash,
         actor,
         frontend,
      }
   }

   pub(crate) fn actor(&self) -> &ActorRef<TorrentActor> {
      &self.actor
   }

   /// Returns the [`InfoHash`] that uniquely identifies this torrent.
   pub fn info_hash(&self) -> InfoHash {
      self.info_hash
   }

   /// Alias for [`Self::info_hash`].
   pub fn key(&self) -> InfoHash {
      self.info_hash()
   }

   pub async fn set_piece_storage(
      &self, piece_storage: PieceStorageStrategy,
   ) -> Result<(), TorrentError> {
      self
         .actor()
         .tell(SetPieceStorage {
            strategy: piece_storage,
         })
         .await
         .map_err(Self::communication_error)?;
      Ok(())
   }

   pub async fn with_output_folder(&self, folder: impl Into<PathBuf>) -> Result<(), TorrentError> {
      self
         .actor()
         .ask(SetOutputPath {
            path: folder.into(),
         })
         .await
         .map_err(Self::communication_error)?;
      Ok(())
   }

   pub async fn with_piece_manager<'a>(
      &'a self, piece_manager: impl PieceManager + 'a + 'static,
   ) -> Result<(), TorrentError> {
      self
         .actor()
         .tell(SetPieceManager {
            manager: Box::new(piece_manager),
         })
         .await
         .map_err(Self::communication_error)?;
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
      let msg = SetState { state };

      self
         .actor()
         .ask(msg)
         .await
         .inspect_err(|e| error!(error = %e, operation, "Failed to change torrent state"))
         .map_err(Self::communication_error)?;

      Ok(())
   }

   pub async fn state(&self) -> Result<TorrentState, TorrentError> {
      self
         .actor()
         .ask(GetState)
         .await
         .map_err(Self::communication_error)
   }

   /// Returns a stable, frontend-ready snapshot of this torrent.
   pub async fn export(&self) -> Result<TorrentSnapshot, TorrentError> {
      self.snapshot().await
   }

   pub async fn snapshot(&self) -> Result<TorrentSnapshot, TorrentError> {
      self
         .actor()
         .ask(SnapshotState)
         .await
         .map(|snapshot| *snapshot)
         .map_err(Self::communication_error)
   }

   pub async fn set_auto_start(&self, auto: bool) -> Result<(), TorrentError> {
      let msg = SetAutoStart { auto };
      self
         .actor()
         .tell(msg)
         .await
         .map_err(Self::communication_error)?;
      Ok(())
   }

   pub async fn set_sufficient_peers(&self, peers: usize) -> Result<(), TorrentError> {
      let msg = SetSufficientPeers { peers };
      self
         .actor()
         .tell(msg)
         .await
         .map_err(Self::communication_error)?;
      Ok(())
   }

   pub async fn poll_ready(&self) -> Result<(), TorrentError> {
      let (hook, hook_rx) = oneshot::channel();
      let msg = ReadyHook { hook };
      self
         .actor()
         .tell(msg)
         .await
         .map_err(Self::communication_error)?;
      hook_rx.await.map_err(Self::communication_error)?;

      Ok(())
   }

   /// Sends a typed frontend command directly to this torrent.
   pub async fn send(&self, command: TorrentCommand) -> Result<(), TorrentError> {
      match command {
         TorrentCommand::Start => self.start().await,
         TorrentCommand::Resume => self.resume().await,
         TorrentCommand::Pause => self.pause().await,
         TorrentCommand::Stop => self.stop().await,
         TorrentCommand::SetOutputPath(path) => self.with_output_folder(path).await,
         TorrentCommand::SetAutostart(enabled) => self.set_auto_start(enabled).await,
         TorrentCommand::SetSufficientPeers(peers) => self.set_sufficient_peers(peers).await,
      }
   }

   /// Subscribes to live events for this torrent only.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription {
      self.frontend.subscribe_torrent(self.info_hash)
   }

   /// Creates a live listener scoped to this torrent.
   #[must_use]
   pub fn listener(&self) -> TorrentListener {
      TorrentListener::new(self.frontend.clone(), self.info_hash)
   }

   /// Returns the latest display-oriented state maintained for this torrent.
   ///
   /// This returns `None` after the torrent has been removed from its engine.
   #[must_use]
   pub fn live_view(&self) -> Option<TorrentView> {
      self.frontend.torrent_view(self.info_hash)
   }

   fn communication_error(error: impl std::fmt::Display) -> TorrentError {
      TorrentError::ActorCommunicationFailed {
         actor_type: "torrent".to_string(),
         reason: error.to_string(),
      }
   }
}
