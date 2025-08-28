mod actor;
mod messages;
use std::path::PathBuf;

pub use actor::*;
use bytes::Bytes;
use kameo::actor::ActorRef;
pub(crate) use messages::*;
use tokio::sync::mpsc;

use crate::hashes::InfoHash;

// Note: libtortillas should never clone this struct. The `Clone` derive exists
// only incase the end developer choose to clone thist
#[derive(Debug, Clone)]
pub struct StreamedPiece {
   pub name: String,
   pub index: usize,
   pub offset: usize,
   pub data: Bytes,
}

#[allow(dead_code)]
pub(super) enum OutputStrategy {
   Folder(PathBuf),
   Stream,
}

/// Should always be used through the [`Engine`](crate::engine::Engine)
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Torrent(InfoHash, ActorRef<TorrentActor>);

impl Torrent {
   pub(crate) fn new(info_hash: InfoHash, actor_ref: ActorRef<TorrentActor>) -> Self {
      Torrent(info_hash, actor_ref)
   }

   pub(crate) fn actor(&self) -> &ActorRef<TorrentActor> {
      &self.1
   }

   pub fn info_hash(&self) -> InfoHash {
      self.0
   }

   /// Alias for [Self::info_hash]
   pub fn key(&self) -> InfoHash {
      self.info_hash()
   }

   pub fn set_piece_storage(&self, piece_storage: PieceStorageStrategy) -> &Self {
      self
         .actor()
         .tell(TorrentMessage::PieceStorage(piece_storage))
         // Use blocking send here because we want to ensure the message is processed before
         // continuing.
         // Also because we don't want the end user to have to use async for this.
         .blocking_send()
         .expect("Failed to set piece storage");

      self
   }

   pub fn with_output_folder(&self, folder: impl Into<PathBuf>) {
      let strategy = OutputStrategy::Folder(folder.into());
      self
         .actor()
         .ask(TorrentRequest::OutputStrategy(strategy))
         .blocking_send()
         .expect("Failed to set output folder");
   }

   pub fn with_output_stream(&self) -> mpsc::Receiver<StreamedPiece> {
      let strategy = OutputStrategy::Stream;
      let res = self
         .actor()
         .ask(TorrentRequest::OutputStrategy(strategy))
         .blocking_send()
         .expect("Failed to send request for ");

      match res {
         TorrentResponse::OutputStrategy(Some(receiver)) => receiver,
         _ => unreachable!(),
      }
   }
}
