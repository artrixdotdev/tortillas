use std::{
   fmt,
   net::SocketAddr,
   path::PathBuf,
   sync::{Arc, atomic::AtomicU8},
};

use bitvec::vec::BitVec;
use dashmap::DashMap;
use kameo::{Actor, actor::ActorRef};
use librqbit_utp::UtpSocketUdp;
use tokio::task::JoinSet;
use tracing::{debug, error, info, instrument, warn};

use crate::{
   errors::TorrentError,
   hashes::InfoHash,
   metainfo::{Info, MetaInfo},
   peer::{Peer, PeerActor, PeerId, PeerTell},
   protocol::{
      messages::{Handshake, PeerMessages},
      stream::{PeerSend, PeerStream},
   },
   tracker::{Tracker, TrackerActor, udp::UdpServer},
};

/// Defines how torrent pieces are stored and accessed.
///
/// A torrent is composed of multiple pieces, and this enum determines
/// whether those pieces are referenced directly from the downloaded
/// files or written into a separate cache directory.
///
/// # Variants
///
/// - [`InFile`]: References pieces directly from the files that the torrent
///   describes. No extra storage is used; the piece data is read directly from
///   the final output files. This is the default strategy and is efficient when
///   you are downloading directly into the final file layout.
///
/// - [`Disk(PathBuf)`]: Stores each piece as a separate file in the specified
///   cache directory. The filename for each piece is its SHA‑1 hash. This
///   strategy is required if you are using a custom output stream, since pieces
///   need to be retreived later on for future seeding. It is also useful for:
///   - HTTP Streaming or when the file itself is never actually written to disk
///   - Supporting non-standard output backends
#[derive(Debug, Default, Clone)]
pub enum PieceStorageStrategy {
   /// Reference pieces directly from the downloaded files themselves.
   ///
   /// This avoids extra storage overhead and is the default strategy.
   #[default]
   InFile,
   /// Write each piece to disk separately in the given cache directory.
   ///
   /// Each piece is stored as a file named by its SHA‑1 hash.
   /// This strategy is **required** when using a custom [`OutputType`].
   Disk(PathBuf),
}

pub(crate) struct TorrentActor {
   pub(crate) peers: Arc<DashMap<PeerId, ActorRef<PeerActor>>>,
   pub(crate) trackers: Arc<DashMap<Tracker, ActorRef<TrackerActor>>>,

   pub(crate) bitfield: Arc<BitVec<AtomicU8>>,
   pub(super) id: PeerId,
   pub(super) info: Option<Info>,
   pub(super) metainfo: MetaInfo,
   #[allow(dead_code)]
   pub(super) tracker_server: UdpServer,
   /// Should only be used to create new connections
   pub(super) utp_server: Arc<UtpSocketUdp>,
   pub(super) actor_ref: ActorRef<Self>,
   #[allow(dead_code)]
   piece_storage: PieceStorageStrategy,
}

impl fmt::Display for TorrentActor {
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

impl TorrentActor {
   pub fn info_dict(&self) -> Option<&Info> {
      if let Some(info) = &self.info {
         Some(info)
      } else {
         match &self.metainfo {
            MetaInfo::Torrent(t) => Some(&t.info),
            _ => None,
         }
      }
   }

