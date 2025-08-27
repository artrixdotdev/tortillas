mod actor;
mod messages;
pub(crate) use actor::*;
use kameo::actor::ActorRef;
pub(crate) use messages::*;

use crate::hashes::InfoHash;

/// Should always be used through the [`Engine`](crate::engine::Engine)
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Torrent(InfoHash, ActorRef<TorrentActor>);

impl Torrent {
   pub(crate) fn new(info_hash: InfoHash, actor_ref: ActorRef<TorrentActor>) -> Self {
      Torrent(info_hash, actor_ref)
   }

   pub fn info_hash(&self) -> InfoHash {
      self.0
   }

   /// Alias for [Self::info_hash]
   pub fn key(&self) -> InfoHash {
      self.info_hash()
   }
}
