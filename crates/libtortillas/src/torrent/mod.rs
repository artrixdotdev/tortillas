use std::{collections::HashMap, fmt, sync::Arc};

use bitvec::vec::BitVec;
use bytes::Bytes;
use kameo::{
   Actor, Reply,
   actor::ActorRef,
   prelude::{Context, Message},
};
use librqbit_utp::UtpSocketUdp;
use sha1::{Digest, Sha1};
use tracing::{debug, error, info, instrument, warn};

use crate::{
   actor_request_response,
   errors::TorrentError,
   hashes::InfoHash,
   metainfo::{Info, MetaInfo},
   peer::{Peer, PeerId},
   protocol::{
      PeerActor,
      messages::{Handshake, PeerMessages},
      stream::{PeerSend, PeerStream},
   },
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
   actor_ref: ActorRef<Self>,
}

impl fmt::Display for Torrent {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      let working_trackers = self.trackers.values().filter(|t| t.is_alive()).count();
      let working_peers = self.peers.values().filter(|p| p.is_alive()).count();
      write!(
         f,
         "Torrent #{} w/ {working_trackers} Trackers & {} Peers",
         self.info_hash(),
         working_peers
      )
   }
}

impl Torrent {
   fn info_dict(&self) -> Option<&Info> {
      if let Some(info) = &self.info {
         Some(info)
      } else {
         match &self.metainfo {
            MetaInfo::Torrent(t) => Some(&t.info),
            _ => None,
         }
      }
   }

   fn info_hash(&self) -> InfoHash {
      if let Some(info) = &self.info_dict() {
         info.hash().expect("Failed to compute info hash")
      } else {
         match &self.metainfo {
            MetaInfo::Torrent(t) => t.info.hash().expect("Failed to compute info hash"),
            MetaInfo::MagnetUri(m) => m
               .info_hash()
               .expect("Magnet URIs should always have info hashes"),
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
         Some(mut stream) => {
            // POSSIBLE REFACTOR: make send_handshake... only send the handshake and not
            // expect a response back, currently it will send a handshake and forcibly
            // request, and verify one from the peer immediately after, which isn't what we
            // want in this situation because the peer has already handshaked us.
            let handshake = Handshake::new(Arc::new(self.info_hash()), self.id);
            if let Err(err) = stream.send(PeerMessages::Handshake(handshake)).await {
               error!("Failed to send handshake to peer: {}", err);
               return;
            }
            stream
         }
         None => {
            if let Ok((peer_id, stream, reserved)) = self.handshake_peer(peer).await {
               id = Some(peer_id);
               peer.reserved = reserved;
               peer.determine_supported().await;
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
      // No logging in this function as its calling functions with very verbose
      // logging
   }
}
/// For incoming from outside sources (e.g Peers, Trackers and Engine)
pub(crate) enum TorrentMessage<'a> {
   /// A message from an announce actor containing new Peers
   Announce(Vec<Peer>),

   /// Sent after an incoming peer initializes a handshake
   /// The handshake will be preverified and routed to this torrent instance.
   ///
   /// We as the instance are expected to reply to said handshake, this is not
   /// the responsibility of the engine.
   IncomingPeer(Peer, Box<PeerStream>),
   /// Bytes for the [Info] dict from an peer, these info bytes are expected to
   /// be verified by the torrent us before being used.
   InfoBytes(Bytes),

   KillPeer(&'a PeerId),
   KillTracker(&'a Tracker),
}

actor_request_response!(
   pub(crate) TorrentRequest,
   pub(crate) TorrentResponse #[derive(Reply)],

   /// Bitfield of the torrent
   Bitfield
   Bitfield(BitVec<u8>),
   /// Current peers of the torrent
   CurrentPeers
   CurrentPeers(Vec<&'static Peer>),
   /// Current trackers of the torrent
   CurrentTrackers
   CurrentTrackers(Vec<&'static Tracker>),
   /// Info hash of the torrent
   InfoHash
   InfoHash(InfoHash),
   /// Sends the current info dict if we have it
   HasInfoDict
   HasInfoDict(Option<Info>),
   /// Requests a piece from the torrent
   Request(usize, usize, usize)
   Request(usize, usize, Bytes),
);

impl Actor for Torrent {
   type Args = (PeerId, MetaInfo, Arc<UtpSocketUdp>, UdpServer);

   // FIXME: This should not be a TrackerError
   type Error = TorrentError;

   async fn on_start(args: Self::Args, us: ActorRef<Self>) -> Result<Self, Self::Error> {
      let (peer_id, metainfo, utp_server, tracker_server) = args;
      info!(
         info_hash = %metainfo.info_hash().unwrap(),
         "Starting new torrent instance",
      );

      // Create tracker actors
      let tracker_list = metainfo.announce_list();
      let mut trackers = HashMap::new();
      for tracker in tracker_list {
         let actor = TrackerActor::spawn(TrackerActor);
         trackers.insert(tracker, actor);
      }
      let info = match &metainfo {
         MetaInfo::Torrent(t) => Some(t.info.clone()),
         _ => None,
      };
      if info.is_none() {
         debug!("No info dict found in metainfo, you're probably using a magnet uri");
      }

      Ok(Self {
         peers: HashMap::new(),
         bitfield: BitVec::EMPTY,
         tracker_server,
         utp_server,
         trackers,
         id: peer_id,
         metainfo,
         info,
         actor_ref: us,
      })
   }
}

impl Message<TorrentMessage<'static>> for Torrent {
   type Reply = ();

   async fn handle(
      &mut self, message: TorrentMessage<'_>, _: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match message {
         TorrentMessage::Announce(peers) => {
            for mut peer in peers {
               self.append_peer(&mut peer, None).await;
            }
         }
         TorrentMessage::IncomingPeer(mut peer, stream) => {
            self.append_peer(&mut peer, Some(*stream)).await
         }
         TorrentMessage::InfoBytes(bytes) => {
            if self.info.is_some() {
               debug!(
                  dict = %String::from_utf8_lossy(&bytes),
                  "Received info dict when we already have one"
               );
               return;
            }
            let mut hasher = Sha1::new();

            hasher.update(&bytes);
            let hash = hex::encode(hasher.finalize());
            if hash == self.info_hash().to_hex() {
               let info = serde_bencode::from_bytes(&bytes).expect("Failed to parse info dict");
               self.info = info;
            } else {
               warn!(
                  dict = %String::from_utf8_lossy(&bytes),
                  "Received invalid info hash"
               );
            }
         }
         TorrentMessage::KillPeer(id) => {
            // Kill the actor quietly
            if let Some(actor) = self.peers.get(id) {
               actor.kill();
               self.peers.remove(id);
            } else {
               warn!("Received kill peer message for unknown peer");
            }
         }
         TorrentMessage::KillTracker(tracker) => {
            // Kill the actor quietly
            if let Some(actor) = self.trackers.get(tracker) {
               actor.kill();
               self.trackers.remove(tracker);
            } else {
               warn!("Received kill tracker message for unknown tracker");
            }
         }
         TorrentMessage::IncomingPiece(i, offset, bytes) => unimplemented!(),
      }
   }
}

impl Message<TorrentRequest> for Torrent {
   type Reply = TorrentResponse;

   // TODO: Figure out a way to send the peers back to the engine (if needed)
   async fn handle(
      &mut self, message: TorrentRequest, _: &mut Context<Self, Self::Reply>,
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

         TorrentRequest::HasInfoDict => TorrentResponse::HasInfoDict(self.info.clone()),
         TorrentRequest::Request(index, offset, length) => {
            unimplemented!()
         }
      }
   }
}
