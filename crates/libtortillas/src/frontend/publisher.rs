use std::{
   collections::HashMap,
   sync::{
      Arc, RwLock, RwLockReadGuard, RwLockWriteGuard,
      atomic::{AtomicU64, Ordering},
   },
};

use tokio::sync::broadcast;

use super::{
   CoreEventKind, EngineView, EventListener, EventSubscription, FrontendHealth,
   FrontendHealthLevel, PeerHandle, PeerView, Sequenced, TorrentView, TrackerHandle, TrackerView,
   handle::{PeerScope, TrackerScope},
};
use crate::{
   engine::EngineStatus,
   hashes::InfoHash,
   torrent::{Torrent, TorrentState},
};

/// Number of discrete frontend events retained for each listener.
pub const DEFAULT_EVENT_CAPACITY: usize = 256;

fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
   lock
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_lock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
   lock
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Generic current-state and event publisher for live application APIs.
///
/// The same primitive backs engine, torrent, peer, and tracker listeners. It
/// can also be reused by future protocol integrations without introducing
/// another channel or listener implementation.
#[derive(Debug, Clone)]
pub struct LivePublisher<V, E> {
   inner: Arc<LivePublisherInner<V, E>>,
}

#[derive(Debug)]
struct LivePublisherInner<V, E> {
   events: broadcast::Sender<Sequenced<E>>,
   view: RwLock<V>,
   sequence: AtomicU64,
}

impl<V, E> LivePublisher<V, E>
where
   V: Clone + Send + Sync + 'static,
   E: Clone + Send + 'static,
{
   /// Creates a publisher with an initial view and bounded event capacity.
   #[must_use]
   pub fn new(initial_view: V, event_capacity: usize) -> Self {
      let (events, _) = broadcast::channel(event_capacity);
      Self {
         inner: Arc::new(LivePublisherInner {
            events,
            view: RwLock::new(initial_view),
            sequence: AtomicU64::new(0),
         }),
      }
   }

   /// Subscribes to all future events from this publisher.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription<E> {
      EventSubscription::new(self.inner.events.clone(), None)
   }

   pub(crate) fn subscribe_where(
      &self, filter: impl Fn(&E) -> bool + Send + Sync + 'static,
   ) -> EventSubscription<E> {
      EventSubscription::new(self.inner.events.clone(), Some(Arc::new(filter)))
   }

   /// Creates a stream-compatible listener paired with the current view.
   #[must_use]
   pub fn listener(&self) -> EventListener<V, E> {
      let publisher = self.clone();
      EventListener::new(self.subscribe(), move || publisher.view())
   }

   /// Clones the latest coherent view.
   #[must_use]
   pub fn view(&self) -> V {
      self.read_view().clone()
   }

   /// Replaces the current view without emitting an event.
   pub fn set_view(&self, view: V) {
      *self.write_view() = view;
   }

   /// Replaces the current view and emits the corresponding event.
   pub fn update(&self, view: V, event: E) {
      self.set_view(view);
      self.publish(event);
   }

   /// Emits an event using this publisher's monotonic sequence.
   pub fn publish(&self, kind: E) {
      let sequence = self.inner.sequence.fetch_add(1, Ordering::Relaxed) + 1;
      let _ = self.inner.events.send(Sequenced { sequence, kind });
   }

   pub(crate) fn edit_view<R>(&self, edit: impl FnOnce(&mut V) -> R) -> R {
      edit(&mut self.write_view())
   }

   fn read_view(&self) -> RwLockReadGuard<'_, V> {
      read_lock(&self.inner.view)
   }

   fn write_view(&self) -> RwLockWriteGuard<'_, V> {
      write_lock(&self.inner.view)
   }
}

/// Shared live-state publisher used by the engine actor hierarchy.
#[derive(Debug, Clone)]
pub(crate) struct FrontendPublisher {
   live: LivePublisher<EngineView, CoreEventKind>,
   torrents: Arc<RwLock<HashMap<InfoHash, Torrent>>>,
   peers: Arc<RwLock<HashMap<PeerScope, PeerView>>>,
   trackers: Arc<RwLock<HashMap<TrackerScope, TrackerView>>>,
}

