//! Internal ownership and coordination for live projections.
//!
//! Read this file from top to bottom to follow the complete ownership path:
//! guard-free registry, scope tree, hub lifetime, then engine, torrent, peer,
//! and tracker publication operations.

use std::{
   hash::Hash,
   sync::{
      Arc, Mutex, MutexGuard, OnceLock, Weak,
      atomic::{AtomicBool, AtomicU64, Ordering},
   },
};

use dashmap::DashMap;

use super::{
   EngineEventKind, EngineView, EventSubscription, LiveHealth, LiveHealthLevel, LivePublisher,
   PeerEventKind, PeerHandle, PeerView, TorrentEventKind, TorrentView, TrackerEventKind,
   TrackerHandle, TrackerView,
   handle::{LiveScope, PeerIdentity, TrackerId, TrackerIdentity},
};
use crate::{
   engine::EngineStatus,
   hashes::InfoHash,
   live::LiveSettings,
   peer::PeerId,
   torrent::{Torrent, TorrentInner},
   tracker::Tracker,
};

// Registry

/// Guard-free facade over sharded keyed scope ownership.
///
/// Registry guards never escape this type: callers receive cloned `Arc`s or
/// owned vectors, so actor communication and async work cannot accidentally
/// retain a DashMap shard lock.
#[derive(Debug)]
struct ScopeRegistry<K: Eq + Hash, V> {
   values: DashMap<K, Arc<V>>,
}

impl<K, V> ScopeRegistry<K, V>
where
   K: Clone + Eq + Hash,
{
   fn new() -> Self {
      Self {
         values: DashMap::new(),
      }
   }

   fn insert(&self, key: K, value: &Arc<V>) {
      self.values.insert(key, Arc::clone(value));
   }

   fn get_or_insert_with(&self, key: K, create: impl FnOnce() -> V) -> Arc<V> {
      if let Some(value) = self.get(&key) {
         return value;
      }

      // Construct before entering the shard so arbitrary initialization never
      // runs while a DashMap lock is held. A racing insertion may make this
      // allocation unused, which is preferable to extending the lock lifetime.
      let candidate = Arc::new(create());
      Arc::clone(self.values.entry(key).or_insert(candidate).value())
   }

   fn get(&self, key: &K) -> Option<Arc<V>> {
      self.values.get(key).map(|value| Arc::clone(value.value()))
   }

   fn remove(&self, key: &K) -> Option<Arc<V>> {
      self.values.remove(key).map(|(_, value)| value)
   }

   fn remove_value(&self, value: &Arc<V>) -> bool {
      let key = self
         .values
         .iter()
         .find(|entry| Arc::ptr_eq(entry.value(), value))
         .map(|entry| entry.key().clone());
      key.is_some_and(|key| {
         self
            .values
            .remove_if(&key, |_, current| Arc::ptr_eq(current, value))
            .is_some()
      })
   }

   fn values(&self) -> Vec<Arc<V>> {
      self
         .values
         .iter()
         .map(|value| Arc::clone(value.value()))
         .collect()
   }

   fn remove_all(&self) -> Vec<Arc<V>> {
      let keys = self
         .values
         .iter()
         .map(|entry| entry.key().clone())
         .collect::<Vec<_>>();
      keys
         .into_iter()
         .filter_map(|key| self.remove(&key))
         .collect()
   }
}

// Scope tree

#[derive(Debug)]
struct EngineScope {
   publisher: LivePublisher<EngineStatus, EngineEventKind>,
}

/// One self-contained torrent projection tree.
#[derive(Debug)]
pub(crate) struct TorrentScope {
   pub(crate) info_hash: InfoHash,
   pub(crate) publisher: Arc<LivePublisher<Option<TorrentView>, TorrentEventKind>>,
   peers: ScopeRegistry<PeerId, LiveScope<PeerIdentity, PeerView, PeerEventKind>>,
   trackers: ScopeRegistry<Tracker, LiveScope<TrackerIdentity, TrackerView, TrackerEventKind>>,
   torrent: OnceLock<Arc<TorrentInner>>,
   registered: AtomicBool,
   publication: Mutex<()>,
}

