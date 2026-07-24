use std::{
   collections::HashMap,
   hash::Hash,
   sync::{
      Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak,
      atomic::{AtomicU64, Ordering},
   },
};

use tokio::sync::broadcast;

use super::{
   CoreEventKind, EngineView, EventListener, EventSubscription, FrontendHealth,
   FrontendHealthLevel, PeerEventKind, PeerHandle, PeerView, Sequenced, TorrentEventKind,
   TorrentView, TrackerEventKind, TrackerHandle, TrackerView,
   handle::{LiveHandle, PeerScope, TrackerId, TrackerScope},
};
use crate::{
   engine::EngineStatus,
   hashes::InfoHash,
   torrent::{Torrent, TorrentInner, TorrentState},
};

/// Number of discrete frontend events retained by each live publisher.
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

fn mutex_lock<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
   lock
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Generic current-state and event publisher for live application APIs.
///
/// The same primitive backs engine, torrent, peer, and tracker listeners. It
/// can also be reused by future protocol integrations without introducing
/// another channel or listener implementation.
#[derive(Debug, Clone)]
pub struct LivePublisher<V, E> {
   state: Arc<Mutex<LiveState<V>>>,
   channel: Arc<LiveChannel<E>>,
}

#[derive(Debug)]
struct LiveState<V> {
   view: V,
   sequence: u64,
   closed: bool,
}

#[derive(Debug)]
struct LiveChannel<E> {
   sender: Mutex<Option<broadcast::Sender<Sequenced<E>>>>,
}

impl<V, E> LivePublisher<V, E>
where
   V: Clone + Send + Sync + 'static,
   E: Clone + Send + 'static,
{
   /// Creates a publisher with an initial view and bounded event capacity.
   ///
   /// # Panics
   ///
   /// Panics when `event_capacity` is zero.
   #[must_use]
   pub fn new(initial_view: V, event_capacity: usize) -> Self {
      assert!(event_capacity > 0, "event capacity must be non-zero");
      let (events, _) = broadcast::channel(event_capacity);
      Self {
         state: Arc::new(Mutex::new(LiveState {
            view: initial_view,
            sequence: 0,
            closed: false,
         })),
         channel: Arc::new(LiveChannel {
            sender: Mutex::new(Some(events)),
         }),
      }
   }

   /// Subscribes to all future events from this publisher.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription<E> {
      let sender = mutex_lock(&self.channel.sender);
      match sender.as_ref() {
         Some(sender) => EventSubscription::from_receiver(sender.subscribe(), sender.downgrade()),
         None => {
            let (sender, receiver) = broadcast::channel(1);
            let weak = sender.downgrade();
            drop(sender);
            EventSubscription::from_receiver(receiver, weak)
         }
      }
   }

   /// Creates a stream-compatible listener paired with the current view.
   #[must_use]
   pub fn listener(&self) -> EventListener<V, E> {
      let state = Arc::clone(&self.state);
      EventListener::new(self.subscribe(), move || mutex_lock(&state).view.clone())
   }

   /// Clones the latest coherent view.
   #[must_use]
   pub fn view(&self) -> V {
      mutex_lock(&self.state).view.clone()
   }

   /// Replaces the current view without emitting an event.
   ///
   /// Returns `false` when the publisher has already closed.
   pub fn set_view(&self, view: V) -> bool {
      let mut state = mutex_lock(&self.state);
      if state.closed {
         return false;
      }
      state.view = view;
      true
   }

   /// Replaces the current view and emits the corresponding event.
   ///
   /// Returns `false` when the publisher has already closed.
   pub fn update(&self, view: V, event: E) -> bool {
      self.mutate(|current| *current = view, event)
   }

   /// Emits an event using this publisher's monotonic sequence.
   ///
   /// Returns `false` when the publisher has already closed.
   pub fn publish(&self, kind: E) -> bool {
      self.mutate(|_| {}, kind)
   }

   /// Atomically updates the view and permanently closes this publisher after
   /// delivering one terminal event.
   ///
   /// Returns `false` if another caller already closed the publisher.
   pub fn close(&self, view: V, event: E) -> bool {
      let mut state = mutex_lock(&self.state);
      if state.closed {
         return false;
      }
      state.view = view;
      state.sequence = state.sequence.saturating_add(1);
      state.closed = true;
      let mut sender = mutex_lock(&self.channel.sender);
      if let Some(sender) = sender.take() {
         let _ = sender.send(Sequenced {
            sequence: state.sequence,
            kind: event,
         });
      }
      true
   }

   pub(crate) fn edit_and_publish(&self, edit: impl FnOnce(&mut V) -> E) -> bool {
      let mut state = mutex_lock(&self.state);
      if state.closed {
         return false;
      }
      let event = edit(&mut state.view);
      state.sequence = state.sequence.saturating_add(1);
      self.send(&state, event);
      true
   }

   pub(crate) fn edit_if_and_publish(&self, edit: impl FnOnce(&mut V) -> bool, event: E) -> bool {
      let mut state = mutex_lock(&self.state);
      if state.closed || !edit(&mut state.view) {
         return false;
      }
      state.sequence = state.sequence.saturating_add(1);
      self.send(&state, event);
      true
   }

   pub(crate) fn edit_view<R>(&self, edit: impl FnOnce(&mut V) -> R) -> Option<R> {
      let mut state = mutex_lock(&self.state);
      (!state.closed).then(|| edit(&mut state.view))
   }

   fn mutate(&self, edit: impl FnOnce(&mut V), event: E) -> bool {
      let mut state = mutex_lock(&self.state);
      if state.closed {
         return false;
      }
      edit(&mut state.view);
      state.sequence = state.sequence.saturating_add(1);
      self.send(&state, event);
      true
   }

   fn send(&self, state: &LiveState<V>, event: E) {
      if let Some(sender) = mutex_lock(&self.channel.sender).as_ref() {
         let _ = sender.send(Sequenced {
            sequence: state.sequence,
            kind: event,
         });
      }
   }
}

