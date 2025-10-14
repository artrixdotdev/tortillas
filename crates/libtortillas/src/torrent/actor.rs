use std::{
   fmt::{self, Display},
   net::SocketAddr,
   path::PathBuf,
   sync::{Arc, atomic::AtomicU8},
   time::Instant,
};

use anyhow::ensure;
use async_trait::async_trait;
use bitvec::vec::BitVec;
use bytes::Bytes;
use dashmap::DashMap;
use kameo::{Actor, actor::ActorRef, mailbox};
use librqbit_utp::UtpSocketUdp;
use tokio::sync::oneshot;
use tracing::{debug, error, info, instrument, trace, warn};

use super::util;
use crate::{
   errors::TorrentError,
   hashes::InfoHash,
   metainfo::{Info, MetaInfo},
   peer::{Peer, PeerActor, PeerId, PeerTell},
   protocol::{
      messages::{Handshake, PeerMessages},
      stream::{PeerSend, PeerStream},
   },
   torrent::piece_manager::{FilePieceManager, PieceManager},
   tracker::{Tracker, TrackerActor, udp::UdpServer},
};
pub const BLOCK_SIZE: usize = 16 * 1024;

/// Defines how torrent pieces are stored and accessed.
///
/// A torrent is composed of multiple pieces, and this enum determines
/// whether those pieces are referenced directly from the downloaded
/// files or written into a separate cache directory.
///
/// # Variants
///
/// - [`Self::InFile`]: References pieces directly from the files that the
///   torrent describes. No extra storage is used; the piece data is read
///   directly from the final output files. This is the default strategy and is
///   efficient when you are downloading directly into the final file layout.
///
/// - [`Self::Disk`]: Stores each piece as a separate file in the specified
///   cache directory. The filename for each piece is its SHA‑1 hash. This
///   strategy is required if you are using a custom output stream, since pieces
///   need to be retrieved later on for future seeding. It is also useful for:
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
   /// This strategy is **required** when using a custom piece receiver.
   ///
   /// The path is not automatically set, and libtortillas will not function
   /// properly without the path being set.
   Disk(PathBuf),
}

/// The current state of the torrent, defaults to
/// [`Inactive`](TorrentState::Inactive)
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TorrentState {
   /// Torrent is downloading new pieces actively
   ///
   /// > Note: Even when in this state, we still seed the pieces that we *do*
   /// > have.
   Downloading,
   /// Torrent is seeding and has already completed the file
   Seeding,
   /// Torrent is paused or currently inactive, no seeding or piece downloading
   /// is happening.
   #[default]
   Inactive,
}

/// A hook that is called when the torrent is ready to start downloading.
/// This is used to implement [`Torrent::poll_ready`].
pub(super) type ReadyHook = oneshot::Sender<()>;

pub(super) enum PieceManagerProxy {
   Custom(Box<dyn PieceManager>),
   Default(FilePieceManager),
}

impl Display for PieceManagerProxy {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      match self {
         Self::Custom(_) => write!(f, "Custom Piece Manager"),
         Self::Default(_) => write!(f, "Default Piece Manager"),
      }
   }
}

#[allow(dead_code)]
impl PieceManagerProxy {
   pub fn is_custom(&self) -> bool {
      matches!(self, Self::Custom(_))
   }
}

#[async_trait]
impl PieceManager for PieceManagerProxy {
   fn info(&self) -> Option<&Info> {
      match self {
         Self::Custom(manager) => manager.info(),
         Self::Default(manager) => manager.info(),
      }
   }
   async fn pre_start(&mut self, info: Info) -> anyhow::Result<()> {
      match self {
         Self::Custom(manager) => manager.pre_start(info).await,
         Self::Default(manager) => manager.pre_start(info).await,
      }
   }
   async fn recv(&self, index: usize, data: Bytes) -> anyhow::Result<()> {
      match self {
         Self::Custom(manager) => manager.recv(index, data).await,
         Self::Default(manager) => manager.recv(index, data).await,
      }
   }
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
   pub(super) piece_storage: PieceStorageStrategy,
   pub(super) piece_manager: PieceManagerProxy,
   pub state: TorrentState,
   pub next_piece: usize,
   /// Map of piece indices to block indices. These will be used to track which
   /// blocks we have for each piece. Each entry is deleted when the piece is
   /// completed.
   pub(super) block_map: Arc<DashMap<usize, BitVec<usize>>>,