impl TorrentScope {
   fn new(info_hash: InfoHash, event_capacity: usize) -> Self {
      Self {
         info_hash,
         publisher: Arc::new(LivePublisher::new(None, event_capacity)),
         peers: ScopeRegistry::new(),
         trackers: ScopeRegistry::new(),
         torrent: OnceLock::new(),
         registered: AtomicBool::new(false),
         publication: Mutex::new(()),
      }
   }

   fn register(&self, torrent: &Torrent) -> bool {
      if self.torrent.set(Arc::clone(&torrent.inner)).is_err() {
         return false;
      }
      self.registered.store(true, Ordering::Release);
      true
   }

   fn is_registered(&self) -> bool {
      self.registered.load(Ordering::Acquire)
   }

   #[cfg(test)]
   pub(super) fn mark_registered_for_benchmark(&self) {
      self.registered.store(true, Ordering::Release);
   }

   fn handle(&self) -> Option<Torrent> {
      self.torrent.get().map(|inner| Torrent {
         inner: Arc::clone(inner),
      })
   }

   fn publication_lock(&self) -> MutexGuard<'_, ()> {
      self
         .publication
         .lock()
         .unwrap_or_else(std::sync::PoisonError::into_inner)
   }
}

/// Ownership root for transport-agnostic live projections.
#[derive(Debug)]
pub(crate) struct HubInner {
   engine: EngineScope,
   torrents: ScopeRegistry<InfoHash, TorrentScope>,
   settings: LiveSettings,
   next_tracker_id: AtomicU64,
}

impl HubInner {
   fn torrent_handle(&self, info_hash: InfoHash) -> Option<Torrent> {
      self
         .torrents
         .get(&info_hash)
         .and_then(|scope| scope.handle())
   }
}

// Hub lifetime

#[derive(Debug, Clone)]
enum HubReference {
   Strong(Arc<HubInner>),
   Weak(Weak<HubInner>),
}

impl HubReference {
   fn inner(&self) -> Option<Arc<HubInner>> {
      match self {
         Self::Strong(inner) => Some(Arc::clone(inner)),
         Self::Weak(inner) => inner.upgrade(),
      }
   }
}

/// Cloneable coordinator for the complete live projection tree.
///
/// The engine owns a strong instance. Supervised actors receive weak instances
/// so the projection tree cannot participate in an ownership cycle.
#[derive(Debug, Clone)]
pub(crate) struct Hub {
   inner: HubReference,
}

impl Hub {
   // Engine projection

   pub(crate) fn new() -> Self {
      Self::with_settings(LiveSettings::default())
   }

   pub(crate) fn with_settings(settings: LiveSettings) -> Self {
      Self {
         inner: HubReference::Strong(Arc::new(HubInner {
            engine: EngineScope {
               publisher: LivePublisher::new(
                  EngineStatus::Starting,
                  settings.engine_event_capacity,
               ),
            },
            torrents: ScopeRegistry::new(),
            settings,
            next_tracker_id: AtomicU64::new(1),
         })),
      }
   }

   pub(crate) fn from_inner(inner: Arc<HubInner>) -> Self {
      Self {
         inner: HubReference::Strong(inner),
      }
   }

   pub(crate) fn weak(&self) -> Self {
      Self {
         inner: HubReference::Weak(self.downgrade()),
      }
   }

   pub(crate) fn downgrade(&self) -> Weak<HubInner> {
      match &self.inner {
         HubReference::Strong(inner) => Arc::downgrade(inner),
         HubReference::Weak(inner) => inner.clone(),
      }
   }

   fn inner(&self) -> Option<Arc<HubInner>> {
      self.inner.inner()
   }

   pub(crate) fn subscribe(&self) -> EventSubscription {
      self
         .inner()
         .map_or_else(EventSubscription::closed, |inner| {
            inner.engine.publisher.subscribe()
         })
   }

