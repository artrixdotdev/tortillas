use std::{
   collections::HashMap,
   fmt::{self, Display},
   net::SocketAddr,
   path::PathBuf,
   sync::{Arc, atomic::AtomicU8},
   time::Instant,
};

use async_trait::async_trait;
use bitvec::vec::BitVec;
use bytes::Bytes;
use kameo::{
   Actor,
   actor::{ActorRef, Spawn, WeakActorRef},
   error::ActorStopReason,
   mailbox::{MailboxReceiver, Signal},
   supervision::{RestartPolicy, SupervisionStrategy},
};
use kameo_actors::scheduler::Scheduler;
use librqbit_utp::UtpSocketUdp;
use tokio::sync::oneshot;
use tracing::{debug, error, info, instrument, trace, warn};

use super::util;
use crate::{
   errors::TorrentError,
   hashes::InfoHash,
   metainfo::{Info, MetaInfo},
   peer::{PeerActor, PeerId},
   pieces::{FilePieceManager, PieceManager, PieceScheduler, PieceStoreActor},
   torrent::{BLOCK_SIZE, PieceStorageStrategy, TorrentExport, TorrentState},
   tracker::{Event, Tracker, TrackerActor, TrackerMessage, TrackerUpdate, udp::UdpServer},
};

/// A hook that is called when the torrent is ready to start downloading.
/// This is used to implement
/// [`Torrent::poll_ready`](crate::torrent::Torrent::poll_ready).
pub(super) type ReadyHookSender = oneshot::Sender<()>;

pub(super) enum PieceManagerProxy {
   Custom(Box<dyn PieceManager>),
   Default(FilePieceManager),
}

impl Display for PieceManagerProxy {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
   pub(crate) peers: HashMap<PeerId, ActorRef<PeerActor>>,
   pub(crate) trackers: HashMap<Tracker, ActorRef<TrackerActor>>,

   pub(crate) bitfield: BitVec<AtomicU8>,
   pub(super) id: PeerId,
   pub(super) info: Option<Info>,
   pub(super) metainfo: MetaInfo,
   #[allow(dead_code)]
   pub(super) tracker_server: UdpServer,
   pub(super) scheduler: ActorRef<Scheduler>,
   /// Should only be used to create new connections
   pub(super) utp_server: Arc<UtpSocketUdp>,
   pub(super) actor_ref: ActorRef<Self>,
   pub(super) piece_storage: PieceStorageStrategy,
   pub(super) piece_store: ActorRef<PieceStoreActor>,
   pub(super) piece_manager: PieceManagerProxy,
   pub state: TorrentState,
   /// Scheduler for managing piece and block requests
   pub(super) piece_scheduler: PieceScheduler,

   pub(super) start_time: Option<Instant>,
   /// The number of peers we need to have before we start downloading, defaults
   /// to 6.
   pub(super) sufficient_peers: usize,

   pub(super) autostart: bool,

   /// If there is already a pending start, we don't want to start a new one
   pub(super) pending_start: bool,

   pub(super) ready_hook: Vec<ReadyHookSender>,
}