   pub(super) start_time: Option<Instant>,
   /// The number of peers we need to have before we start downloading, defaults
   /// to 6.
   pub(super) sufficient_peers: usize,

   pub(super) autostart: bool,

   /// If there is already a pending start, we don't want to start a new one
   pub(super) pending_start: bool,

   pub(super) ready_hook: Option<ReadyHook>,
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
   /// Checks if the torrent is empty (we haven't downloaded any pieces yet) by
   /// checking if our bitfield is filled with zeros.
   ///
   /// The reason why we can't just use `self.bitfield.is_empty()` is because a
   /// bitfield filled with zeros isn't considered "empty" since it still has
   /// data in it
   pub fn is_empty(&self) -> bool {
      self.bitfield.count_zeros() == self.bitfield.len()
   }

   /// Checks if the torrent is ready to autostart (via [`Self::autostart`]) and
   /// torrenting process
   #[instrument(skip(self), fields(torrent_id = %self.info_hash()))]
   pub async fn autostart(&mut self) {
      self.pending_start = true;

      let is_ready = self.is_ready_to_start();
      if is_ready {
         if self.autostart {
            trace!("Autostarting torrent");
            self.start().await;
         } else {
            // Torrent is ready, but auto-start is disabled
            if let Some(err) = self.ready_hook.take().and_then(|hook| hook.send(()).err()) {
               error!(?err, "Failed to send ready hook");
            }
         }
      }

      self.pending_start = false;
   }

   pub async fn start(&mut self) {
      if self.is_full() {
         self.state = TorrentState::Seeding;
         info!(id = %self.info_hash(), "Torrent is now seeding");
      } else {
         self.state = TorrentState::Downloading;
         info!(id = %self.info_hash(), "Torrent is now downloading");

         trace!(id = %self.info_hash(), peer_count = self.peers.len(), "Requesting first piece from peers");

         self.next_piece = self.bitfield.first_zero().unwrap_or_default();
         // Request first piece from peers
         self
            .broadcast_to_peers(PeerTell::NeedPiece(self.next_piece, 0, BLOCK_SIZE))
            .await;
         self.start_time = Some(Instant::now());
      }
      if let Some(err) = self.ready_hook.take().and_then(|hook| hook.send(()).err()) {
         error!(?err, "Failed to send ready hook");
      }
      let Some(info) = self.info.as_ref() else {
         warn!(id = %self.info_hash(), "Start requested before info dict is available; deferring");
         return;
      };
      self
         .piece_manager
         // Probably not the best to clone here, but should be fine for now
         .pre_start(info.clone())
         .await
         .expect("Failed to pre-start piece manager");

      info!(
         torrent_id = %self.info_hash(),
         piece_manager = %self.piece_manager,
         storage_strategy = ?self.piece_storage,
         peer_count = self.peers.len(),
         tracker_count = self.trackers.len(),
         total_pieces = info.piece_count(),
         peer_id = %self.id,
         state = ?self.state,
         "Started torrenting process"
      );
   }

   /// Checks if the torrent has all of the pieces (we've downloaded/have
   /// started with the entire file) by checking if our bitfield is filled with
   /// zeroes.
   pub fn is_full(&self) -> bool {
      self.bitfield.count_ones() == self.bitfield.len()
   }

   pub fn is_ready(&self) -> bool {
      self.info.is_some() && self.peers.len() >= self.sufficient_peers
   }

