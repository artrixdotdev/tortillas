#[cfg(feature = "live")]
use std::net::SocketAddr;
#[cfg(feature = "live")]
use std::sync::Weak;
use std::{fmt, path::PathBuf, sync::Arc};

use kameo::actor::ActorRef;
use tokio::sync::oneshot;
use tracing::error;

use super::{
   PieceStorageStrategy, TorrentActor, TorrentSnapshot, TorrentState,
   commands::{
      ForceReannounce, GetState, ReadyHook, SetAutoStart, SetOutputPath, SetPieceManager,
      SetPieceStorage, SetState, SetSufficientPeers, SnapshotState,
   },
};
#[cfg(feature = "live")]
use crate::live::{
   EventSubscription, Hub, HubInner, LivePublisher, PeerHandle, TorrentEventKind, TorrentListener,
   TorrentView, TrackerHandle,
};
#[cfg(feature = "live")]
use crate::{
   errors::map_torrent_communication_error,
   peer::WirePeer,
   torrent::commands::{AddPeer, AddTracker, DisconnectPeer, ReannounceTracker, RemoveTracker},
   tracker::Tracker,
};
use crate::{
   errors::{TorrentError, map_torrent_send_error},
   hashes::InfoHash,
   pieces::PieceManager,
};

#[derive(Debug)]
pub(crate) struct TorrentInner {
   pub(crate) info_hash: InfoHash,
   pub(crate) actor: ActorRef<TorrentActor>,
   #[cfg(feature = "live")]
   pub(crate) hub: Weak<HubInner>,
   #[cfg(feature = "live")]
   pub(crate) publisher: Arc<LivePublisher<Option<TorrentView>, TorrentEventKind>>,
}

/// A handle to a torrent managed by the engine.
///
/// This struct acts as the primary interface for controlling, observing, and
/// configuring a torrent after it has been added to the
/// [`Engine`](crate::engine::Engine).
#[derive(Clone)]
pub struct Torrent {
   pub(crate) inner: Arc<TorrentInner>,
}

impl fmt::Debug for Torrent {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter
         .debug_struct("Torrent")
         .field("info_hash", &self.info_hash())
         .finish_non_exhaustive()
   }
}

impl Torrent {
   pub(crate) fn actor(&self) -> &ActorRef<TorrentActor> {
      &self.inner.actor
   }

   /// Returns the [`InfoHash`] that uniquely identifies this torrent.
   pub fn info_hash(&self) -> InfoHash {
      self.inner.info_hash
   }

   pub async fn set_piece_storage(
      &self, piece_storage: PieceStorageStrategy,
   ) -> Result<(), TorrentError> {
      self
         .actor()
         .ask(SetPieceStorage {
            strategy: piece_storage,
         })
         .await
         .map_err(|error| map_torrent_send_error("set piece storage", error))?;
      Ok(())
   }

   /// Sets the output folder used by the default file piece manager.
   pub async fn set_output_folder(&self, folder: impl Into<PathBuf>) -> Result<(), TorrentError> {
      self
         .actor()
         .ask(SetOutputPath {
            path: folder.into(),
         })
         .await
         .map_err(|error| map_torrent_send_error("set output folder", error))?;
      Ok(())
   }