impl fmt::Display for TorrentActor {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
            self.send_ready_hooks();
         }
      }

      self.pending_start = false;
   }

   pub async fn start(&mut self) {
      self.send_ready_hooks();

      let Some(info) = self.info.clone() else {
         warn!(id = %self.info_hash(), "Start requested before info dict is available; deferring");
         return;
      };

      // Pre-start the piece manager before transitioning state
      if let Err(err) = self.piece_manager.pre_start(info.clone()).await {
         error!(?err, "Failed to pre-start piece manager; aborting start");
         return;
      }

      if self.is_full() {
         self.state = TorrentState::Seeding;
         info!(id = %self.info_hash(), "Torrent is now seeding");
         self
            .update_trackers(TrackerUpdate::Event(Event::Completed))
            .await;
      } else {
         self.state = TorrentState::Downloading;
         info!(id = %self.info_hash(), "Torrent is now downloading");

         self
            .update_trackers(TrackerUpdate::Event(Event::Started))
            .await;
         self.broadcast_to_trackers(TrackerMessage::Announce).await;
         self
            .update_trackers(TrackerUpdate::Event(Event::Empty))
            .await;
         self.start_time = Some(Instant::now());
      };

      if self.state == TorrentState::Downloading {
         let peer_ids: Vec<_> = self.peers.keys().copied().collect();
         for peer_id in peer_ids {
            self.request_blocks_from_peer(peer_id, 32).await;
         }
      }

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

   pub(super) fn send_ready_hooks(&mut self) {
      for hook in self.ready_hook.drain(..) {
         if let Err(err) = hook.send(()) {
            error!(?err, "Failed to send ready hook");
         }
      }
   }

   pub fn total_bytes_downloaded(&self) -> Option<usize> {
      let info = self.info_dict()?;
      let total_length = info.total_length();
      let piece_length = info.piece_length as usize;

      let num_pieces = self.bitfield.len();
      let mut total_bytes = 0usize;

      let last_piece_len = if total_length % piece_length == 0 {
         piece_length
      } else {
         total_length % piece_length
      };

      for piece_idx in 0..num_pieces {
         if self.bitfield[piece_idx] {
            let piece_size = if piece_idx == num_pieces - 1 {
               last_piece_len
            } else {
               piece_length
            };
            total_bytes = total_bytes.saturating_add(piece_size);
         }
      }

      let block_map = self.piece_scheduler.block_map_export();
      for entry in block_map.iter() {
         let piece_idx = *entry.key();
         let block = entry.value();
         if piece_idx < num_pieces && !self.bitfield[piece_idx] {
            let piece_size = if piece_idx == num_pieces - 1 {
               last_piece_len
            } else {
               piece_length
            };

            let mut piece_offset = 0usize;
            for block_idx in 0..block.len() {
               if block[block_idx] {
                  let block_size = (piece_size - piece_offset).min(BLOCK_SIZE);
                  total_bytes = total_bytes.saturating_add(block_size);
               }
               piece_offset =
                  piece_offset.saturating_add(BLOCK_SIZE.min(piece_size - piece_offset));
            }
         }
      }

      Some(total_bytes)
   }

   pub fn export(&self) -> TorrentExport {
      TorrentExport {
         info_hash: self.info_hash(),
         state: self.state,
         auto_start: self.autostart,
         sufficient_peers: self.sufficient_peers,
         output_path: match &self.piece_manager {
            PieceManagerProxy::Default(manager) => manager.path().cloned(),
            _ => None,
         },
         metainfo: self.metainfo.clone(),
         piece_storage: self.piece_storage.clone(),
         info_dict: self.info_dict().cloned(),
         bitfield: self.bitfield.clone(),
         block_map: self.piece_scheduler.block_map_export(),
      }
   }

   pub fn is_full(&self) -> bool {
      self.bitfield.count_ones() == self.bitfield.len()
   }

   pub fn is_ready(&self) -> bool {
      self.info.is_some() && self.peers.len() >= self.sufficient_peers
   }

   pub fn is_ready_to_start(&self) -> bool {
      self.is_ready() && self.state == TorrentState::Inactive
   }
}

/// Configuration arguments for creating a [`TorrentActor`].
///
/// This struct provides a well-documented way to configure the torrent actor
/// instead of using an unlabeled tuple. Some fields are required; optional
/// fields have sensible defaults.
#[derive(Debug, Clone)]
pub struct TorrentActorArgs {
   /// Peer ID for this torrent instance.
   ///
   /// This should typically match the engine's peer ID.
   pub peer_id: PeerId,

   /// Meta information for the torrent (either from a .torrent file or magnet
   /// URI).
   ///
   /// This contains all the necessary information to start downloading/seeding.
   pub metainfo: MetaInfo,

   /// uTP server for peer connections.
   ///
   /// This is used to establish uTP connections with peers.
   pub utp_server: Arc<UtpSocketUdp>,

   /// UDP server for tracker communication.
   ///
   /// This is used to communicate with UDP trackers.
   pub tracker_server: UdpServer,

   /// Primary address for this torrent instance.
   ///
   /// If not provided, defaults to the uTP server's bind address.
   pub primary_addr: Option<SocketAddr>,

   /// Strategy for storing torrent pieces.
   ///
   /// This determines how pieces are stored and accessed for this torrent.
   pub piece_storage: PieceStorageStrategy,

   /// Whether to automatically start this torrent when it becomes ready.
   ///
   /// If not provided, defaults to `true`.
   pub autostart: Option<bool>,

