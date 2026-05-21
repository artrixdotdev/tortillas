use std::path::PathBuf;

use anyhow::Result;
use kameo::actor::ActorRef;
use tokio::sync::oneshot;
use tracing::error;

use super::{
   PieceManager, PieceStorageStrategy, TorrentActor, TorrentExport, TorrentMessage, TorrentRequest,
   TorrentResponse, TorrentState,
};
use crate::hashes::InfoHash;

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
         .tell(TorrentMessage::PieceStorage(piece_storage))
         .await?;
      Ok(())
   }

   pub async fn with_output_folder(&self, folder: impl Into<PathBuf>) -> Result<()> {
      self
         .actor()
         .ask(TorrentMessage::SetOutputPath(folder.into()))
         .await?;
      Ok(())
   }

   pub async fn with_piece_manager<'a>(
      &'a self, piece_manager: impl PieceManager + 'a + 'static,
   ) -> Result<()> {
      self
         .actor()
         .tell(TorrentMessage::PieceManager(Box::new(piece_manager)))
         .await?;
      Ok(())
   }

   pub async fn start(&self) -> Result<()> {
      let msg = TorrentMessage::SetState(TorrentState::Downloading);

      self
         .actor()
         .tell(msg)
         .await
         .inspect_err(|e| error!(error = %e, "Failed to start torrent"))?;

      Ok(())
   }

   pub async fn state(&self) -> Result<TorrentState> {
      let msg = TorrentRequest::GetState;

      match self.actor().ask(msg).await? {
         TorrentResponse::GetState(state) => Ok(state),
         _ => unreachable!(),
      }
   }

   pub async fn export(&self) -> Result<TorrentExport> {
      let msg = TorrentRequest::Export;

      match self.actor().ask(msg).await? {
         TorrentResponse::Export(export) => Ok(*export),
         _ => unreachable!(),
      }
   }

   pub async fn set_auto_start(&self, auto: bool) -> Result<()> {
      let msg = TorrentMessage::SetAutoStart(auto);
      self.actor().tell(msg).await?;
      Ok(())
   }

   pub async fn set_sufficient_peers(&self, peers: usize) -> Result<()> {
      let msg = TorrentMessage::SetSufficientPeers(peers);
      self.actor().tell(msg).await?;
      Ok(())
   }

   pub async fn poll_ready(&self) -> Result<()> {
      let (hook, hook_rx) = oneshot::channel();
      let msg = TorrentMessage::ReadyHook(hook);
      self.actor().tell(msg).await?;
      hook_rx.await?;

      Ok(())
   }
}