impl FrontendPublisher {
   pub(crate) fn new() -> Self {
      Self::with_event_capacity(DEFAULT_EVENT_CAPACITY)
   }

   fn with_event_capacity(event_capacity: usize) -> Self {
      Self {
         live: LivePublisher::new(
            EngineView {
               status: EngineStatus::Starting,
               torrent_count: 0,
               torrents: Vec::new(),
            },
            event_capacity,
         ),
         torrents: Arc::new(RwLock::new(HashMap::new())),
         peers: Arc::new(RwLock::new(HashMap::new())),
         trackers: Arc::new(RwLock::new(HashMap::new())),
      }
   }

   pub(crate) fn subscribe(&self) -> EventSubscription {
      self.live.subscribe()
   }

   pub(crate) fn subscribe_torrent(&self, torrent: InfoHash) -> EventSubscription {
      self
         .live
         .subscribe_where(move |event| event.torrent() == Some(torrent))
   }

   pub(crate) fn subscribe_peer(&self, scope: PeerScope) -> EventSubscription {
      self.live.subscribe_where(move |event| event.is_peer(scope))
   }

   pub(crate) fn subscribe_tracker(&self, scope: TrackerScope) -> EventSubscription {
      self
         .live
         .subscribe_where(move |event| event.is_tracker(&scope))
   }

   pub(crate) fn view(&self) -> EngineView {
      self.live.view()
   }

   pub(crate) fn torrent_view(&self, torrent: InfoHash) -> Option<TorrentView> {
      self
         .live
         .view()
         .torrents
         .into_iter()
         .find(|view| view.info_hash == torrent)
   }

   pub(crate) fn peer_view(&self, scope: PeerScope) -> Option<PeerView> {
      read_lock(&self.peers).get(&scope).cloned()
   }

   pub(crate) fn tracker_view(&self, scope: &TrackerScope) -> Option<TrackerView> {
      read_lock(&self.trackers).get(scope).cloned()
   }

   pub(crate) fn peer_handles(&self, torrent: InfoHash) -> Vec<PeerHandle> {
      read_lock(&self.peers)
         .iter()
         .filter(|(scope, view)| scope.torrent == torrent && view.connected)
         .map(|(scope, _)| PeerHandle::new(*scope, self.clone()))
         .collect()
   }

   pub(crate) fn tracker_handles(&self, torrent: InfoHash) -> Vec<TrackerHandle> {
      read_lock(&self.trackers)
         .keys()
         .filter(|scope| scope.torrent == torrent)
         .map(|scope| TrackerHandle::new(scope.clone(), self.clone()))
         .collect()
   }

   pub(crate) fn engine_started(&self) {
      let view = self.set_engine_status(EngineStatus::Running);
      self.publish(CoreEventKind::EngineStarted(view));
   }

   pub(crate) fn engine_stopping(&self) {
      self.set_engine_status(EngineStatus::Stopping);
   }

   pub(crate) fn engine_stopped(&self) {
      let view = self.set_engine_status(EngineStatus::Stopped);
      self.publish(CoreEventKind::Shutdown(view));
   }

   pub(crate) fn initialize_torrent(&self, torrent: TorrentView) {
      self.replace_torrent(torrent);
   }

   pub(crate) fn torrent_added(&self, torrent: Torrent) {
      self
         .write_torrents()
         .insert(torrent.info_hash(), torrent.clone());
      self.publish(CoreEventKind::TorrentAdded(torrent));
   }

   pub(crate) fn update_torrent(&self, torrent: TorrentView) {
      if let Some(torrent) = self.update_torrent_entry(torrent) {
         self.publish(CoreEventKind::TorrentUpdated(torrent));
      }
   }

   pub(crate) fn metadata_resolved(&self, torrent: TorrentView) {
      if let Some(torrent) = self.update_torrent_entry(torrent) {
         self.publish(CoreEventKind::MetadataResolved(torrent));
      }
   }