   /// Minimum number of peers required before starting download.
   ///
   /// If not provided, defaults to 6.
   pub sufficient_peers: Option<usize>,

   /// Base path for torrent downloads.
   ///
   /// If not provided, torrents will use their own default paths.
   pub base_path: Option<PathBuf>,
}

impl Actor for TorrentActor {
   type Args = TorrentActorArgs;

   type Error = TorrentError;

   fn supervision_strategy() -> SupervisionStrategy {
      SupervisionStrategy::OneForOne
   }

   async fn on_start(args: Self::Args, us: ActorRef<Self>) -> Result<Self, Self::Error> {
      let TorrentActorArgs {
         peer_id,
         metainfo,
         utp_server,
         tracker_server,
         primary_addr,
         piece_storage,
         autostart,
         sufficient_peers,
         base_path,
      } = args;

      let torrent_id = metainfo.info_hash()?;
      let primary_addr = primary_addr.unwrap_or_else(|| {
         let addr = utp_server.bind_addr();
         debug!(torrent_id = %torrent_id, %addr, "No primary address provided, using default");
         addr
      });
      if let PieceStorageStrategy::Disk(dir) = &piece_storage {
         util::create_dir(dir).await?;
      }

      info!(
         torrent_id = %torrent_id,
         "Starting new torrent instance",
      );

      let scheduler = Scheduler::supervise_with(&us, Scheduler::new)
         .restart_policy(RestartPolicy::Permanent)
         .restart_limit(3, std::time::Duration::from_secs(10))
         .spawn()
         .await;

      // Create tracker actors
      let tracker_list = metainfo.announce_list();
      let mut trackers = HashMap::new();
      for tracker in tracker_list {
         let actor = TrackerActor::supervise(
            &us,
            (
               tracker.clone(),
               peer_id,
               tracker_server.clone(),
               primary_addr,
               us.clone(),
               scheduler.clone(),
            ),
         )
         .restart_policy(RestartPolicy::Transient)
         .restart_limit(3, std::time::Duration::from_secs(60))
         .spawn()
         .await;
         trackers.insert(tracker, actor);
      }
      let info = match &metainfo {
         MetaInfo::Torrent(t) => Some(t.info.clone()),
         _ => None,
      };
      if info.is_none() {
         debug!(torrent_id = %torrent_id, "No info dict found in metainfo, you're probably using a magnet uri");
      }
      let piece_count = if let Some(info) = &info {
         debug!(torrent_id = %torrent_id, "Using bitfield length {}", info.piece_count());
         info.piece_count()
      } else {
         0
      };
      let bitfield = BitVec::repeat(false, piece_count);
      let default_manager = FilePieceManager(base_path, info.clone());
      let piece_store = PieceStoreActor::supervise(&us, ())
         .restart_policy(RestartPolicy::Permanent)
         .restart_limit(3, std::time::Duration::from_secs(10))
         .spawn()
         .await;

      Ok(Self {
         peers: HashMap::new(),
         bitfield,
         tracker_server,
         scheduler,
         utp_server,
         trackers,
         id: peer_id,
         metainfo,
         info,
         actor_ref: us,
         piece_storage,
         piece_store,
         state: TorrentState::default(),
         piece_scheduler: PieceScheduler::new(piece_count),
         start_time: None,
         sufficient_peers: sufficient_peers.unwrap_or(6),
         autostart: autostart.unwrap_or(true),
         pending_start: false,
         ready_hook: Vec::new(),
         piece_manager: PieceManagerProxy::Default(default_manager),
      })
   }

   async fn next(
      &mut self, _: WeakActorRef<Self>, mailbox_rx: &mut MailboxReceiver<Self>,
   ) -> Result<Option<Signal<Self>>, Self::Error> {
      if !self.pending_start {
         self.autostart().await;
      }
      Ok(mailbox_rx.recv().await)
   }

   async fn on_stop(
      &mut self, _: WeakActorRef<Self>, _: ActorStopReason,
   ) -> Result<(), Self::Error> {
      for peer in self.peers.values() {
         peer.kill();
      }
      for tracker in self.trackers.values() {
         tracker.kill();
      }
      self.piece_store.kill();
      self.scheduler.kill();

      Ok(())
   }
}

#[cfg(test)]
mod tests {
   use std::{path::PathBuf, time::Duration};