#[derive(Debug)]
struct ScopeRegistry<K, V> {
   values: RwLock<HashMap<K, Arc<V>>>,
}

impl<K, V> ScopeRegistry<K, V>
where
   K: Copy + Eq + Hash,
{
   fn new() -> Self {
      Self {
         values: RwLock::new(HashMap::new()),
      }
   }

   fn insert(&self, key: K, value: &Arc<V>) {
      write_lock(&self.values).insert(key, Arc::clone(value));
   }

   fn get(&self, key: &K) -> Option<Arc<V>> {
      read_lock(&self.values).get(key).cloned()
   }

   fn remove(&self, key: &K) -> Option<Arc<V>> {
      write_lock(&self.values).remove(key)
   }

   fn values(&self) -> Vec<Arc<V>> {
      read_lock(&self.values).values().cloned().collect()
   }

   fn retain(&self, keep: impl Fn(K) -> bool) {
      write_lock(&self.values).retain(|key, _| keep(*key));
   }
}

/// Shared live-state hub used by the engine actor hierarchy.
#[derive(Debug)]
pub(crate) struct FrontendHub {
   live: LivePublisher<EngineView, CoreEventKind>,
   torrents: ScopeRegistry<InfoHash, TorrentInner>,
   peers: ScopeRegistry<PeerScope, LiveHandle<PeerScope, PeerView, PeerEventKind>>,
   trackers: ScopeRegistry<TrackerScope, LiveHandle<TrackerScope, TrackerView, TrackerEventKind>>,
   next_tracker_id: AtomicU64,
}

#[derive(Debug, Clone)]
enum HubReference {
   Strong(Arc<FrontendHub>),
   Weak(Weak<FrontendHub>),
}

/// Cloneable access to one frontend hub.
#[derive(Debug, Clone)]
pub(crate) struct FrontendPublisher {
   hub: HubReference,
}

impl FrontendPublisher {
   pub(crate) fn new() -> Self {
      Self::with_event_capacity(DEFAULT_EVENT_CAPACITY)
   }

   fn with_event_capacity(event_capacity: usize) -> Self {
      Self {
         hub: HubReference::Strong(Arc::new(FrontendHub {
            live: LivePublisher::new(
               EngineView {
                  status: EngineStatus::Starting,
                  torrent_count: 0,
                  torrents: Vec::new(),
               },
               event_capacity,
            ),
            torrents: ScopeRegistry::new(),
            peers: ScopeRegistry::new(),
            trackers: ScopeRegistry::new(),
            next_tracker_id: AtomicU64::new(1),
         })),
      }
   }