   /// Derives the root projection from engine lifecycle and registered child
   /// scopes. The root never caches torrent views.
   pub(crate) fn view(&self) -> EngineView {
      let Some(inner) = self.inner() else {
         return EngineView {
            status: EngineStatus::Stopped,
            torrents: Vec::new(),
         };
      };
      let mut torrents = inner
         .torrents
         .values()
         .into_iter()
         .filter(|scope| scope.is_registered())
         .filter_map(|scope| scope.publisher.view())
         .collect::<Vec<_>>();
      torrents.sort_by(|left, right| left.info_hash.as_bytes().cmp(right.info_hash.as_bytes()));
      EngineView {
         status: inner.engine.publisher.view(),
         torrents,
      }
   }

   pub(crate) fn engine_started(&self) {
      let Some(inner) = self.inner() else {
         return;
      };
      let _ = inner.engine.publisher.replace_view(EngineStatus::Running);
      let _ = inner
         .engine
         .publisher
         .emit_without_view_change(EngineEventKind::EngineStarted(self.view()));
   }

   pub(crate) fn engine_start_failed(&self, message: impl Into<String>) {
      let Some(inner) = self.inner() else {
         return;
      };
      let health = LiveHealth {
         torrent: None,
         level: LiveHealthLevel::Error,
         message: message.into(),
      };
      let _ = inner
         .engine
         .publisher
         .close_with_terminal_event(EngineStatus::Failed, EngineEventKind::Health(health));
   }

   pub(crate) fn engine_stopping(&self) {
      let Some(inner) = self.inner() else {
         return;
      };
      let _ = inner.engine.publisher.replace_view(EngineStatus::Stopping);
   }

   pub(crate) fn engine_stopped(&self) {
      let Some(inner) = self.inner() else {
         return;
      };
      let mut view = self.view();
      view.status = EngineStatus::Stopped;
      let _ = inner
         .engine
         .publisher
         .close_with_terminal_event(EngineStatus::Stopped, EngineEventKind::Shutdown(view));
   }

   // Torrent scopes

   pub(crate) fn ensure_torrent_scope(&self, info_hash: InfoHash) -> Option<Arc<TorrentScope>> {
      let inner = self.inner()?;
      Some(inner.torrents.get_or_insert_with(info_hash, || {
         TorrentScope::new(info_hash, inner.settings.torrent_event_capacity)
      }))
   }

   pub(crate) fn torrent_handle(&self, torrent: InfoHash) -> Option<Torrent> {
      self.inner()?.torrent_handle(torrent)
   }

   #[cfg(test)]
   pub(crate) fn torrent_view(&self, torrent: InfoHash) -> Option<TorrentView> {
      self
         .inner()?
         .torrents
         .get(&torrent)
         .and_then(|scope| scope.publisher.view())
   }

   pub(crate) fn initialize_torrent_projection(&self, torrent: TorrentView) {
      let Some(scope) = self.ensure_torrent_scope(torrent.info_hash) else {
         return;
      };
      let _ = scope.publisher.replace_view(Some(torrent));
   }

   pub(crate) fn register_torrent_scope(&self, torrent: Torrent) {
      let info_hash = torrent.info_hash();
      let Some(inner) = self.inner() else {
         return;
      };
      let scope = inner.torrents.get_or_insert_with(info_hash, || {
         TorrentScope::new(info_hash, inner.settings.torrent_event_capacity)
      });
      if !scope.register(&torrent) || scope.publisher.view().is_none() {
         return;
      }
      let _publication = scope.publication_lock();
      if !scope
         .publisher
         .emit_without_view_change(TorrentEventKind::Added)
      {
         return;
      }
      let _ = inner
         .engine
         .publisher
         .emit_without_view_change(EngineEventKind::Torrent {
            torrent,
            event: TorrentEventKind::Added,
         });
   }