   pub async fn set_piece_manager<'a>(
      &'a self, piece_manager: impl PieceManager + 'a + 'static,
   ) -> Result<(), TorrentError> {
      self
         .actor()
         .ask(SetPieceManager {
            manager: Box::new(piece_manager),
         })
         .await
         .map_err(|error| map_torrent_send_error("set piece manager", error))?;
      Ok(())
   }

   pub async fn start(&self) -> Result<(), TorrentError> {
      self.set_state(TorrentState::Downloading, "start").await
   }

   /// Resumes downloading or seeding this torrent.
   pub async fn resume(&self) -> Result<(), TorrentError> {
      self.start().await
   }

   /// Pauses this torrent while preserving its downloaded data and metadata.
   pub async fn pause(&self) -> Result<(), TorrentError> {
      self.set_state(TorrentState::Paused, "pause").await
   }

   /// Stops this torrent's active transfers.
   pub async fn stop(&self) -> Result<(), TorrentError> {
      self.set_state(TorrentState::Paused, "stop").await
   }

   async fn set_state(
      &self, state: TorrentState, operation: &'static str,
   ) -> Result<(), TorrentError> {
      self
         .actor()
         .ask(SetState { state })
         .await
         .inspect_err(|error| {
            error!(%error, operation, "Failed to change torrent state");
         })
         .map_err(|error| map_torrent_send_error(operation, error))?;
      Ok(())
   }

   pub async fn state(&self) -> Result<TorrentState, TorrentError> {
      self
         .actor()
         .ask(GetState)
         .await
         .map_err(|error| map_torrent_send_error("get state", error))
   }

   /// Captures this torrent's metadata, storage configuration, and verified or
   /// partial piece state in a Serde-compatible persistence snapshot.
   ///
   /// With the `live` feature, use the torrent listener for current state and
   /// incremental updates.
   pub async fn snapshot(&self) -> Result<TorrentSnapshot, TorrentError> {
      self
         .actor()
         .ask(SnapshotState)
         .await
         .map(|snapshot| *snapshot)
         .map_err(|error| map_torrent_send_error("snapshot torrent", error))
   }

   pub async fn set_auto_start(&self, auto: bool) -> Result<(), TorrentError> {
      self
         .actor()
         .ask(SetAutoStart { auto })
         .await
         .map_err(|error| map_torrent_send_error("set auto start", error))?;
      Ok(())
   }

   pub async fn set_sufficient_peers(&self, peers: usize) -> Result<(), TorrentError> {
      self
         .actor()
         .ask(SetSufficientPeers { peers })
         .await
         .map_err(|error| map_torrent_send_error("set sufficient peers", error))?;
      Ok(())
   }

   pub async fn poll_ready(&self) -> Result<(), TorrentError> {
      let (hook, hook_rx) = oneshot::channel();
      self
         .actor()
         .ask(ReadyHook { hook })
         .await
         .map_err(|error| map_torrent_send_error("register ready hook", error))?;
      hook_rx
         .await
         .map_err(|error| TorrentError::ActorCommunicationFailed {
            operation: "wait for readiness",
            reason: error.to_string(),
         })?;
      Ok(())
   }

   /// Queues an immediate announce on every configured tracker.
   ///
   /// Every tracker is attempted even when another command delivery fails.
   /// `Ok` returns the number of trackers that accepted the command. Tracker
   /// responses remain asynchronous and are reported through their live views
   /// and event streams.
   pub async fn force_reannounce(&self) -> Result<usize, TorrentError> {
      self
         .actor()
         .ask(ForceReannounce)
         .await
         .map_err(|error| map_torrent_send_error("force tracker reannounce", error))
   }
}

#[cfg(all(test, feature = "live"))]
mod tests {
   use std::time::Duration;

   use tokio::time::timeout;

   use crate::{
      engine::{Engine, TorrentSource},
      errors::TorrentError,
      live::{PeerEventKind, TrackerEventKind, TrackerStatus},
      peer::PeerId,
      settings::Settings,
      testing::{self, LocalHttpTracker, LocalPeer},
      tracker::Tracker,
   };

   fn deterministic_engine() -> Engine {
      let mut settings = Settings::default();
      settings.dht.enabled = false;
      Engine::builder()
         .settings(settings)
         .autostart(false)
         .build()
   }

   async fn trackerless_source() -> TorrentSource {
      let mut metainfo = testing::read_torrent_fixture(testing::BIG_BUCK_BUNNY_TORRENT_FILE).await;
      metainfo.clear_announce_list();
      TorrentSource::torrent_file_bytes(serde_bencode::to_bytes(&metainfo).unwrap())
   }

   #[tokio::test]
   async fn torrent_handle_manages_manual_peer_lifecycle() {
      let engine = deterministic_engine();
      let torrent = engine
         .add_torrent(trackerless_source().await)
         .await
         .unwrap();
      let remote_id = PeerId::Unknown([42; 20]);
      let local_peer = LocalPeer::start(remote_id, Vec::new()).await.unwrap();

      let peer = torrent
         .add_peer(local_peer.peer().socket_addr())
         .await
         .unwrap();
      let mut listener = peer.listener();

      assert_eq!(peer.id(), remote_id);
      assert!(peer.view().connected);
      assert_eq!(torrent.peers(), vec![peer.clone()]);

      torrent.disconnect_peer(&peer).await.unwrap();

      assert!(!listener.view().connected);
      assert_eq!(
         timeout(Duration::from_secs(2), listener.recv())
            .await
            .expect("peer disconnected event timed out")
            .unwrap()
            .kind,
         PeerEventKind::Disconnected
      );
      assert!(torrent.peers().is_empty());
      assert!(matches!(
         torrent.disconnect_peer(&peer).await,
         Err(TorrentError::PeerNotFound { peer_id }) if peer_id == remote_id
      ));

      engine.shutdown().await.unwrap();
   }