   use librqbit_utp::UtpSocket;
   use tokio::{fs, time::sleep};
   use tracing::trace;

   use super::*;
   use crate::{
      metainfo::MetaInfo,
      testing,
      torrent::{BLOCK_SIZE, Torrent, TorrentExport, commands::HasInfoDict},
   };

   #[tokio::test(flavor = "multi_thread")]
   #[ignore = "external-network test: reaches public trackers and peers"]
   async fn torrent_actor_when_public_torrent_is_available_then_reaches_ready_state() {
      testing::init_tracing();
      let metainfo = testing::read_torrent_fixture(testing::BIG_BUCK_BUNNY_TORRENT_FILE).await;

      let peer_id = testing::peer_id();

      let udp_server = testing::udp_server().await;
      let utp_server = UtpSocket::new_udp(testing::ephemeral_socket_addr())
         .await
         .unwrap();
      let sufficient_peers = 6;

      let info_hash = metainfo.clone().info_hash().unwrap();

      let actor = TorrentActor::spawn(TorrentActorArgs {
         peer_id,
         metainfo,
         utp_server,
         tracker_server: udp_server.clone(),
         primary_addr: None,
         piece_storage: PieceStorageStrategy::default(),
         autostart: Some(false), /* We don't need to autostart because we're only checking if we
                                  * have peers */
         sufficient_peers: Some(sufficient_peers),
         base_path: None,
      });

      let torrent = Torrent::new(info_hash, actor.clone());

      assert!(torrent.poll_ready().await.is_ok());

      actor.stop_gracefully().await.expect("Failed to stop");
   }

   #[tokio::test(flavor = "multi_thread")]
   #[ignore = "external-network test: reaches public trackers and peers"]
   async fn torrent_actor_when_public_magnet_uri_is_available_then_retrieves_info_dict() {
      testing::init_tracing();
      let metainfo = testing::read_magnet_fixture(testing::BIG_BUCK_BUNNY_MAGNET_FILE).await;

      let peer_id = testing::peer_id();

      let udp_server = testing::udp_server().await;
      let utp_server = UtpSocket::new_udp(testing::ephemeral_socket_addr())
         .await
         .unwrap();

      let actor = TorrentActor::spawn(TorrentActorArgs {
         peer_id,
         metainfo,
         utp_server,
         tracker_server: udp_server.clone(),
         primary_addr: None,
         piece_storage: PieceStorageStrategy::default(),
         autostart: Some(false),
         sufficient_peers: None,
         base_path: None,
      });

      // Blocking loop that runs until we get an info dict
      loop {
         if actor.ask(HasInfoDict).await.unwrap().is_some() {
            trace!("Got info dict!");
            break;
         }
         sleep(Duration::from_millis(100)).await;
      }

      actor.stop_gracefully().await.expect("Failed to stop");
   }