   pub fn info_hash(&self) -> InfoHash {
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

   /// Spawns a new [`PeerActor`] for the given [`Peer`] and adds it to the
   /// torrent's peer set.
   ///
   /// - If a [`PeerStream`] is provided, a handshake is sent immediately.
   /// - If no stream is provided, this function attempts to connect to the peer
   ///   and performs the handshake sequence inline.
   ///
   /// The peer is ignored if:
   /// - The handshake fails,
   /// - The peer ID matches our own, or
   /// - The peer already exists in the peer set.
   #[instrument(skip(self, peer, stream), fields(%self, peer = ?peer.socket_addr()))]
   pub(super) fn append_peer(&self, mut peer: Peer, stream: Option<PeerStream>) {
      let info_hash = Arc::new(self.info_hash());
      let actor_ref = self.actor_ref.clone();
      let our_id = self.id;
      let utp_server = self.utp_server.clone();
      let peers = self.peers.clone();

      tokio::spawn(async move {
         // Should pass the stream to PeerActor at some point
         let mut id = peer.id;
         let stream = match stream {
            Some(mut stream) => {
               let handshake = Handshake::new(info_hash, our_id);
               if let Err(err) = stream.send(PeerMessages::Handshake(handshake)).await {
                  error!("Failed to send handshake to peer: {}", err);
                  return;
               }
               stream
            }
            None => {
               let stream = PeerStream::connect(peer.socket_addr(), Some(utp_server)).await;
               match stream {
                  Ok(mut stream) => {
                     match stream.send_handshake(our_id, Arc::clone(&info_hash)).await {
                        Ok(_) => match stream.recv_handshake().await {
                           Ok((peer_id, reserved)) => {
                              id = Some(peer_id);
                              peer.reserved = reserved;
                              peer.determine_supported().await;
                              stream
                           }
                           Err(err) => {
                              warn!(error = %err, "Failed to receive handshake from peer; exiting");
                              return;
                           }
                        },
                        Err(err) => {
                           warn!(error = %err, "Failed to send handshake to peer; exiting");
                           return;
                        }
                     }
                  }
                  Err(err) => {
                     warn!(error = %err, "Failed to connect to peer; exiting");
                     return;
                  }
               }
            }
         };
         // Safe because we always know the id is defined by the lines above
         let id = id.unwrap();

         // Dont add ourselves as peers
         if id == our_id {
            return;
         }

         // #109
         if peers.contains_key(&id) {
            warn!("Peer already exists, ignoring");
            return;
         }

         peer.id = Some(id);

         let actor = PeerActor::spawn((peer.clone(), stream, actor_ref));
         // We cant store peers until #86 is implemented
         peers.insert(id, actor);
      });
   }

   /// Broadcasts a message to all peers concurrently.
   ///
   /// This function snapshots the current set of peer actor references before
   /// sending, which avoids holding the [`DashMap`] lock across `.await`
   /// points. This means other tasks can continue to access and modify the
   /// peer set while the broadcast is in progress.
   ///
   /// Each peer receives the message in parallel using a
   /// [`tokio::task::JoinSet`]. This prevents a slow or unresponsive peer
   /// from blocking delivery to others. However, this also means that
   /// broadcasting may use more memory, since all messages are cloned and
   /// dispatched at once.
   ///
   /// Any errors from individual peers are logged, but do not stop the
   /// broadcast from continuing to other peers.
   #[instrument(skip(self, tell), fields(msg = ?tell))]
   pub(super) async fn broadcast_to_peers(&self, tell: PeerTell) {
      // Snapshot actor refs to release DashMap locks before awaiting.
      // Might use more memory but it will increase performance
      let actor_refs: Vec<ActorRef<PeerActor>> = self
         .peers
         .iter()
         .map(|entry| entry.value().clone())
         .collect();

      // Fan out concurrently; each task awaits its own tell.
      let mut set = JoinSet::new();
      for actor in actor_refs {
         let msg = tell.clone();
         set.spawn(async move { actor.tell(msg).await });
      }
      while let Some(res) = set.join_next().await {
         match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!(error = %e, "Failed to send to peer"),
            Err(join_err) => warn!(error = %join_err, "broadcast_to_peers task panicked"),
         }
      }
   }
}

impl Actor for TorrentActor {
   type Args = (
      PeerId,
      MetaInfo,
      Arc<UtpSocketUdp>,
      UdpServer,
      Option<SocketAddr>,
      PieceStorageStrategy,
   );

   type Error = TorrentError;