   pub(crate) fn replace_torrent_view_and_emit(
      &self, torrent: TorrentView, event: TorrentEventKind,
   ) {
      let info_hash = torrent.info_hash;
      let Some(inner) = self.inner() else {
         return;
      };
      let Some(scope) = inner.torrents.get(&info_hash) else {
         return;
      };
      debug_assert_eq!(scope.info_hash, info_hash);
      let _publication = scope.publication_lock();
      if !scope
         .publisher
         .replace_view_and_emit(Some(torrent), event.clone())
      {
         return;
      }
      let Some(handle) = scope.handle() else {
         return;
      };
      let _ = inner
         .engine
         .publisher
         .emit_without_view_change(EngineEventKind::Torrent {
            torrent: handle,
            event,
         });
   }

   pub(crate) fn emit_health(
      &self, torrent: Option<InfoHash>, level: LiveHealthLevel, message: impl Into<String>,
   ) {
      let Some(inner) = self.inner() else {
         return;
      };
      let health = LiveHealth {
         torrent,
         level,
         message: message.into(),
      };
      if let Some(info_hash) = torrent
         && let Some(scope) = inner.torrents.get(&info_hash)
      {
         Self::emit_without_torrent_view_change(&inner, &scope, TorrentEventKind::Health(health));
      } else {
         let _ = inner
            .engine
            .publisher
            .emit_without_view_change(EngineEventKind::Health(health));
      }
   }

   pub(crate) fn remove_torrent_scope(&self, info_hash: InfoHash) {
      let Some(inner) = self.inner() else {
         return;
      };
      let Some(scope) = inner.torrents.get(&info_hash) else {
         return;
      };
      let torrent = scope.handle();
      let peers = scope
         .peers
         .values()
         .into_iter()
         .map(|inner| PeerHandle { inner })
         .collect::<Vec<_>>();
      let trackers = scope
         .trackers
         .values()
         .into_iter()
         .map(|inner| TrackerHandle { inner })
         .collect::<Vec<_>>();
      let publication = scope.publication_lock();

      for peer in peers {
         peer.close_without_parent_event();
      }
      for tracker in trackers {
         tracker.close_without_parent_event();
      }

      if !scope
         .publisher
         .close_with_terminal_event(None, TorrentEventKind::Removed)
      {
         return;
      }
      drop(publication);
      inner.torrents.remove(&info_hash);
      if let Some(torrent) = torrent {
         let _ = inner
            .engine
            .publisher
            .emit_without_view_change(EngineEventKind::Torrent {
               torrent,
               event: TorrentEventKind::Removed,
            });
      }
   }

   fn emit_without_torrent_view_change(
      inner: &HubInner, scope: &TorrentScope, event: TorrentEventKind,
   ) {
      let Some(torrent) = scope.handle() else {
         return;
      };
      let _publication = scope.publication_lock();
      if !scope.publisher.emit_without_view_change(event.clone()) {
         return;
      }
      let _ = inner
         .engine
         .publisher
         .emit_without_view_change(EngineEventKind::Torrent { torrent, event });
   }

   // Peer scopes

   pub(crate) fn peer_handles(&self, torrent: InfoHash) -> Vec<PeerHandle> {
      let Some(inner) = self.inner() else {
         return Vec::new();
      };
      inner.torrents.get(&torrent).map_or_else(Vec::new, |scope| {
         scope
            .peers
            .values()
            .into_iter()
            .map(|inner| PeerHandle { inner })
            .filter(|peer| peer.view().connected)
            .collect()
      })
   }

   pub(crate) fn register_peer_scope(
      &self, identity: PeerIdentity, view: PeerView,
   ) -> Option<PeerHandle> {
      let inner = self.inner()?;
      let scope = inner.torrents.get_or_insert_with(identity.torrent, || {
         TorrentScope::new(identity.torrent, inner.settings.torrent_event_capacity)
      });
      let peer = PeerHandle::new(
         identity,
         view,
         self.downgrade(),
         inner.settings.peer_event_capacity,
      );
      scope.peers.insert(identity.peer, &peer.inner);
      Some(peer)
   }