   pub fn is_ready_to_start(&self) -> bool {
      self.is_ready() && self.state == TorrentState::Inactive
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
   #[instrument(skip(self, peer, stream), fields(%self, peer_addr = ?peer.socket_addr(), torrent_id = %self.info_hash()))]
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
                  trace!(error = %err, peer_addr = %peer.socket_addr(), "Failed to send handshake to peer");
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
                              trace!(
                                 error = %err,
                                 peer_addr = %peer.socket_addr(),
                                 "Failed to receive handshake from peer; exiting"
                              );
                              return;
                           }
                        },
                        Err(err) => {
                           trace!(
                              error = %err,
                              peer_addr = %peer.socket_addr(),
                              "Failed to send handshake to peer; exiting"
                           );
                           return;
                        }
                     }
                  }
                  Err(err) => {
                     trace!(error = %err, "Failed to connect to peer; exiting");
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

         peer.id = Some(id);

         // Prevents a TOCTOU bug. Checks if peer id is in dashmap. The closure
         // atomically prevents the `PeerActor` from being created unless there
         // is no entry for the peer id (?)
         peers.entry(id).or_insert_with(|| {
            PeerActor::spawn_with_mailbox((peer.clone(), stream, actor_ref), mailbox::bounded(120))
         });
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
   #[instrument(skip(self, tell), fields(torrent_id = %self.info_hash(), msg = ?tell))]
   pub(super) async fn broadcast_to_peers(&self, tell: PeerTell) {
      // Snapshot actor refs to release DashMap locks before awaiting.
      let peers = self.peers.clone(); // assuming Arc<DashMap<..>>

      let actor_refs: Vec<(PeerId, ActorRef<PeerActor>)> = peers
         .iter()
         .map(|entry| (*entry.key(), entry.value().clone()))
         .collect();

      for (id, actor) in actor_refs {
         let msg = tell.clone();
         let peers = peers.clone();

         tokio::spawn(async move {
            if actor.is_alive() {
               if let Err(e) = actor.tell(msg).await {
                  warn!(error = %e, peer_id = %id, "Failed to send to peer");
               }
            } else {
               trace!(peer_id = %id, "Peer actor is dead, removing from peers set");
               peers.remove(&id);
            }
         });
      }
      // Returns immediately, without waiting for any peer responses
   }
   /// Gets the path to a piece file based on the index. Only should be used
   /// when the piece storage strategy is [`Disk`](PieceStorageStrategy::Disk),
   /// this function will panic otherwise.
   pub(super) fn get_piece_path(&self, index: usize) -> anyhow::Result<PathBuf> {
      let info_dict = self.info_dict().ok_or(TorrentError::MissingInfoDict)?;
      ensure!(info_dict.pieces.len() > index, "Index out of bounds");

      let hash = info_dict.pieces[index];

      // Panic because this is a user error, this function should never be called if
      // the storage strategy is not Disk
      assert!(
         matches!(self.piece_storage, PieceStorageStrategy::Disk(_)),
         "Piece storage strategy is not Disk"
      );

      if let PieceStorageStrategy::Disk(path) = &self.piece_storage {
         let mut path = path.clone();
         path.push(format!("{hash}.piece"));
         Ok(path.to_path_buf())
      } else {
         unreachable!()
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
      Option<bool>,
      Option<usize>,
      Option<PathBuf>,
   );

   type Error = TorrentError;

   async fn on_start(args: Self::Args, us: ActorRef<Self>) -> Result<Self, Self::Error> {
      let (
         peer_id,
         metainfo,
         utp_server,
         tracker_server,
         primary_addr,
         piece_storage,
         autostart,
         sufficient_peers,
         base_path,
      ) = args;
      let primary_addr = primary_addr.unwrap_or_else(|| {
         let addr = utp_server.bind_addr();
         debug!(torrent_id = %metainfo.info_hash().unwrap(), %addr, "No primary address provided, using default");
         addr
      });
      if let PieceStorageStrategy::Disk(dir) = &piece_storage {
         util::create_dir(dir).await?;
      }

      info!(
         torrent_id = %metainfo.info_hash().unwrap(),
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
         debug!(torrent_id = %metainfo.info_hash().unwrap(), "No info dict found in metainfo, you're probably using a magnet uri");
      }
      let bitfield: Arc<BitVec<AtomicU8>> = if let Some(info) = &info {
         debug!(torrent_id = %metainfo.info_hash().unwrap(), "Using bitfield length {}", info.piece_count());
         Arc::new(BitVec::repeat(false, info.piece_count()))
      } else {
         Arc::new(BitVec::EMPTY)
      };
      let default_manager = FilePieceManager(base_path, info.clone());

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
         state: TorrentState::default(),
         next_piece: 0,
         block_map: Arc::new(DashMap::new()),
         start_time: None,
         sufficient_peers: sufficient_peers.unwrap_or(6),
         autostart: autostart.unwrap_or(true),
         pending_start: false,
         ready_hook: None,
         piece_manager: PieceManagerProxy::Default(default_manager),
      })
   }

   async fn next(
      &mut self, _: kameo::prelude::WeakActorRef<Self>,
      mailbox_rx: &mut kameo::prelude::MailboxReceiver<Self>,
   ) -> Option<mailbox::Signal<Self>> {
      if !self.pending_start {
         self.autostart().await;
      }
      mailbox_rx.recv().await
   }
}