   async fn on_start(args: Self::Args, us: ActorRef<Self>) -> Result<Self, Self::Error> {
      let (peer_id, metainfo, utp_server, tracker_server, primary_addr, piece_storage) = args;
      let primary_addr = primary_addr.unwrap_or_else(|| {
         let addr = utp_server.bind_addr();
         info!("No primary address provided, using {}", addr);
         addr
      });

      info!(
         info_hash = %metainfo.info_hash().unwrap(),
         "Starting new torrent instance",
      );

      // Create tracker actors
      let tracker_list = metainfo.announce_list();
      let trackers = DashMap::new();
      for tracker in tracker_list {
         let actor = TrackerActor::spawn((
            tracker.clone(),
            peer_id,
            tracker_server.clone(),
            primary_addr,
            us.clone(),
         ));
         trackers.insert(tracker, actor);
      }
      let info = match &metainfo {
         MetaInfo::Torrent(t) => Some(t.info.clone()),
         _ => None,
      };
      if info.is_none() {
         debug!("No info dict found in metainfo, you're probably using a magnet uri");
      }
      let bitfield: Arc<BitVec<AtomicU8>> = if let Some(info) = &info {
         debug!("Using bitfield length {}", info.piece_count());
         Arc::new(BitVec::repeat(false, info.piece_count()))
      } else {
         Arc::new(BitVec::EMPTY)
      };

      Ok(Self {
         peers: Arc::new(DashMap::new()),
         bitfield,
         tracker_server,
         utp_server,
         trackers: Arc::new(trackers),
         id: peer_id,
         metainfo,
         info,
         actor_ref: us,
         piece_storage,
      })
   }
}

#[cfg(test)]
mod tests {
   use std::{net::SocketAddr, str::FromStr, time::Duration};

   use librqbit_utp::UtpSocket;
   use tokio::time::sleep;
   use tracing::trace;

   use super::*;
   use crate::{
      metainfo::{MagnetUri, TorrentFile},
      torrent::{TorrentRequest, TorrentResponse},
   };

   #[tokio::test(flavor = "multi_thread")]
   async fn test_torrent_actor() {
      tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .init();
      let metainfo = TorrentFile::parse(include_bytes!(
         "../../tests/torrents/big-buck-bunny.torrent"
      ))
      .unwrap();

      let peer_id = PeerId::default();

      let udp_server = UdpServer::new(None).await;
      let utp_server =
         UtpSocket::new_udp(SocketAddr::from_str("0.0.0.0:0").expect("Failed to parse"))
            .await
            .unwrap();

      let actor = TorrentActor::spawn((
         peer_id,
         metainfo,
         utp_server,
         udp_server.clone(),
         None,
         PieceStorageStrategy::default(),
      ));

      // Blocking loop that runs until we successfully handshake with atleast 6 peers
      loop {
         let peers_count = match actor.ask(TorrentRequest::PeerCount).await.unwrap() {
            TorrentResponse::PeerCount(count) => count,
            _ => unreachable!(),
         };
         if peers_count > 6 {
            break;
         } else {
            info!(
               current_peers_count = peers_count,
               "Waiting for more peers...."
            )
         }
         sleep(Duration::from_millis(100)).await;
      }

      let peers_count = match actor.ask(TorrentRequest::PeerCount).await.unwrap() {
         TorrentResponse::PeerCount(count) => count,
         _ => unreachable!(),
      };

      actor.stop_gracefully().await.expect("Failed to stop");
      info!("Connected to {peers_count} peers!")
   }

   #[tokio::test(flavor = "multi_thread")]
   async fn test_info_dict_retrieval() {
      tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .init();

      // Test with a magnet URI, since magnet URIs don't come with an info dict
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MagnetUri::parse(contents).unwrap();

      let peer_id = PeerId::default();

      let udp_server = UdpServer::new(None).await;
      let utp_server =
         UtpSocket::new_udp(SocketAddr::from_str("0.0.0.0:0").expect("Failed to parse"))
            .await
            .unwrap();

      let actor = TorrentActor::spawn((
         peer_id,
         metainfo,
         utp_server,
         udp_server.clone(),
         None,
         PieceStorageStrategy::default(),
      ));

      // Blocking loop that runs until we get an info dict
      loop {
         match actor.ask(TorrentRequest::HasInfoDict).await.unwrap() {
            TorrentResponse::HasInfoDict(maybe_info_dict) => {
               if maybe_info_dict.is_some() {
                  trace!("Got info dict!");
                  break;
               }
            }
            _ => unreachable!(),
         };
         sleep(Duration::from_millis(100)).await;
      }

      actor.stop_gracefully().await.expect("Failed to stop");
   }
}