   pub(crate) fn emit_peer_connected(&self, peer: &PeerHandle) {
      let Some(inner) = self.inner() else {
         return;
      };
      let Some(scope) = inner.torrents.get(&peer.torrent()) else {
         return;
      };
      if scope.peers.get(&peer.id()).is_some() {
         Self::emit_without_torrent_view_change(
            &inner,
            &scope,
            TorrentEventKind::PeerConnected(peer.clone()),
         );
      }
   }

   pub(crate) fn mark_peer_disconnected(&self, peer: &PeerHandle) {
      let Some(inner) = self.inner() else {
         return;
      };
      let Some(scope) = inner.torrents.get(&peer.torrent()) else {
         return;
      };
      if scope.peers.remove(&peer.id()).is_none() {
         return;
      }
      Self::emit_without_torrent_view_change(
         &inner,
         &scope,
         TorrentEventKind::PeerDisconnected(peer.clone()),
      );
   }

   pub(crate) fn close_peer_scopes_for_torrent_restart(&self, torrent: InfoHash) {
      let Some(inner) = self.inner() else {
         return;
      };
      let Some(scope) = inner.torrents.get(&torrent) else {
         return;
      };
      for inner in scope.peers.remove_all() {
         PeerHandle { inner }.close_without_parent_event();
      }
   }

   // Tracker scopes

   pub(crate) fn tracker_handles(&self, torrent: InfoHash) -> Vec<TrackerHandle> {
      let Some(inner) = self.inner() else {
         return Vec::new();
      };
      inner.torrents.get(&torrent).map_or_else(Vec::new, |scope| {
         scope
            .trackers
            .values()
            .into_iter()
            .map(|inner| TrackerHandle { inner })
            .collect()
      })
   }

   pub(crate) fn register_tracker_scope(
      &self, torrent: InfoHash, source: &Tracker, view: TrackerView,
   ) -> Option<TrackerHandle> {
      let inner = self.inner()?;
      let torrent_scope = inner.torrents.get_or_insert_with(torrent, || {
         TorrentScope::new(torrent, inner.settings.torrent_event_capacity)
      });
      if let Some(existing) = torrent_scope.trackers.get(source) {
         if !existing.publisher.is_closed() {
            return Some(TrackerHandle { inner: existing });
         }
         let _ = torrent_scope.trackers.remove_value(&existing);
      }
      let id = TrackerId::new(inner.next_tracker_id.fetch_add(1, Ordering::Relaxed));
      let identity = TrackerIdentity { torrent, id };
      let tracker = TrackerHandle::new(
         identity,
         view,
         self.downgrade(),
         inner.settings.tracker_event_capacity,
      );
      torrent_scope
         .trackers
         .insert(source.clone(), &tracker.inner);
      Some(tracker)
   }

   pub(crate) fn remove_tracker_scope(&self, tracker: &TrackerHandle) {
      let Some(inner) = self.inner() else {
         return;
      };
      let Some(scope) = inner.torrents.get(&tracker.torrent()) else {
         return;
      };
      let _ = scope.trackers.remove_value(&tracker.inner);
   }

   pub(crate) fn emit_tracker_event(&self, tracker: &TrackerHandle, event: TrackerEventKind) {
      let Some(inner) = self.inner() else {
         return;
      };
      let Some(scope) = inner.torrents.get(&tracker.torrent()) else {
         return;
      };
      let torrent_event = match event {
         TrackerEventKind::AnnounceSucceeded { .. } => {
            TorrentEventKind::TrackerAnnounceSucceeded(tracker.clone())
         }
         TrackerEventKind::AnnounceFailed => {
            TorrentEventKind::TrackerAnnounceFailed(tracker.clone())
         }
         TrackerEventKind::Restarting => TorrentEventKind::TrackerRestarting(tracker.clone()),
         TrackerEventKind::Stopped => TorrentEventKind::TrackerStopped(tracker.clone()),
      };
      Self::emit_without_torrent_view_change(&inner, &scope, torrent_event);
   }
}