   pub(crate) fn progress_changed(&self, torrent: TorrentView) {
      let info_hash = torrent.info_hash;
      let progress = torrent.progress.clone();
      if self.update_torrent_entry(torrent).is_some() {
         self.publish(CoreEventKind::ProgressChanged {
            torrent: info_hash,
            progress,
         });
      }
   }

   pub(crate) fn peer_connected(
      &self, torrent: TorrentView, scope: PeerScope, view: PeerView,
   ) -> PeerHandle {
      let info_hash = torrent.info_hash;
      write_lock(&self.peers).insert(scope, view);
      let peer = PeerHandle::new(scope, self.clone());
      if self.update_torrent_entry(torrent).is_some() {
         self.publish(CoreEventKind::PeerConnected {
            torrent: info_hash,
            peer: peer.clone(),
         });
      }
      peer
   }

   pub(crate) fn peer_updated(&self, peer: &PeerHandle, view: PeerView) {
      let scope = peer.scope();
      if let Some(current) = write_lock(&self.peers).get_mut(&scope) {
         *current = view;
         self.publish(CoreEventKind::PeerUpdated {
            torrent: scope.torrent,
            peer: peer.clone(),
         });
      }
   }

   pub(crate) fn peer_disconnected(&self, peer: PeerHandle) {
      let scope = peer.scope();
      if let Some(view) = write_lock(&self.peers).get_mut(&scope) {
         view.connected = false;
         self.publish(CoreEventKind::PeerDisconnected {
            torrent: scope.torrent,
            peer,
         });
      }
   }

   pub(crate) fn tracker(&self, scope: TrackerScope, view: TrackerView) -> TrackerHandle {
      write_lock(&self.trackers).insert(scope.clone(), view);
      TrackerHandle::new(scope, self.clone())
   }

   pub(crate) fn tracker_announce_succeeded(&self, tracker: &TrackerHandle, view: TrackerView) {
      let scope = tracker.scope();
      if let Some(current) = write_lock(&self.trackers).get_mut(scope) {
         *current = view;
         self.publish(CoreEventKind::TrackerAnnounceSucceeded {
            torrent: scope.torrent,
            tracker: tracker.clone(),
         });
      }
   }

   pub(crate) fn tracker_announce_failed(&self, tracker: &TrackerHandle, view: TrackerView) {
      let scope = tracker.scope();
      if let Some(current) = write_lock(&self.trackers).get_mut(scope) {
         *current = view;
         self.publish(CoreEventKind::TrackerAnnounceFailed {
            torrent: scope.torrent,
            tracker: tracker.clone(),
         });
      }
   }

   pub(crate) fn tracker_stopped(&self, tracker: &TrackerHandle) {
      let scope = tracker.scope();
      if let Some(view) = write_lock(&self.trackers).get_mut(scope) {
         view.active = false;
         self.publish(CoreEventKind::TrackerStopped {
            torrent: scope.torrent,
            tracker: tracker.clone(),
         });
      }
   }

   pub(crate) fn health(
      &self, torrent: Option<InfoHash>, level: FrontendHealthLevel, message: impl Into<String>,
   ) {
      self.publish(CoreEventKind::Health(FrontendHealth {
         torrent,
         level,
         message: message.into(),
      }));
   }

   pub(crate) fn torrent_state_changed(&self, previous: TorrentState, torrent: TorrentView) {
      let info_hash = torrent.info_hash;
      let current = torrent.state;
      if self.update_torrent_entry(torrent).is_some() {
         self.publish(CoreEventKind::TorrentStateChanged {
            torrent: info_hash,
            previous,
            current,
         });
      }
   }