   #[tokio::test(flavor = "multi_thread")]
   #[ignore = "external-network test: reaches public trackers and peers"]
   async fn torrent_actor_when_public_torrent_is_available_then_writes_piece_storage() {
      testing::init_tracing();
      let metainfo = testing::read_torrent_fixture(testing::BIG_BUCK_BUNNY_TORRENT_FILE).await;
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

      let piece_path = testing::torrent_temp_path();
      let file_path = piece_path.join("files");

      let peer_id = testing::peer_id();

      let udp_server = testing::udp_server().await;
      let utp_server = UtpSocket::new_udp(testing::ephemeral_socket_addr())
         .await
         .unwrap();

      let actor = TorrentActor::spawn(TorrentActorArgs {
         peer_id,
         metainfo,
         utp_server,
         tracker_server: udp_server.clone(),
         primary_addr: None,
         piece_storage: PieceStorageStrategy::Disk(piece_path.clone()),
         autostart: None,
         sufficient_peers: None,
         base_path: Some(file_path),
      });

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

   #[tokio::test(flavor = "multi_thread")]
   async fn torrent_actor_when_pieces_are_marked_complete_then_exports_progress_correctly() {
      testing::init_tracing();
      let metainfo = testing::read_torrent_fixture(testing::BIG_BUCK_BUNNY_TORRENT_FILE).await;
      let info_dict = match &metainfo {
         MetaInfo::Torrent(file) => file.info.clone(),
         _ => unreachable!(),
      };
      let info_hash = info_dict.hash().unwrap();

      let piece_path = testing::torrent_temp_path();
      let file_path = piece_path.join("files");

      let peer_id = testing::peer_id();
      let udp_server = testing::udp_server().await;
      let utp_server = UtpSocket::new_udp(testing::ephemeral_socket_addr())
         .await
         .unwrap();

      // Spawn the actor first so we get an ActorRef, then immediately stop it
      // and reconstruct state for direct export testing.
      let actor_ref = TorrentActor::spawn(TorrentActorArgs {
         peer_id,
         metainfo: metainfo.clone(),
         utp_server: utp_server.clone(),
         tracker_server: udp_server.clone(),
         primary_addr: None,
         piece_storage: PieceStorageStrategy::Disk(piece_path.clone()),
         autostart: Some(false),
         sufficient_peers: Some(usize::MAX),
         base_path: Some(file_path.clone()),
      });

      // Build the bitfield with fake completed pieces
      let piece_count = info_dict.piece_count();
      let bitfield: BitVec<AtomicU8> = BitVec::repeat(false, piece_count);
      let fake_completed: usize = 3;
      for i in 0..fake_completed {
         bitfield.set_aliased(i, true);
      }

      // Build a fake block map with one partial piece
      let partial_piece_index = fake_completed; // next piece
      let total_blocks = (info_dict.piece_length as usize).div_ceil(BLOCK_SIZE);
      let mut blocks = BitVec::<usize>::repeat(false, total_blocks);
      let partial_blocks_received = 2;
      for i in 0..partial_blocks_received {
         blocks.set(i, true);
      }
      let mut piece_scheduler = PieceScheduler::new(info_dict.piece_count());
      piece_scheduler.set_piece_blocks(partial_piece_index, blocks);

      // Construct the actor manually for export testing
      let test_actor = TorrentActor {
         peers: HashMap::new(),
         trackers: HashMap::new(),
         bitfield,
         id: peer_id,
         info: Some(info_dict.clone()),
         metainfo: metainfo.clone(),
         tracker_server: udp_server.clone(),
         scheduler: Scheduler::spawn(Scheduler::new()),
         utp_server,
         actor_ref: actor_ref.clone(),
         piece_storage: PieceStorageStrategy::Disk(piece_path.clone()),
         piece_store: PieceStoreActor::spawn(()),
         piece_manager: PieceManagerProxy::Default(FilePieceManager(
            Some(file_path),
            Some(info_dict.clone()),
         )),
         state: TorrentState::Inactive,
         piece_scheduler,
         start_time: None,
         sufficient_peers: 6,
         autostart: false,
         pending_start: false,
         ready_hook: Vec::new(),
      };

      let export = test_actor.export();

      // Verify export contents
      assert_eq!(export.info_hash, info_hash);
      assert_eq!(export.state, TorrentState::Inactive);
      assert!(!export.auto_start);
      assert_eq!(export.sufficient_peers, 6);
      assert!(export.info_dict.is_some(), "Info dict should be present");
      assert_eq!(export.bitfield.count_ones(), fake_completed);
      assert_eq!(export.bitfield.len(), piece_count);
      assert_eq!(export.block_map.len(), 1);

      let partial_entry = export.block_map.get(&partial_piece_index).unwrap();
      assert_eq!(partial_entry.count_ones(), partial_blocks_received);

      match &export.piece_storage {
         PieceStorageStrategy::Disk(p) => assert_eq!(p, &piece_path),
         _ => panic!("Expected Disk storage strategy"),
      }

      // Test serialization round-trip
      use serde_json::{from_str, to_string};
      let export_str = to_string(&export).unwrap();
      let from_export: TorrentExport = from_str(&export_str).unwrap();

      assert_eq!(export.info_hash, from_export.info_hash);
      assert_eq!(export.state, from_export.state);
      assert_eq!(export.auto_start, from_export.auto_start);
      assert_eq!(export.sufficient_peers, from_export.sufficient_peers);
      assert_eq!(export.output_path, from_export.output_path);
      assert_eq!(export.bitfield, from_export.bitfield);
      assert_eq!(export.block_map.len(), from_export.block_map.len());

      trace!("Export: {export_str}");

      actor_ref.stop_gracefully().await.unwrap();
   }
}
