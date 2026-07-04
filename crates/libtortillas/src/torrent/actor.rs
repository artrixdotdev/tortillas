use std::{
   collections::HashMap,
   fmt::{self, Display},
   net::SocketAddr,
   ops::ControlFlow,
   path::PathBuf,
   sync::{Arc, atomic::AtomicU8},
   time::Instant,
};

use async_trait::async_trait;
use bitvec::vec::BitVec;
use bytes::Bytes;
use kameo::{
   Actor,
   actor::{ActorId, ActorRef, Spawn, WeakActorRef},
   error::ActorStopReason,
   mailbox::{MailboxReceiver, Signal},
   supervision::{RestartPolicy, SupervisionStrategy},
};
use kameo_actors::scheduler::{Scheduler, SetTimeout};
use librqbit_utp::UtpSocketUdp;
use tokio::{sync::oneshot, task::AbortHandle};
use tracing::{debug, error, info, instrument, trace, warn};

use super::{choking::ChokingScheduler, util};
use crate::{
   errors::TorrentError,
   hashes::InfoHash,
   metainfo::{Info, MetaInfo},
   peer::{PeerActor, PeerId},
   pieces::{FilePieceManager, PieceManager, PieceScheduler, PieceStoreActor},
   settings::Settings,
   torrent::{BLOCK_SIZE, PieceStorageStrategy, TorrentExport, TorrentState},
   tracker::{
      Announce, Event, Tracker, TrackerActor, TrackerActorArgs, TrackerUpdate, udp::UdpServer,
   },
};

/// A hook that is called when the torrent is ready to start downloading.
/// This is used to implement
/// [`Torrent::poll_ready`](crate::torrent::Torrent::poll_ready).
pub(super) type ReadyHookSender = oneshot::Sender<()>;