   pub(crate) fn from_hub(hub: Arc<FrontendHub>) -> Self {
      Self {
         hub: HubReference::Strong(hub),
      }
   }

   pub(crate) fn weak(&self) -> Self {
      Self {
         hub: HubReference::Weak(self.downgrade()),
      }
   }

   pub(crate) fn downgrade(&self) -> Weak<FrontendHub> {
      match &self.hub {
         HubReference::Strong(hub) => Arc::downgrade(hub),
         HubReference::Weak(hub) => hub.clone(),
      }
   }

   fn hub(&self) -> Arc<FrontendHub> {
      match &self.hub {
         HubReference::Strong(hub) => Arc::clone(hub),
         HubReference::Weak(hub) => hub
            .upgrade()
            .expect("frontend hub outlived by its actor hierarchy"),
      }
   }

   pub(crate) fn subscribe(&self) -> EventSubscription {
      self.hub().live.subscribe()
   }

   pub(crate) fn view(&self) -> EngineView {
      self.hub().live.view()
   }

   pub(crate) fn torrent_view(&self, torrent: InfoHash) -> Option<TorrentView> {
      self
         .view()
         .torrents
         .into_iter()
         .find(|view| view.info_hash == torrent)
   }

   pub(crate) fn torrent_handle(&self, torrent: InfoHash) -> Option<Torrent> {
      self
         .hub()
         .torrents
         .get(&torrent)
         .map(|inner| Torrent { inner })
   }

   pub(crate) fn peer_handles(&self, torrent: InfoHash) -> Vec<PeerHandle> {
      self
         .hub()
         .peers
         .values()
         .into_iter()
         .map(|inner| PeerHandle { inner })
         .filter(|peer| peer.torrent() == torrent && peer.live_view().connected)
         .collect()
   }

   pub(crate) fn tracker_handles(&self, torrent: InfoHash) -> Vec<TrackerHandle> {
      self
         .hub()
         .trackers
         .values()
         .into_iter()
         .map(|inner| TrackerHandle { inner })
         .filter(|tracker| tracker.torrent() == torrent)
         .collect()
   }

   pub(crate) fn engine_started(&self) {
      self.hub().live.edit_and_publish(|view| {
         view.status = EngineStatus::Running;
         CoreEventKind::EngineStarted(view.clone())
      });
   }

   pub(crate) fn engine_stopping(&self) {
      let _ = self.hub().live.edit_view(|view| {
         view.status = EngineStatus::Stopping;
      });
   }

   pub(crate) fn engine_stopped(&self) {
      let mut view = self.view();
      view.status = EngineStatus::Stopped;
      self
         .hub()
         .live
         .close(view.clone(), CoreEventKind::Shutdown(view));
   }

   pub(crate) fn initialize_torrent(&self, torrent: TorrentView) {
      let _ = self.hub().live.edit_view(|view| {
         Self::replace_torrent_view(view, torrent);
      });
   }

   pub(crate) fn torrent_added(&self, torrent: Torrent) {
      let _routing = torrent.routing_lock();
      self
         .hub()
         .torrents
         .insert(torrent.info_hash(), &torrent.inner);
      if let Some(view) = torrent.live_view() {
         self.hub().live.edit_and_publish(|engine| {
            Self::replace_torrent_view(engine, view);
            CoreEventKind::Torrent {
               torrent: torrent.clone(),
               event: TorrentEventKind::Added,
            }
         });
      }
   }

   pub(crate) fn update_torrent(&self, torrent: TorrentView) {
      self.publish_torrent(torrent, TorrentEventKind::Updated);
   }

   pub(crate) fn metadata_resolved(&self, torrent: TorrentView) {
      self.publish_torrent(torrent, TorrentEventKind::MetadataResolved);
   }

   pub(crate) fn progress_changed(&self, torrent: TorrentView) {
      let progress = torrent.progress.clone();
      self.publish_torrent(torrent, TorrentEventKind::ProgressChanged(progress));
   }

   pub(crate) fn peer(&self, scope: PeerScope, view: PeerView) -> PeerHandle {
      let peer = PeerHandle::new(scope, view, self.downgrade());
      self.hub().peers.insert(scope, &peer.inner);
      peer
   }

