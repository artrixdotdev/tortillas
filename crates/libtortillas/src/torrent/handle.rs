use std::path::PathBuf;

use anyhow::Result;
use kameo::actor::ActorRef;
use tokio::sync::oneshot;
use tracing::error;

use super::{
   PieceStorageStrategy, TorrentActor, TorrentExport, TorrentState,
   commands::{
      ExportState, GetState, ReadyHook, SetAutoStart, SetOutputPath, SetPieceManager,
      SetPieceStorage, SetState, SetSufficientPeers,
   },
};
use crate::{hashes::InfoHash, pieces::PieceManager};

/// A handle to a torrent managed by the engine.
///
/// This struct acts as the primary interface for controlling and configuring
/// a torrent after it has been added to the [`Engine`](crate::engine::Engine).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Torrent(InfoHash, ActorRef<TorrentActor>);

impl Torrent {
   /// Creates a new [`Torrent`] handle from an [`InfoHash`] and a reference
   /// to its underlying [`TorrentActor`].
   pub(crate) fn new(info_hash: InfoHash, actor_ref: ActorRef<TorrentActor>) -> Self {
      Torrent(info_hash, actor_ref)
   }

   pub(crate) fn actor(&self) -> &ActorRef<TorrentActor> {
      &self.1
   }

   /// Returns the [`InfoHash`] that uniquely identifies this torrent.
   pub fn info_hash(&self) -> InfoHash {
      self.0
   }

   /// Alias for [`Self::info_hash`].
   pub fn key(&self) -> InfoHash {
      self.info_hash()
   }

   pub async fn set_piece_storage(&self, piece_storage: PieceStorageStrategy) -> Result<()> {
      self
         .actor()
         .tell(SetPieceStorage {
            strategy: piece_storage,
         })
         .await?;
      Ok(())
   }

   pub async fn with_output_folder(&self, folder: impl Into<PathBuf>) -> Result<()> {
      self
         .actor()
         .ask(SetOutputPath {
            path: folder.into(),
         })
         .await?;
      Ok(())
   }

   pub async fn with_piece_manager<'a>(
      &'a self, piece_manager: impl PieceManager + 'a + 'static,
   ) -> Result<()> {
      self
         .actor()
         .tell(SetPieceManager {
            manager: Box::new(piece_manager),
         })
         .await?;
      Ok(())
   }

   pub async fn start(&self) -> Result<()> {
      self.set_state(TorrentState::Downloading, "start").await
   }

   /// Resumes downloading or seeding this torrent.
   pub async fn resume(&self) -> Result<()> {
      self.start().await
   }

   /// Pauses this torrent while preserving its downloaded data and metadata.
   pub async fn pause(&self) -> Result<()> {
      self.set_state(TorrentState::Inactive, "pause").await
   }

   /// Stops this torrent's active transfers.
   pub async fn stop(&self) -> Result<()> {
      self.set_state(TorrentState::Inactive, "stop").await
   }

   async fn set_state(&self, state: TorrentState, operation: &'static str) -> Result<()> {
      let msg = SetState { state };

      self
         .actor()
         .ask(msg)
         .await
         .inspect_err(|e| error!(error = %e, operation, "Failed to change torrent state"))?;

      Ok(())
   }

   pub async fn state(&self) -> Result<TorrentState> {
      Ok(self.actor().ask(GetState).await?)
   }

   pub async fn export(&self) -> Result<TorrentExport> {
      Ok(*self.actor().ask(ExportState).await?)
   }

   pub async fn set_auto_start(&self, auto: bool) -> Result<()> {
      let msg = SetAutoStart { auto };
      self.actor().tell(msg).await?;
      Ok(())
   }

   pub async fn set_sufficient_peers(&self, peers: usize) -> Result<()> {
      let msg = SetSufficientPeers { peers };
      self.actor().tell(msg).await?;
      Ok(())
   }

   pub async fn poll_ready(&self) -> Result<()> {
      let (hook, hook_rx) = oneshot::channel();
      let msg = ReadyHook { hook };
      self.actor().tell(msg).await?;
      hook_rx.await?;

      Ok(())
   }
}