   #[tokio::test]
   async fn torrent_handle_manages_tracker_and_reannounce_lifecycle() {
      let engine = deterministic_engine();
      let torrent = engine
         .add_torrent(trackerless_source().await)
         .await
         .unwrap();
      let local_tracker = LocalHttpTracker::start([]).await.unwrap();
      let source = Tracker::Http(local_tracker.uri());

      assert!(matches!(
         torrent
            .add_tracker(Tracker::Websocket(
               "wss://tracker.example/announce".to_string()
            ))
            .await,
         Err(TorrentError::UnsupportedTrackerProtocol {
            protocol: "websocket"
         })
      ));
      let tracker = timeout(Duration::from_secs(2), torrent.add_tracker(source.clone()))
         .await
         .expect("tracker initialization timed out")
         .unwrap();
      let mut listener = tracker.listener();

      assert_eq!(tracker.view().status, TrackerStatus::Pending);
      assert_eq!(torrent.trackers(), vec![tracker.clone()]);
      assert!(matches!(
         timeout(Duration::from_secs(2), torrent.add_tracker(source))
            .await
            .expect("duplicate tracker check timed out"),
         Err(TorrentError::TrackerAlreadyExists { .. })
      ));

      timeout(Duration::from_secs(2), torrent.reannounce_tracker(&tracker))
         .await
         .expect("tracker reannounce timed out")
         .unwrap();
      let announce = timeout(Duration::from_secs(2), listener.recv())
         .await
         .unwrap()
         .unwrap();
      assert!(matches!(
         announce.kind,
         TrackerEventKind::AnnounceSucceeded { peers_returned: 0 }
      ));
      assert_eq!(
         timeout(Duration::from_secs(2), torrent.force_reannounce())
            .await
            .expect("torrent reannounce timed out")
            .unwrap(),
         1
      );
      let announce = timeout(Duration::from_secs(2), listener.recv())
         .await
         .unwrap()
         .unwrap();
      assert!(matches!(
         announce.kind,
         TrackerEventKind::AnnounceSucceeded { peers_returned: 0 }
      ));
      assert_eq!(listener.view().status, TrackerStatus::Healthy);
      assert!(local_tracker.requests().await.len() >= 2);

      drop(local_tracker);
      timeout(Duration::from_secs(2), torrent.reannounce_tracker(&tracker))
         .await
         .expect("failed tracker reannounce timed out")
         .unwrap();
      assert!(matches!(
         timeout(Duration::from_secs(2), listener.recv())
            .await
            .unwrap()
            .unwrap()
            .kind,
         TrackerEventKind::AnnounceFailed
      ));
      assert_eq!(listener.view().status, TrackerStatus::Degraded);

      timeout(Duration::from_secs(2), torrent.remove_tracker(&tracker))
         .await
         .expect("tracker removal timed out")
         .unwrap();

      assert_eq!(listener.view().status, TrackerStatus::Stopped);
      assert!(torrent.trackers().is_empty());
      assert!(matches!(
         torrent.reannounce_tracker(&tracker).await,
         Err(TorrentError::TrackerNotFound { .. })
      ));

      engine.shutdown().await.unwrap();
   }
}

#[cfg(not(feature = "live"))]
impl Torrent {
   pub(crate) fn new(info_hash: InfoHash, actor: ActorRef<TorrentActor>) -> Self {
      Self {
         inner: Arc::new(TorrentInner { info_hash, actor }),
      }
   }
}

#[cfg(feature = "live")]
impl Torrent {
   async fn await_delegated<T>(
      result: oneshot::Receiver<Result<T, TorrentError>>, operation: &'static str,
   ) -> Result<T, TorrentError> {
      result
         .await
         .map_err(|error| TorrentError::ActorCommunicationFailed {
            operation,
            reason: error.to_string(),
         })?
   }

   #[cfg(test)]
   pub(crate) fn new(info_hash: InfoHash, actor_ref: ActorRef<TorrentActor>) -> Self {
      Self::new_with_hub(info_hash, actor_ref, &Hub::default(), None)
   }

   pub(crate) fn new_with_hub(
      info_hash: InfoHash, actor: ActorRef<TorrentActor>, hub: &Hub,
      initial_view: Option<TorrentView>,
   ) -> Self {
      let scope = hub
         .ensure_torrent_scope(info_hash)
         .expect("torrent handles require a live engine hub");
      if let Some(view) = initial_view {
         let _ = scope.publisher.install_initial_view(view);
      }
      let inner = Arc::new(TorrentInner {
         info_hash,
         actor,
         hub: hub.downgrade(),
         publisher: Arc::clone(&scope.publisher),
      });
      Self { inner }
   }