   pub(crate) fn peer_connected(&self, torrent: TorrentView, peer: &PeerHandle) {
      self.publish_torrent(torrent, TorrentEventKind::PeerConnected(peer.clone()));
   }

   pub(crate) fn peer_updated(&self, peer: &PeerHandle) {
      if self.hub().peers.get(&peer.scope()).is_none() {
         return;
      }
      if let Some(view) = self.torrent_view(peer.torrent()) {
         self.publish_torrent(view, TorrentEventKind::PeerUpdated(peer.clone()));
      }
   }

   pub(crate) fn peer_disconnected(&self, peer: &PeerHandle, torrent: Option<TorrentView>) {
      if self.hub().peers.remove(&peer.scope()).is_none() {
         return;
      }
      if let Some(view) = torrent {
         self.publish_torrent(view, TorrentEventKind::PeerDisconnected(peer.clone()));
      }
   }

   pub(crate) fn tracker(&self, torrent: InfoHash, view: TrackerView) -> TrackerHandle {
      let id = TrackerId::new(self.hub().next_tracker_id.fetch_add(1, Ordering::Relaxed));
      let scope = TrackerScope { torrent, id };
      let tracker = TrackerHandle::new(scope, view, self.downgrade());
      self.hub().trackers.insert(scope, &tracker.inner);
      tracker
   }

   pub(crate) fn tracker_event(&self, tracker: &TrackerHandle, event: TrackerEventKind) {
      if self.hub().trackers.get(&tracker.scope()).is_none() {
         return;
      }
      let torrent_event = match event {
         TrackerEventKind::AnnounceSucceeded { .. } => {
            TorrentEventKind::TrackerAnnounceSucceeded(tracker.clone())
         }
         TrackerEventKind::AnnounceFailed => {
            TorrentEventKind::TrackerAnnounceFailed(tracker.clone())
         }
         TrackerEventKind::Stopped => TorrentEventKind::TrackerStopped(tracker.clone()),
      };
      if let Some(view) = self.torrent_view(tracker.torrent()) {
         self.publish_torrent(view, torrent_event);
      }
   }

   pub(crate) fn health(
      &self, torrent: Option<InfoHash>, level: FrontendHealthLevel, message: impl Into<String>,
   ) {
      let health = FrontendHealth {
         torrent,
         level,
         message: message.into(),
      };
      if let Some(info_hash) = torrent
         && let Some(view) = self.torrent_view(info_hash)
      {
         self.publish_torrent(view, TorrentEventKind::Health(health));
      } else {
         self.hub().live.publish(CoreEventKind::Health(health));
      }
   }

   pub(crate) fn torrent_state_changed(&self, previous: TorrentState, torrent: TorrentView) {
      let current = torrent.state;
      self.publish_torrent(
         torrent,
         TorrentEventKind::StateChanged { previous, current },
      );
   }

   pub(crate) fn torrent_removed(&self, info_hash: InfoHash) {
      let removed = self
         .hub()
         .torrents
         .remove(&info_hash)
         .map(|inner| Torrent { inner });

      for peer in self
         .peer_handles(info_hash)
         .into_iter()
         .filter(|peer| peer.live_view().connected)
      {
         peer.disconnected(None);
      }
      self.hub().peers.retain(|scope| scope.torrent != info_hash);

      for tracker in self
         .tracker_handles(info_hash)
         .into_iter()
         .filter(|tracker| tracker.live_view().status.is_active())
      {
         tracker.stopped();
      }
      self
         .hub()
         .trackers
         .retain(|scope| scope.torrent != info_hash);

      let Some(torrent) = removed else {
         let _ = self.hub().live.edit_view(|view| {
            Self::remove_torrent_view(view, info_hash);
         });
         return;
      };

      let _routing = torrent.routing_lock();
      if torrent.removed() {
         self.hub().live.edit_and_publish(|view| {
            Self::remove_torrent_view(view, info_hash);
            CoreEventKind::Torrent {
               torrent: torrent.clone(),
               event: TorrentEventKind::Removed,
            }
         });
      }
   }

