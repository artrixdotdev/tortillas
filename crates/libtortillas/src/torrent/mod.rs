use std::{fmt, sync::Arc};

use bitvec::vec::BitVec;
use bytes::Bytes;
use dashmap::DashMap;
use kameo::{
   Actor, Reply,
   actor::ActorRef,
   prelude::{Context, Message},
};
use librqbit_utp::UtpSocketUdp;
use sha1::{Digest, Sha1};
use tracing::{debug, error, info, instrument, trace, warn};

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
   peers: Arc<DashMap<PeerId, ActorRef<PeerActor>>>,
   trackers: Arc<DashMap<Tracker, ActorRef<TrackerActor>>>,

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
      let working_trackers = self.trackers.len();
      let working_peers = self.peers.len();
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
   async fn append_peer(&self, mut peer: Peer, stream: Option<PeerStream>) {
      let info_hash = Arc::new(self.info_hash());
      let actor_ref = self.actor_ref.clone();
      let our_id = self.id;
      let utp_server = self.utp_server.clone();

      tokio::spawn(async move {
         // Should pass the stream to PeerActor at some point
         let mut id = peer.id;
         let stream = match stream {
            Some(mut stream) => {
               // POSSIBLE REFACTOR: make send_handshake... only send the handshake and not
               // expect a response back, currently it will send ahandshake and forcibly
               // request, and verify one from the peer immediately after, which isn't what
               // we want in this situation because the peer has already handshaked us.
               let handshake = Handshake::new(info_hash, our_id);
               if let Err(err) = stream.send(PeerMessages::Handshake(handshake)).await {
                  error!("Failed to send handshake to peer: {}", err);
                  return;
               }
               stream
            }
            None => {
               let mut stream = PeerStream::connect(peer.socket_addr(), Some(utp_server)).await;

               let handshake = stream.send_handshake(our_id, info_hash).await;
               if let Ok((peer_id, reserved)) = handshake {
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
         if id == our_id {
            return;
         }
         peer.id = Some(id);

         let actor = PeerActor::spawn((peer.clone(), stream, actor_ref));
         // We cant store peers until #86 is implemented
         // self.peers.insert(id, actor);
      });
   }
}
/// For incoming from outside sources (e.g Peers, Trackers and Engine)
pub(crate) enum TorrentMessage {
   /// A message from an announce actor containing new Peers
   Announce(Vec<Peer>),

   /// Sent after an incoming peer initializes a handshake
   /// The handshake will be preverified and routed to this torrent instance.
   ///
   /// We as the instance are expected to reply to said handshake, this is not
   /// the responsibility of the engine.
   IncomingPeer(Peer, Box<PeerStream>),

   /// Used to manually add a peer. This is primarily used for testing but can
   /// be used to initiate a peer connection without it having to come from an
   /// announce.
   AddPeer(Peer),
   /// Index, Offset, Data
   /// See the corresponding [peer message](PeerMessages::Piece)
   IncomingPiece(usize, usize, Bytes),
   /// Bytes for the [Info] dict from an peer, these info bytes are expected to
   /// be verified by the torrent us before being used.
   InfoBytes(Bytes),

   KillPeer(PeerId),
   KillTracker(Tracker),
}

impl fmt::Debug for TorrentMessage {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      match self {
         TorrentMessage::InfoBytes(bytes) => write!(f, "InfoBytes({:?})", bytes),
         TorrentMessage::KillPeer(peer_id) => write!(f, "KillPeer({:?})", peer_id),
         TorrentMessage::KillTracker(tracker) => write!(f, "KillTracker({:?})", tracker),
         TorrentMessage::AddPeer(peer) => write!(f, "AddPeer({:?})", peer),
         TorrentMessage::IncomingPiece(index, offset, data) => {
            write!(f, "IncomingPiece({}, {}, {:?})", index, offset, data)
         }
         TorrentMessage::Announce(peers) => write!(f, "Announce({:?})", peers),
         _ => write!(f, "TorrentMessage"), // Add more later,
      }
   }
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
      let trackers = DashMap::new();
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
         peers: Arc::new(DashMap::new()),
         bitfield: BitVec::EMPTY,
         tracker_server,
         utp_server,
         trackers: Arc::new(trackers),
         id: peer_id,
         metainfo,
         info,
         actor_ref: us,
      })
   }
}

impl Message<TorrentMessage> for Torrent {
   type Reply = ();

   async fn handle(
      &mut self, message: TorrentMessage, _: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      trace!(message = ?message, "Received message");
      match message {
         TorrentMessage::Announce(peers) => {
            for mut peer in peers {
               self.append_peer(peer, None).await;
            }
         }
         TorrentMessage::IncomingPeer(mut peer, stream) => {
            self.append_peer(peer, Some(*stream)).await
         }
         TorrentMessage::AddPeer(mut peer) => {
            self.append_peer(peer, None).await;
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
            if let Some(actor) = self.peers.get(&id) {
               actor.kill();
               self.peers.remove(&id);
            } else {
               warn!("Received kill peer message for unknown peer");
            }
         }
         TorrentMessage::KillTracker(tracker) => {
            // Kill the actor quietly
            if let Some(actor) = self.trackers.get(&tracker) {
               actor.kill();
               self.trackers.remove(&tracker);
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

#[cfg(test)]
mod tests {
   use std::{net::SocketAddr, str::FromStr, time::Duration};

   use futures::StreamExt;
   use librqbit_utp::UtpSocket;
   use rand::random_range;
   use tokio::time::{sleep, timeout};

   use super::*;

   #[tokio::test(flavor = "multi_thread", worker_threads = 200)]
   async fn test_torrent_actor() {
      tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .init();
      let metainfo =
         MetaInfo::new(include_str!("../../tests/magneturis/big-buck-bunny.txt").into())
            .await
            .unwrap();

      let port: u16 = random_range(1024..65535);

      let socket_addr = SocketAddr::from_str(&format!("0.0.0.0:{}", port)).unwrap();
      let peer_id = PeerId::default();

      let udp_server = UdpServer::new(None).await;
      let utp_server = UtpSocket::new_udp(socket_addr).await.unwrap();
      let announce_list = metainfo.announce_list();
      let announce_list = announce_list
         .iter()
         .filter(|t| !matches!(**t, Tracker::Websocket(..)));
      let mut peers = Vec::new();

      let info_hash = metainfo.info_hash().unwrap();
      let actor = Torrent::spawn((peer_id, metainfo, utp_server, udp_server.clone()));

      for tracker in announce_list {
         let instance = tracker
            .to_instance(info_hash, peer_id, port, udp_server.clone())
            .await;
         if let Err(err) = instance {
            error!("Failed to create tracker instance: {}", err);
            continue;
         }
         let instance = instance.unwrap();
         let comms = timeout(Duration::from_secs(3), instance.configure()).await;
         match comms {
            Ok(Err(_)) => continue,
            Err(_) => continue,
            _ => {}
         }

         let stream = timeout(Duration::from_secs(3), instance.announce_stream()).await;

         match stream {
            Ok(mut stream) => {
               let _ = timeout(Duration::from_secs(2), async {
                  while let Some(peer) = stream.next().await {
                     if peers.len() > 100 {
                        break;
                     }
                     peers.push(peer);
                  }
               })
               .await;
            }
            Err(_) => continue,
         }
      }

      actor.tell(TorrentMessage::Announce(peers)).await.unwrap();

      sleep(Duration::from_secs(30)).await;
   }
}