   pub(crate) fn torrent_removed(&self, torrent: InfoHash) {
      self.live.edit_view(|view| {
         view
            .torrents
            .retain(|candidate| candidate.info_hash != torrent);
         view.torrent_count = u64::try_from(view.torrents.len()).unwrap_or(u64::MAX);
      });
      let peers = read_lock(&self.peers)
         .iter()
         .filter(|(scope, view)| scope.torrent == torrent && view.connected)
         .map(|(scope, _)| *scope)
         .collect::<Vec<_>>();
      for scope in peers {
         self.peer_disconnected(PeerHandle::new(scope, self.clone()));
      }
      write_lock(&self.peers).retain(|scope, _| scope.torrent != torrent);
      let trackers = read_lock(&self.trackers)
         .keys()
         .filter(|scope| scope.torrent == torrent)
         .cloned()
         .collect::<Vec<_>>();
      for scope in &trackers {
         self.tracker_stopped(&TrackerHandle::new(scope.clone(), self.clone()));
      }
      write_lock(&self.trackers).retain(|scope, _| scope.torrent != torrent);
      if let Some(torrent) = self.write_torrents().remove(&torrent) {
         self.publish(CoreEventKind::TorrentRemoved(torrent));
      }
   }

   pub(crate) fn publish(&self, kind: CoreEventKind) {
      self.live.publish(kind);
   }

   fn set_engine_status(&self, status: EngineStatus) -> EngineView {
      self.live.edit_view(|view| {
         view.status = status;
         view.clone()
      })
   }

   fn replace_torrent(&self, torrent: TorrentView) {
      self.live.edit_view(|view| {
         match view
            .torrents
            .iter_mut()
            .find(|candidate| candidate.info_hash == torrent.info_hash)
         {
            Some(current) => *current = torrent,
            None => view.torrents.push(torrent),
         }
         view
            .torrents
            .sort_by(|left, right| left.info_hash.as_bytes().cmp(right.info_hash.as_bytes()));
         view.torrent_count = u64::try_from(view.torrents.len()).unwrap_or(u64::MAX);
      });
   }

   fn update_torrent_entry(&self, torrent: TorrentView) -> Option<Torrent> {
      let info_hash = torrent.info_hash;
      let updated = self.live.edit_view(|view| {
         let Some(current) = view
            .torrents
            .iter_mut()
            .find(|candidate| candidate.info_hash == torrent.info_hash)
         else {
            return false;
         };
         *current = torrent;
         true
      });
      updated
         .then(|| self.read_torrents().get(&info_hash).cloned())
         .flatten()
   }

   fn read_torrents(&self) -> RwLockReadGuard<'_, HashMap<InfoHash, Torrent>> {
      read_lock(&self.torrents)
   }

   fn write_torrents(&self) -> RwLockWriteGuard<'_, HashMap<InfoHash, Torrent>> {
      write_lock(&self.torrents)
   }
}

impl Default for FrontendPublisher {
   fn default() -> Self {
      Self::new()
   }
}

#[cfg(test)]
mod tests {
   use std::net::{Ipv4Addr, SocketAddr};

   use super::*;
   use crate::peer::PeerId;

   #[tokio::test]
   async fn peer_handle_when_updated_then_only_its_listener_receives_event() {
      let frontend = FrontendPublisher::new();
      let scope = PeerScope {
         torrent: InfoHash::from_bytes([1; 20]),
         peer: PeerId::Unknown([2; 20]),
      };
      let view = PeerView {
         address: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 6881))),
         client: Some("Unknown".to_string()),
         connected: true,
         peer_choking: true,
         peer_interested: false,
         client_choking: true,
         client_interested: false,
         available_pieces: 0,
         download_rate_bytes_per_second: 0,
         upload_rate_bytes_per_second: 0,
         downloaded_bytes: 0,
         uploaded_bytes: 0,
      };
      write_lock(&frontend.peers).insert(scope, view.clone());
      let peer = PeerHandle::new(scope, frontend);
      let mut listener = peer.listener();
      let mut updated = view;
      updated.downloaded_bytes = 16;

      peer.update(updated);

      let event = listener.recv().await.unwrap();
      assert!(matches!(event.kind, CoreEventKind::PeerUpdated { .. }));
      assert_eq!(listener.view().unwrap().downloaded_bytes, 16);
   }
}