pub(super) enum PieceManagerProxy {
   Custom(Box<dyn PieceManager>),
   Default(FilePieceManager),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct TrackerAnnounceProgress {
   pub(super) downloaded: usize,
   pub(super) left: usize,
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
   /// Scheduler for BEP 3 upload choking decisions.
   pub(super) choking_scheduler: ChokingScheduler,
   pub(super) next_rechoke: Option<AbortHandle>,

   pub(super) start_time: Option<Instant>,
   /// The number of peers we need to have before we start downloading, defaults
   /// to 6.
   pub(super) sufficient_peers: usize,

   pub(super) autostart: bool,

   /// If there is already a pending start, we don't want to start a new one
   pub(super) pending_start: bool,

   pub(super) ready_hook: Vec<ReadyHookSender>,
   pub(super) settings: Settings,
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
            self.state = TorrentState::Ready;
            self.send_ready_hooks();
         }
      }

      self.pending_start = false;
   }

   pub async fn start(&mut self) {
      if !self.state.can_start() {
         trace!(state = ?self.state, "Ignoring start while torrent cannot start");
         return;
      }

      self.send_ready_hooks();

      let Some(info) = self.info.clone() else {
         self.state = TorrentState::ResolvingMetadata;
         warn!(id = %self.info_hash(), "Start requested before info dict is available; deferring");
         return;
      };

      // Pre-start the piece manager before transitioning state
      if let Err(err) = self.piece_manager.pre_start(info.clone()).await {
         self.state = TorrentState::Failed;
         error!(?err, "Failed to pre-start piece manager; aborting start");
         return;
      }

      self.sync_tracker_announce_progress().await;

      if self.is_full() {
         self.state = TorrentState::Seeding;
         info!(id = %self.info_hash(), "Torrent is now seeding");
      } else {
         self.state = TorrentState::Downloading;
         info!(id = %self.info_hash(), "Torrent is now downloading");
         self.start_time = Some(Instant::now());
      };

      self
         .update_trackers(TrackerUpdate::Event(Event::Started))
         .await;
      self.broadcast_to_trackers(Announce).await;
      self
         .update_trackers(TrackerUpdate::Event(Event::Empty))
         .await;

      if self.state == TorrentState::Downloading {
         let peer_ids: Vec<_> = self.peers.keys().copied().collect();
         for peer_id in peer_ids {
            self
               .request_blocks_from_peer(peer_id, self.settings.torrent.initial_peer_request_window)
               .await;
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

      self.rechoke_peers().await;
      self.schedule_next_rechoke().await;
   }

   pub(super) async fn schedule_next_rechoke(&mut self) {
      if !self.state.is_transfer_active() {
         return;
      }

      if let Some(next_rechoke) = self.next_rechoke.take() {
         next_rechoke.abort();
      }

      let rechoke_interval = self.settings.torrent.rechoke_interval;
      match self
         .scheduler
         .ask(SetTimeout::new(
            self.actor_ref.downgrade(),
            rechoke_interval,
            super::commands::Rechoke,
         ))
         .await
      {
         Ok(next_rechoke) => self.next_rechoke = Some(next_rechoke),
         Err(err) => {
            warn!(?err, "Failed to schedule next rechoke; using local timeout");
            let actor_ref = self.actor_ref.clone();
            let fallback = tokio::spawn(async move {
               tokio::time::sleep(rechoke_interval).await;
               if let Err(err) = actor_ref.tell(super::commands::Rechoke).await {
                  warn!(?err, "Failed to run fallback rechoke");
               }
            });
            self.next_rechoke = Some(fallback.abort_handle());
         }
      }
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

   pub(super) fn tracker_announce_progress(&self) -> Option<TrackerAnnounceProgress> {
      let info = self.info_dict()?;
      let total_length = info.total_length();
      let downloaded = self.total_bytes_downloaded()?.min(total_length);
      Some(TrackerAnnounceProgress {
         downloaded,
         left: total_length.saturating_sub(downloaded),
      })
   }

   pub(super) async fn sync_tracker_announce_progress(&mut self) {
      let Some(progress) = self.tracker_announce_progress() else {
         return;
      };

      self
         .update_trackers(TrackerUpdate::Downloaded(progress.downloaded))
         .await;
      self
         .update_trackers(TrackerUpdate::Left(progress.left))
         .await;
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
      self.is_ready() && self.state.can_become_ready()
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

   /// Runtime behavior settings.
   pub settings: Settings,
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
         settings,
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
         .restart_limit(
            settings.torrent.scheduler_restart.limit,
            settings.torrent.scheduler_restart.period,
         )
         .spawn()
         .await;

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
      let initial_state = if info.is_some() {
         TorrentState::Added
      } else {
         TorrentState::ResolvingMetadata
      };
      let bitfield = BitVec::repeat(false, piece_count);
      let initial_left = info.as_ref().map(Info::total_length);

      // Create tracker actors
      let tracker_list = metainfo.announce_list();
      let mut trackers = HashMap::new();
      for tracker in tracker_list {
         let actor = TrackerActor::supervise(
            &us,
            TrackerActorArgs {
               tracker: tracker.clone(),
               peer_id,
               server: tracker_server.clone(),
               socket_addr: primary_addr,
               initial_left,
               supervisor: us.clone(),
               scheduler: scheduler.clone(),
               settings: settings.tracker.clone(),
            },
         )
         .restart_policy(RestartPolicy::Transient)
         .restart_limit(
            settings.torrent.tracker_restart.limit,
            settings.torrent.tracker_restart.period,
         )
         .spawn()
         .await;

         trackers.insert(tracker, actor);
      }
      let default_manager = FilePieceManager(base_path, info.clone());
      let piece_store = PieceStoreActor::supervise(&us, ())
         .restart_policy(RestartPolicy::Permanent)
         .restart_limit(
            settings.torrent.piece_store_restart.limit,
            settings.torrent.piece_store_restart.period,
         )
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
         state: initial_state,
         piece_scheduler: PieceScheduler::new(piece_count),
         choking_scheduler: ChokingScheduler::new(
            settings.torrent.upload_slots,
            settings.torrent.optimistic_unchoke_rounds,
         ),
         next_rechoke: None,
         start_time: None,
         sufficient_peers: sufficient_peers.unwrap_or(settings.torrent.sufficient_peers),
         autostart: autostart.unwrap_or(settings.torrent.autostart),
         pending_start: false,
         ready_hook: Vec::new(),
         piece_manager: PieceManagerProxy::Default(default_manager),
         settings,
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

   #[instrument(skip(self), fields(torrent_id = %self.info_hash()))]
   async fn on_stop(
      &mut self, _: WeakActorRef<Self>, reason: ActorStopReason,
   ) -> Result<(), Self::Error> {
      self.state = TorrentState::Stopping;
      info!(reason = %reason, "Torrent stopped");
      for peer in self.peers.values() {
         peer.kill();
      }
      for tracker in self.trackers.values() {
         tracker.kill();
      }
      if let Some(next_rechoke) = self.next_rechoke.take() {
         next_rechoke.abort();
      }
      self.piece_store.kill();
      self.scheduler.kill();
      self.state = TorrentState::Stopped;

      Ok(())
   }

   #[instrument(skip(self), fields(torrent_id = %self.info_hash()))]
   async fn on_link_died(
      &mut self, _: WeakActorRef<Self>, id: ActorId, reason: ActorStopReason,
   ) -> Result<ControlFlow<ActorStopReason>, Self::Error> {
      error!(?id, ?reason, "Linked child died");

      Ok(ControlFlow::Continue(()))
   }
}

#[cfg(test)]
mod tests {
   use std::{path::PathBuf, time::Duration};

   use librqbit_utp::UtpSocket;
   use tokio::{
      fs,
      io::{AsyncReadExt, AsyncWriteExt},
      net::TcpListener,
      sync::oneshot,
      time::{sleep, timeout},
   };
   use tracing::trace;

   use super::*;
   use crate::{
      hashes::HashVec,
      metainfo::{InfoKeys, MetaInfo, TorrentFile},
      settings::Settings,
      testing,
      torrent::{
         BLOCK_SIZE, Torrent, TorrentExport,
         commands::{ExportState, GetState, HasInfoDict, SetState},
      },
      tracker::Tracker,
   };

   fn empty_torrent(tracker: Tracker) -> MetaInfo {
      MetaInfo::Torrent(TorrentFile {
         announce: tracker,
         announce_list: None,
         comment: None,
         created_by: None,
         creation_date: None,
         encoding: None,
         info: Info {
            name: "empty".to_string(),
            piece_length: BLOCK_SIZE as u64,
            pieces: HashVec::new(),
            file: InfoKeys::Single {
               length: 0,
               md5sum: None,
            },
            is_private: None,
            publisher: None,
            publisher_url: None,
            source: None,
         },
         url_list: None,
      })
   }

   async fn one_shot_http_tracker() -> (Tracker, oneshot::Receiver<String>) {
      let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
      let addr = listener.local_addr().unwrap();
      let (query_tx, query_rx) = oneshot::channel();

      tokio::spawn(async move {
         let (mut stream, _) = listener.accept().await.unwrap();
         let mut buf = [0; 4096];
         let read = stream.read(&mut buf).await.unwrap();
         let request = String::from_utf8_lossy(&buf[..read]);
         let request_target = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or_default();
         let query = request_target
            .split_once('?')
            .map(|(_, query)| query.to_string())
            .unwrap_or_default();
         let _ = query_tx.send(query);

         let body = b"d8:intervali60e5:peers0:e";
         let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
         );
         stream.write_all(response.as_bytes()).await.unwrap();
         stream.write_all(body).await.unwrap();
      });

      (Tracker::Http(format!("http://{addr}/announce")), query_rx)
   }

   #[tokio::test(flavor = "multi_thread")]
   async fn torrent_actor_when_started_then_announces_started_with_current_progress() {
      testing::init_tracing();
      let (tracker, query_rx) = one_shot_http_tracker().await;
      let mut metainfo = testing::read_torrent_fixture(testing::BIG_BUCK_BUNNY_TORRENT_FILE).await;
      let info = match &mut metainfo {
         MetaInfo::Torrent(file) => {
            file.announce = tracker;
            file.announce_list = None;
            file.info.clone()
         }
         _ => unreachable!(),
      };
      let info_hash = info.hash().unwrap();
      let peer_id = testing::peer_id();
      let piece_path = testing::torrent_temp_path();
      let file_path = piece_path.join("files");
      let udp_server = testing::udp_server().await;
      let utp_server = UtpSocket::new_udp(testing::ephemeral_socket_addr())
         .await
         .unwrap();
      let mut settings = Settings::default();
      settings.tracker.initial_announce_delay = Duration::from_secs(60);

      let actor = TorrentActor::spawn(TorrentActorArgs {
         peer_id,
         metainfo,
         utp_server,
         tracker_server: udp_server,
         primary_addr: None,
         piece_storage: PieceStorageStrategy::default(),
         autostart: Some(false),
         sufficient_peers: Some(usize::MAX),
         base_path: Some(file_path),
         settings,
      });
      actor
         .tell(SetState {
            state: TorrentState::Downloading,
         })
         .await
         .unwrap();

      let query = timeout(Duration::from_secs(5), query_rx)
         .await
         .expect("tracker should receive a start announce")
         .expect("tracker query should be captured");

      assert!(query.contains("event=started"));
      assert!(query.contains("uploaded=0"));
      assert!(query.contains("downloaded=0"));
      assert!(query.contains(&format!("left={}", info.total_length())));
      assert!(query.contains("compact=0"));

      let export = actor.ask(ExportState).await.unwrap();
      assert_eq!(export.info_hash, info_hash);
      assert_eq!(export.state, TorrentState::Downloading);

      actor.stop_gracefully().await.unwrap();
   }

   #[tokio::test(flavor = "multi_thread")]
   async fn torrent_actor_when_initial_data_is_complete_then_announces_started_as_seeder() {
      testing::init_tracing();
      let (tracker, query_rx) = one_shot_http_tracker().await;
      let metainfo = empty_torrent(tracker);
      let peer_id = testing::peer_id();
      let udp_server = testing::udp_server().await;
      let utp_server = UtpSocket::new_udp(testing::ephemeral_socket_addr())
         .await
         .unwrap();
      let mut settings = Settings::default();
      settings.tracker.initial_announce_delay = Duration::from_secs(60);

      let actor = TorrentActor::spawn(TorrentActorArgs {
         peer_id,
         metainfo,
         utp_server,
         tracker_server: udp_server,
         primary_addr: None,
         piece_storage: PieceStorageStrategy::default(),
         autostart: Some(false),
         sufficient_peers: Some(usize::MAX),
         base_path: Some(testing::torrent_temp_path()),
         settings,
      });
      actor
         .tell(SetState {
            state: TorrentState::Downloading,
         })
         .await
         .unwrap();

      let query = timeout(Duration::from_secs(5), query_rx)
         .await
         .expect("tracker should receive a start announce")
         .expect("tracker query should be captured");

      assert!(query.contains("event=started"));
      assert!(query.contains("downloaded=0"));
      assert!(query.contains("left=0"));
      assert_eq!(actor.ask(GetState).await.unwrap(), TorrentState::Seeding);

      actor.stop_gracefully().await.unwrap();
   }

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
         settings: Settings::default(),
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
         settings: Settings::default(),
      });

      // Blocking loop that runs until we get an info dict
      loop {
         match actor.ask(HasInfoDict).await {
            Ok(Some(_)) => {
               trace!("Got info dict!");
               break;
            }
            Ok(None) => {}
            Err(err) => {
               error!(error = %err, "Failed to get info dict");
               break;
            }
         }
         sleep(Duration::from_millis(100)).await;
      }

      actor.stop_gracefully().await.expect("Failed to stop");
   }

   #[tokio::test(flavor = "multi_thread")]
   async fn torrent_actor_when_metadata_is_ready_without_autostart_then_reports_ready() {
      testing::init_tracing();
      let metainfo = testing::read_torrent_fixture(testing::BIG_BUCK_BUNNY_TORRENT_FILE).await;

      let actor = TorrentActor::spawn(TorrentActorArgs {
         peer_id: testing::peer_id(),
         metainfo,
         utp_server: UtpSocket::new_udp(testing::ephemeral_socket_addr())
            .await
            .unwrap(),
         tracker_server: testing::udp_server().await,
         primary_addr: None,
         piece_storage: PieceStorageStrategy::default(),
         autostart: Some(false),
         sufficient_peers: Some(0),
         base_path: None,
         settings: Settings::default(),
      });

      assert_eq!(actor.ask(GetState).await.unwrap(), TorrentState::Ready);

      actor.stop_gracefully().await.expect("Failed to stop");
   }

   #[tokio::test(flavor = "multi_thread")]
   async fn torrent_actor_when_metadata_is_missing_then_reports_resolving_metadata() {
      testing::init_tracing();

      let actor = TorrentActor::spawn(TorrentActorArgs {
         peer_id: testing::peer_id(),
         metainfo: testing::big_buck_bunny_magnet(),
         utp_server: UtpSocket::new_udp(testing::ephemeral_socket_addr())
            .await
            .unwrap(),
         tracker_server: testing::udp_server().await,
         primary_addr: None,
         piece_storage: PieceStorageStrategy::default(),
         autostart: Some(false),
         sufficient_peers: Some(0),
         base_path: None,
         settings: Settings::default(),
      });

      assert_eq!(
         actor.ask(GetState).await.unwrap(),
         TorrentState::ResolvingMetadata
      );

      actor.stop_gracefully().await.expect("Failed to stop");
   }

   #[tokio::test(flavor = "multi_thread")]
   async fn torrent_actor_when_paused_then_does_not_report_ready() {
      testing::init_tracing();
      let metainfo = testing::read_torrent_fixture(testing::BIG_BUCK_BUNNY_TORRENT_FILE).await;

      let actor = TorrentActor::spawn(TorrentActorArgs {
         peer_id: testing::peer_id(),
         metainfo,
         utp_server: UtpSocket::new_udp(testing::ephemeral_socket_addr())
            .await
            .unwrap(),
         tracker_server: testing::udp_server().await,
         primary_addr: None,
         piece_storage: PieceStorageStrategy::default(),
         autostart: Some(false),
         sufficient_peers: Some(0),
         base_path: None,
         settings: Settings::default(),
      });

      actor
         .tell(SetState {
            state: TorrentState::Paused,
         })
         .await
         .unwrap();

      assert_eq!(actor.ask(GetState).await.unwrap(), TorrentState::Paused);

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
         settings: Settings::default(),
      });

      let torrent = Torrent::new(info_hash, actor.clone());

      assert!(torrent.poll_ready().await.is_ok());

      let wrote_piece_block = timeout(Duration::from_secs(60), async {
         loop {
            let export = actor.ask(ExportState).await.unwrap();
            let has_persisted_progress = export.bitfield.count_ones() > 0
               || export
                  .block_map
                  .iter()
                  .any(|entry| entry.value().count_ones() > 0);

            if has_persisted_progress {
               let mut entries = fs::read_dir(&piece_path).await.unwrap();
               while let Some(entry) = entries.next_entry().await.unwrap() {
                  let path = entry.path();
                  if let Some(ext) = path.extension()
                     && ext == "piece"
                     && entry.metadata().await.unwrap().len() > 0
                  {
                     return true;
                  }
               }
            }

            sleep(Duration::from_millis(200)).await;
         }
      })
      .await
      .unwrap_or(false);

      assert!(
         wrote_piece_block,
         "timed out waiting for piece storage write"
      );

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
         settings: Settings::default(),
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
         state: TorrentState::Added,
         piece_scheduler,
         choking_scheduler: ChokingScheduler::default(),
         next_rechoke: None,
         start_time: None,
         sufficient_peers: 6,
         autostart: false,
         pending_start: false,
         ready_hook: Vec::new(),
         settings: Settings::default(),
      };

      let export = test_actor.export();

      // Verify export contents
      assert_eq!(export.info_hash, info_hash);
      assert_eq!(export.state, TorrentState::Added);
      assert!(!export.auto_start);
      assert_eq!(export.sufficient_peers, 6);
      assert!(export.info_dict.is_some(), "Info dict should be present");
      assert_eq!(export.bitfield.count_ones(), fake_completed);
      assert_eq!(export.bitfield.len(), piece_count);
      assert_eq!(export.block_map.len(), 1);

      let partial_entry = export.block_map.get(&partial_piece_index).unwrap();
      assert_eq!(partial_entry.count_ones(), partial_blocks_received);

      let announce_progress = test_actor
         .tracker_announce_progress()
         .expect("tracker progress should be available with an info dict");
      let expected_downloaded = (fake_completed * info_dict.piece_length as usize)
         + (partial_blocks_received * BLOCK_SIZE);
      assert_eq!(announce_progress.downloaded, expected_downloaded);
      assert_eq!(
         announce_progress.left,
         info_dict.total_length() - expected_downloaded
      );

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
