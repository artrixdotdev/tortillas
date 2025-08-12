#![allow(dead_code, unused_variables, unreachable_code)]
use std::{collections::HashMap, fmt, sync::Arc};

// REMOVE SOON
use bitvec::vec::BitVec;
use kameo::{
   Actor, Reply,
   actor::ActorRef,
   prelude::{Context, Message},
};
use librqbit_utp::UtpSocketUdp;
use tracing::{instrument, warn};

use crate::{
   errors::TrackerError,
   hashes::InfoHash,
   metainfo::{Info, MetaInfo},
   peer::{Peer, PeerId},
   protocol::{PeerActor, stream::PeerStream},
   tracker::{Tracker, TrackerActor, udp::UdpServer},
};

pub(crate) struct Torrent {
   peers: HashMap<PeerId, ActorRef<PeerActor>>,
   trackers: HashMap<Tracker, ActorRef<TrackerActor>>,

   bitfield: BitVec<u8>,
   id: PeerId,
   info: Option<Info>,
   metainfo: MetaInfo,
   tracker_server: UdpServer,
   /// Should only be used to create new connections
   utp_server: Arc<UtpSocketUdp>,
}

impl fmt::Display for Torrent {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      let working_trackers = self.trackers.values().filter(|t| t.is_alive()).count();
      let working_peers = self.peers.values().filter(|p| p.is_alive()).count();
      write!(
         f,
         "Torrent #{} w/ {working_trackers} Trackers & {} Peers",
         self.id, working_peers
      )
   }
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

   fn info_hash(&self) -> InfoHash {
      if let Some(info) = &self.info_dict() {
         info.hash().unwrap()
      } else {
         match &self.metainfo {
            MetaInfo::Torrent(t) => t.info.hash().unwrap(),
            MetaInfo::MagnetUri(m) => m.info_hash().unwrap(),
         }
      }
   }

   /// Utility function that will create the peer actor and add it to the
   /// torrent.
   ///
   /// This function calls [Self::handshake_peer] if no stream is provided to
   /// retrieve the peer id
   #[instrument(skip(self, peer, stream), fields(%self, peer = ?peer.socket_addr()))]
   async fn append_peer(&mut self, peer: &mut Peer, stream: Option<PeerStream>) {
      // Should pass the stream to PeerActor at some point
      let mut id = peer.id;
      let stream = match stream {
         Some(stream) => stream,
         None => {
            if let Ok((peer_id, stream, reserved)) = self.handshake_peer(&peer).await {
               id = Some(peer_id);
               peer.reserved = reserved;
               stream
            } else {
               warn!("Failed to handshake with peer... silently exiting");
               return;
            }
         }
      };
      // Safe because we always know the id is defined by the lines above
      let id = id.unwrap();

      // Dont add ourselves as peers
      if id == self.id {
         return;
      }

      let actor = PeerActor::spawn(PeerActor);
      self.peers.insert(id, actor);
   }

   async fn handshake_peer(&self, peer: &Peer) -> anyhow::Result<(PeerId, PeerStream, [u8; 8])> {
      let mut stream = PeerStream::connect(peer.socket_addr(), Some(self.utp_server.clone())).await;

      let (id, reserved) = stream
         .send_handshake(self.id, Arc::new(self.info_hash()))
         .await?;

      Ok((id, stream, reserved))
   }
}

pub(crate) enum TorrentMessage {
   /// A message from an announce actor containing new Peers
   Announce(Vec<Peer>),

   /// Sent after an incoming peer initializes a handshake
   /// The handshake will be preverified and routed to this torrent instance.
   ///
   /// We as the instance are expected to reply to said handshake, this is not
   /// the responsibility of the engine.
   IncomingPeer(Peer, PeerStream),
}

pub(crate) enum TorrentRequest {
   Bitfield,
   CurrentPeers,
   CurrentTrackers,
   InfoHash,
}
#[derive(Reply)]
pub(crate) enum TorrentResponse {
   Bitfield(BitVec<u8>),
   CurrentPeers(Vec<&'static Peer>),
   CurrentTrackers(Vec<&'static Tracker>),
   InfoHash(InfoHash),
}

impl Actor for Torrent {
   type Args = (PeerId, MetaInfo, Arc<UtpSocketUdp>);

   // FIXME: This should not be a TrackerError
   type Error = TrackerError;

   async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
      let (peer_id, metainfo, utp_server) = args;

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
         id: peer_id,
         metainfo,
         info,
      })
   }
}

impl Message<TorrentMessage> for Torrent {
   type Reply = Option<()>;

   async fn handle(
      &mut self, message: TorrentMessage, ctx: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match message {
         TorrentMessage::Announce(peers) => {
            for mut peer in peers {
               self.append_peer(&mut peer, None).await;
            }
         }
         TorrentMessage::IncomingPeer(mut peer, stream) => {
            self.append_peer(&mut peer, Some(stream)).await
         }
      }
      None
   }
}

impl Message<TorrentRequest> for Torrent {
   type Reply = TorrentResponse;

   // TODO: Figure out a way to send the peers back to the engine (if needed)
   async fn handle(
      &mut self, message: TorrentRequest, ctx: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match message {
         TorrentRequest::Bitfield => TorrentResponse::Bitfield(self.bitfield.clone()),
         TorrentRequest::CurrentPeers => {
            unimplemented!()
            // TorrentResponse::CurrentPeers(self.peers.values().map(|peer|
            // peer).collect());
         }
         TorrentRequest::CurrentTrackers => {
            unimplemented!()
            // TorrentResponse::CurrentTrackers(self.trackers.keys().collect())
         }
         TorrentRequest::InfoHash => TorrentResponse::InfoHash(self.info_hash()),
      }
   }
}