   /// Connects and handshakes with a manually supplied peer.
   ///
   /// A successful result guarantees that the handshake completed and the
   /// peer was registered with this torrent. Peer actor initialization then
   /// follows the same asynchronous path as discovered peers.
   pub async fn add_peer(&self, address: SocketAddr) -> Result<PeerHandle, TorrentError> {
      let result = self
         .actor()
         .ask(AddPeer {
            peer: WirePeer::from_socket_addr(address),
         })
         .await
         .map_err(|error| map_torrent_communication_error("add peer", error))?;
      Self::await_delegated(result, "add peer").await
   }

   /// Disconnects a peer.
   ///
   /// `Ok` guarantees that the peer is no longer registered in the active
   /// swarm, its actor has been told to stop, and its terminal live event has
   /// been published.
   pub async fn disconnect_peer(&self, peer: &PeerHandle) -> Result<(), TorrentError> {
      self.ensure_torrent_identity(peer.torrent(), "disconnect peer")?;
      self
         .actor()
         .ask(DisconnectPeer {
            id: peer.id(),
            handle: peer.clone(),
         })
         .await
         .map_err(|error| map_torrent_send_error("disconnect peer", error))
   }

   /// Adds a tracker and starts its actor.
   ///
   /// `Ok` guarantees that the tracker was accepted and registered. Actor
   /// initialization and announces are asynchronous; use its listener or
   /// [`TrackerHandle::view`] to observe their result.
   pub async fn add_tracker(&self, tracker: Tracker) -> Result<TrackerHandle, TorrentError> {
      self
         .actor()
         .ask(AddTracker { tracker })
         .await
         .map_err(|error| map_torrent_send_error("add tracker", error))
   }

   /// Removes a tracker and publishes its terminal event.
   ///
   /// `Ok` guarantees that the tracker is no longer configured and its live
   /// scope is terminal. Its final stopped announce is best-effort and may
   /// finish after this method returns.
   pub async fn remove_tracker(&self, tracker: &TrackerHandle) -> Result<(), TorrentError> {
      let source = self.tracker_source(tracker, "remove tracker")?;
      self
         .actor()
         .ask(RemoveTracker { tracker: source })
         .await
         .map_err(|error| map_torrent_send_error("remove tracker", error))
   }

   /// Queues an immediate announce on one tracker.
   ///
   /// `Ok` guarantees command delivery. The tracker response is asynchronous
   /// and updates the handle's view and event stream.
   pub async fn reannounce_tracker(&self, tracker: &TrackerHandle) -> Result<(), TorrentError> {
      let source = self.tracker_source(tracker, "reannounce tracker")?;
      self
         .actor()
         .ask(ReannounceTracker { tracker: source })
         .await
         .map_err(|error| map_torrent_send_error("reannounce tracker", error))
   }

   /// Subscribes to live events for this torrent only.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription<TorrentEventKind> {
      self.inner.publisher.subscribe()
   }

   /// Creates a live listener scoped to this torrent.
   #[must_use]
   pub fn listener(&self) -> TorrentListener {
      self.inner.publisher.listener()
   }

   /// Returns the latest state maintained for this torrent.
   ///
   /// This returns `None` after the torrent has been removed from its engine.
   #[must_use]
   pub fn view(&self) -> Option<TorrentView> {
      self.inner.publisher.view()
   }

   /// Returns handles for this torrent's currently connected peers.
   #[must_use]
   pub fn peers(&self) -> Vec<PeerHandle> {
      self
         .hub()
         .map_or_else(Vec::new, |hub| hub.peer_handles(self.info_hash()))
   }

   /// Returns handles for this torrent's configured trackers.
   #[must_use]
   pub fn trackers(&self) -> Vec<TrackerHandle> {
      self
         .hub()
         .map_or_else(Vec::new, |live| live.tracker_handles(self.info_hash()))
   }

   fn hub(&self) -> Option<Hub> {
      self.inner.hub.upgrade().map(Hub::from_inner)
   }

   fn ensure_torrent_identity(
      &self, torrent: InfoHash, operation: &'static str,
   ) -> Result<(), TorrentError> {
      if torrent == self.info_hash() {
         return Ok(());
      }
      Err(TorrentError::InvalidOperation {
         operation,
         reason: "the handle belongs to a different torrent".to_string(),
      })
   }

   fn tracker_source(
      &self, tracker: &TrackerHandle, operation: &'static str,
   ) -> Result<Tracker, TorrentError> {
      self.ensure_torrent_identity(tracker.torrent(), operation)?;
      self
         .hub()
         .and_then(|hub| hub.tracker_source(tracker))
         .ok_or_else(|| TorrentError::TrackerNotFound {
            endpoint: tracker.endpoint(),
         })
   }
}