#[cfg(test)]
mod tests {
   use std::{net::SocketAddr, str::FromStr, time::Duration};

   use librqbit_utp::UtpSocket;
   use tokio::{fs, time::sleep};
   use tracing::trace;

   use super::*;
   use crate::{
      metainfo::{MagnetUri, TorrentFile},
      torrent::{Torrent, TorrentRequest, TorrentResponse},
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
      let sufficient_peers = 6;

      let info_hash = metainfo.clone().info_hash().unwrap();

      let actor = TorrentActor::spawn((
         peer_id,
         metainfo,
         utp_server,
         udp_server.clone(),
         None,
         PieceStorageStrategy::default(),
         Some(false), // We don't need to autostart because we're only checking if we have peers
         Some(sufficient_peers),
         None,
      ));

      let torrent = Torrent::new(info_hash, actor.clone());

      assert!(torrent.poll_ready().await.is_ok());

      actor.stop_gracefully().await.expect("Failed to stop");
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
         Some(false),
         None,
         None,
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

   #[tokio::test(flavor = "multi_thread")]
   async fn test_torrent_actor_piece_storage() {
      tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .init();
      let metainfo = TorrentFile::parse(include_bytes!(
         "../../tests/torrents/big-buck-bunny.torrent"
      ))
      .unwrap();
      let info_dict = match &metainfo {
         MetaInfo::Torrent(file) => file.info.clone(),
         _ => unreachable!(),
      };
      let info_hash = info_dict.hash().unwrap();

      // Clears piece files
      async fn clear_piece_files(piece_path: &PathBuf) {
         let mut entries = fs::read_dir(&piece_path).await.unwrap();
         while let Some(entry) = entries.next_entry().await.unwrap() {
            let path = entry.path();
            if let Some(ext) = path.extension()
               && ext == "piece"
            {
               fs::remove_file(&path).await.unwrap();
            }
         }
      }

      let piece_path = std::env::temp_dir().join("tortillas");
      let file_path = piece_path.join("files");

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
         PieceStorageStrategy::Disk(piece_path.clone()),
         None,
         None,
         Some(file_path),
      ));

      let torrent = Torrent::new(info_hash, actor.clone());

      assert!(torrent.poll_ready().await.is_ok());

      loop {
         let mut entries = fs::read_dir(&piece_path).await.unwrap();
         let mut found_piece = false;
         while let Some(entry) = entries.next_entry().await.unwrap() {
            let path = entry.path();
            if let Some(ext) = path.extension()
               && ext == "piece"
            {
               let metadata = entry.metadata().await.unwrap();
               if metadata.len() == info_dict.piece_length {
                  found_piece = true;
                  break;
               }
            }
         }
         if found_piece {
            break;
         }

         sleep(Duration::from_millis(200)).await;
      }

      actor.stop_gracefully().await.unwrap();
      clear_piece_files(&piece_path).await;
   }
}
