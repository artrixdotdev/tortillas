#![allow(dead_code, unused_variables, unreachable_code)]
use std::{collections::HashMap, sync::Arc};

// REMOVE SOON
use bitvec::vec::BitVec;
use kameo::{Actor, actor::ActorRef};
use librqbit_utp::UtpSocketUdp;

use crate::{
   errors::TrackerError,
   metainfo::{Info, MetaInfo},
   peer::{Peer, PeerId},
   protocol::PeerActor,
   tracker::{Tracker, TrackerActor, udp::UdpServer},
};

pub(crate) struct Torrent {
   peers: HashMap<PeerId, ActorRef<PeerActor>>,
   trackers: HashMap<Tracker, ActorRef<TrackerActor>>,

   bitfield: BitVec<u8>,
   peer_id: PeerId,
   info: Option<Info>,
   metainfo: MetaInfo,
   tracker_server: UdpServer,
   /// Should only be used to create new connections
   utp_server: Arc<UtpSocketUdp>,
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

pub(crate) enum TorrentMessage {
   /// A message from an announce containing new Peers
   Announce(Vec<Peer>),

   /// Sent after an incoming peer initializes a handshake
   /// The handshake will be preverified and routed to this torrent instance.
   ///
   /// We as the instance are expected to reply, this is not the responsibility
   /// of the engine.
   IncomingPeer(Peer),
}

impl Actor for Torrent {
   type Args = (MetaInfo, Arc<UtpSocketUdp>);

   // FIXME: This should not be a TrackerError
   type Error = TrackerError;

   async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
      let (metainfo, utp_server) = args;

      let peer_id = PeerId::new();
      // Create tracker actors
      let tracker_list = metainfo.announce_list();
      let mut trackers = HashMap::new();
      // Should be used later on for spawning the tracker actor
      let tracker_server = UdpServer::new(None).await;

      for tracker in tracker_list {
         trackers.insert(tracker, TrackerActor::spawn(TrackerActor));
      }
      let info = match &metainfo {
         MetaInfo::Torrent(t) => Some(t.info.clone()),
         _ => None,
      };

      Ok(Self {
         peers: HashMap::new(),
         bitfield: BitVec::EMPTY,
         tracker_server,
         utp_server,
         trackers,
         peer_id,
         metainfo,
         info,
      })
   }
}