impl Default for Hub {
   fn default() -> Self {
      Self::new()
   }
}

#[cfg(test)]
mod tests {
   use std::net::{Ipv4Addr, SocketAddr};

   use super::{
      super::{EventStreamError, TrackerStatus},
      *,
   };
   use crate::{
      metrics::{
         ByteCount, ContentProgress, PeerMetrics, TorrentMetrics, TrackerMetrics, TransferMetrics,
      },
      peer::PeerId,
      torrent::TorrentState,
   };

   fn connected_peer_view() -> PeerView {
      PeerView {
         address: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 6881))),
         client: Some("Unknown".to_string()),
         connected: true,
         metrics: PeerMetrics {
            peer_choking: true,
            client_choking: true,
            ..Default::default()
         },
      }
   }

   fn pending_tracker_view() -> TrackerView {
      TrackerView {
         endpoint: "https://tracker.example".to_string(),
         status: TrackerStatus::Pending,
         metrics: TrackerMetrics::default(),
      }
   }

   fn torrent_view(info_hash: InfoHash) -> TorrentView {
      TorrentView {
         info_hash,
         name: "removed".to_string(),
         state: TorrentState::Downloading,
         auto_start: true,
         sufficient_peers: 1,
         peer_count: 0,
         tracker_count: 0,
         output_path: None,
         metrics: TorrentMetrics::new(
            TransferMetrics::default(),
            ContentProgress {
               total_bytes: Some(ByteCount(1_000)),
               verified_bytes: ByteCount::ZERO,
               remaining_bytes: Some(ByteCount(1_000)),
               progress_fraction: Some(0.0),
               completed_pieces: 0,
               partial_pieces: 0,
               total_pieces: 1,
            },
         ),
      }
   }

   #[tokio::test]
   async fn torrent_removal_closes_every_child_scope_exactly_once() {
      let hub = Hub::new();
      let info_hash = InfoHash::from_bytes([4; 20]);
      hub.initialize_torrent_projection(torrent_view(info_hash));
      let peer = hub
         .register_peer_scope(
            PeerIdentity {
               torrent: info_hash,
               peer: PeerId::Unknown([5; 20]),
            },
            connected_peer_view(),
         )
         .unwrap();
      let source = Tracker::Http("https://tracker.example/announce".to_string());
      let tracker = hub
         .register_tracker_scope(info_hash, &source, pending_tracker_view())
         .unwrap();
      let mut peer_events = peer.subscribe();
      let mut tracker_events = tracker.subscribe();

      hub.remove_torrent_scope(info_hash);
      hub.remove_torrent_scope(info_hash);

      assert_eq!(
         peer_events.recv().await.unwrap().kind,
         PeerEventKind::Disconnected
      );
      assert_eq!(peer_events.recv().await, Err(EventStreamError::Closed));
      assert_eq!(
         tracker_events.recv().await.unwrap().kind,
         TrackerEventKind::Stopped
      );
      assert_eq!(tracker_events.recv().await, Err(EventStreamError::Closed));
   }

   #[test]
   fn torrent_view_updates_before_handle_registration() {
      let hub = Hub::new();
      let info_hash = InfoHash::from_bytes([6; 20]);
      let mut updated = torrent_view(info_hash);
      updated.name = "newer".to_string();

      hub.initialize_torrent_projection(torrent_view(info_hash));
      hub.replace_torrent_view_and_emit(updated.clone(), TorrentEventKind::Updated);

      assert_eq!(hub.torrent_view(info_hash), Some(updated));
   }

   #[tokio::test]
   async fn expired_weak_hub_publication_is_a_no_op() {
      let hub = Hub::new();
      let weak = hub.weak();
      drop(hub);

      weak.emit_health(None, LiveHealthLevel::Error, "late");
      weak.engine_stopping();

      assert_eq!(weak.view().status, EngineStatus::Stopped);
      assert!(matches!(
         weak.subscribe().recv().await,
         Err(EventStreamError::Closed)
      ));
   }
}