   fn publish_torrent(&self, view: TorrentView, event: TorrentEventKind) {
      let Some(torrent) = self.torrent_handle(view.info_hash) else {
         return;
      };
      let _routing = torrent.routing_lock();
      if !torrent.publish(view.clone(), event.clone()) {
         return;
      }
      self.hub().live.edit_if_and_publish(
         |engine| {
            let Some(current) = engine
               .torrents
               .iter_mut()
               .find(|candidate| candidate.info_hash == view.info_hash)
            else {
               return false;
            };
            *current = view;
            true
         },
         CoreEventKind::Torrent {
            torrent: torrent.clone(),
            event,
         },
      );
   }

   fn replace_torrent_view(view: &mut EngineView, torrent: TorrentView) {
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
   }

   fn remove_torrent_view(view: &mut EngineView, torrent: InfoHash) {
      view
         .torrents
         .retain(|candidate| candidate.info_hash != torrent);
      view.torrent_count = u64::try_from(view.torrents.len()).unwrap_or(u64::MAX);
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
      let peer = frontend.peer(scope, view.clone());
      let mut listener = peer.listener();
      let mut updated = view;
      updated.downloaded_bytes = 16;

      peer.update(updated);

      let event = listener.recv().await.unwrap();
      assert_eq!(event.kind, super::super::PeerEventKind::Updated);
      assert_eq!(listener.view().downloaded_bytes, 16);
   }

   #[tokio::test]
   async fn disconnected_peer_rejects_late_actor_updates() {
      let frontend = FrontendPublisher::new();
      let scope = PeerScope {
         torrent: InfoHash::from_bytes([1; 20]),
         peer: PeerId::Unknown([2; 20]),
      };
      let view = PeerView {
         address: None,
         client: None,
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
      let peer = frontend.peer(scope, view.clone());
      let mut listener = peer.listener();

      peer.disconnected(None);
      let mut late = view;
      late.downloaded_bytes = 32;
      peer.update(late);

      assert_eq!(
         listener.recv().await.unwrap().kind,
         PeerEventKind::Disconnected
      );
      assert_eq!(
         listener.recv().await,
         Err(super::super::EventStreamError::Closed)
      );
      assert!(!listener.view().connected);
      assert_eq!(listener.view().downloaded_bytes, 0);
   }

   #[test]
   fn live_handles_do_not_keep_their_frontend_hub_alive() {
      let frontend = FrontendPublisher::new();
      let hub = frontend.downgrade();
      let scope = PeerScope {
         torrent: InfoHash::from_bytes([1; 20]),
         peer: PeerId::Unknown([2; 20]),
      };
      let peer = frontend.peer(
         scope,
         PeerView {
            address: None,
            client: None,
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
         },
      );

      drop(frontend);

      assert!(hub.upgrade().is_none());
      assert!(peer.live_view().connected);
   }

   #[test]
   fn trackers_with_the_same_public_endpoint_keep_distinct_identities() {
      let frontend = FrontendPublisher::new();
      let torrent = InfoHash::from_bytes([3; 20]);
      let view = TrackerView {
         endpoint: "https://tracker.example".to_string(),
         status: super::super::TrackerStatus::Pending,
         peers_returned: None,
      };

      let first = frontend.tracker(torrent, view.clone());
      let second = frontend.tracker(torrent, view);

      assert_ne!(first.id(), second.id());
      assert_ne!(first, second);
      assert_eq!(frontend.tracker_handles(torrent).len(), 2);
   }

   #[tokio::test]
   async fn stopped_tracker_rejects_late_announces() {
      let frontend = FrontendPublisher::new();
      let tracker = frontend.tracker(
         InfoHash::from_bytes([3; 20]),
         TrackerView {
            endpoint: "https://tracker.example".to_string(),
            status: super::super::TrackerStatus::Pending,
            peers_returned: None,
         },
      );
      let mut listener = tracker.listener();

      tracker.stopped();
      tracker.announce_succeeded(10);

      assert_eq!(
         listener.recv().await.unwrap().kind,
         TrackerEventKind::Stopped
      );
      assert_eq!(
         listener.recv().await,
         Err(super::super::EventStreamError::Closed)
      );
      assert_eq!(listener.view().status, super::super::TrackerStatus::Stopped);
      assert_eq!(listener.view().peers_returned, None);
   }
}
