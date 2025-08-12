#![allow(dead_code, unused_variables, unreachable_code)] // REMOVE SOON
use bitvec::vec::BitVec;
use kameo::{Actor, actor::ActorRef};
use kameo_actors::pool::ActorPool;

use crate::{
   errors::TrackerError,
   metainfo::{Info, MetaInfo},
   peer::PeerId,
   protocol::PeerActor,
   tracker::TrackerActor,
};

pub(crate) struct Torrent {
   peers: ActorPool<PeerActor>,
   trackers: ActorPool<TrackerActor>,
   bitfield: BitVec<u8>,
   peer_id: PeerId,
   info: Option<Info>,
   metainfo: MetaInfo,
}

pub(crate) struct TorrentState {
   metainfo: MetaInfo,
}

impl Torrent {
   fn info_dict(&self) -> Option<Info> {
      if let Some(info) = &self.info {
         Some(info.clone())
      } else {
         match &self.metainfo {
            MetaInfo::Torrent(t) => Some(t.info.clone()),
            _ => None,
         }
      }
   }
}

impl Actor for Torrent {
   type Args = TorrentState;

   // FIXME: This should not be a TrackerError
   type Error = TrackerError;

   async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
      let TorrentState { metainfo } = args;

      // TODO
      // Initialize our peer id
      let peer_id = PeerId::new();
      // Create tracker actors
      // Place tracker actors into ActorPool
      // Request initial peers
      // Create peer actors
      // Place peer actors into PeerPool

      Ok(Self {
         peers: unimplemented!(),
         trackers: unimplemented!(),
         bitfield: BitVec::EMPTY,
         peer_id,
         metainfo,
         info: None,
      })
   }
}
