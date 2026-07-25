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
   EngineEventKind, EngineView, EventSubscription, FrontendHealth, FrontendHealthLevel,
   LivePublisher, PeerEventKind, PeerHandle, PeerView, TorrentEventKind, TorrentView,
   TrackerEventKind, TrackerHandle, TrackerView,
   handle::{LiveScope, PeerIdentity, TrackerId, TrackerIdentity},
};
use crate::{
   engine::EngineStatus,
   hashes::InfoHash,
   peer::PeerId,
   settings::FrontendSettings,
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
   live: LivePublisher<EngineStatus, EngineEventKind>,
}

/// One self-contained torrent projection tree.
#[derive(Debug)]
pub(crate) struct TorrentScope {
   pub(crate) info_hash: InfoHash,
   pub(crate) live: Arc<LivePublisher<Option<TorrentView>, TorrentEventKind>>,
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
         live: Arc::new(LivePublisher::new(None, event_capacity)),
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
pub(crate) struct FrontendHubInner {
   engine: EngineScope,
   torrents: ScopeRegistry<InfoHash, TorrentScope>,
   settings: FrontendSettings,
   next_tracker_id: AtomicU64,
}

impl FrontendHubInner {
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
   Strong(Arc<FrontendHubInner>),
   Weak(Weak<FrontendHubInner>),
}

/// Cloneable coordinator for the complete live projection tree.
///
/// The engine owns a strong instance. Supervised actors receive weak instances
/// so the projection tree cannot participate in an ownership cycle.
#[derive(Debug, Clone)]
pub(crate) struct FrontendHub {
   inner: HubReference,
}

impl FrontendHub {
   // Engine projection

   pub(crate) fn new() -> Self {
      Self::with_settings(FrontendSettings::default())
   }

   pub(crate) fn with_settings(settings: FrontendSettings) -> Self {
      Self {
         inner: HubReference::Strong(Arc::new(FrontendHubInner {
            engine: EngineScope {
               live: LivePublisher::new(EngineStatus::Starting, settings.engine_event_capacity),
            },
            torrents: ScopeRegistry::new(),
            settings,
            next_tracker_id: AtomicU64::new(1),
         })),
      }
   }

   pub(crate) fn from_inner(inner: Arc<FrontendHubInner>) -> Self {
      Self {
         inner: HubReference::Strong(inner),
      }
   }

   pub(crate) fn weak(&self) -> Self {
      Self {
         inner: HubReference::Weak(self.downgrade()),
      }
   }

   pub(crate) fn downgrade(&self) -> Weak<FrontendHubInner> {
      match &self.inner {
         HubReference::Strong(inner) => Arc::downgrade(inner),
         HubReference::Weak(inner) => inner.clone(),
      }
   }

   fn inner(&self) -> Arc<FrontendHubInner> {
      match &self.inner {
         HubReference::Strong(inner) => Arc::clone(inner),
         HubReference::Weak(inner) => inner
            .upgrade()
            .expect("frontend hub outlived by its actor hierarchy"),
      }
   }

   pub(crate) fn subscribe(&self) -> EventSubscription {
      self.inner().engine.live.subscribe()
   }

   /// Derives the root projection from engine lifecycle and registered child
   /// scopes. The root never caches torrent views.
   pub(crate) fn view(&self) -> EngineView {
      let inner = self.inner();
      let mut torrents = inner
         .torrents
         .values()
         .into_iter()
         .filter(|scope| scope.is_registered())
         .filter_map(|scope| scope.live.view())
         .collect::<Vec<_>>();
      torrents.sort_by(|left, right| left.info_hash.as_bytes().cmp(right.info_hash.as_bytes()));
      EngineView {
         status: inner.engine.live.view(),
         torrents,
      }
   }

   pub(crate) fn engine_started(&self) {
      let inner = self.inner();
      let _ = inner.engine.live.replace_view(EngineStatus::Running);
      let _ = inner
         .engine
         .live
         .emit_without_view_change(EngineEventKind::EngineStarted(self.view()));
   }

   pub(crate) fn engine_stopping(&self) {
      let _ = self
         .inner()
         .engine
         .live
         .replace_view(EngineStatus::Stopping);
   }

   pub(crate) fn engine_stopped(&self) {
      let mut view = self.view();
      view.status = EngineStatus::Stopped;
      let _ = self
         .inner()
         .engine
         .live
         .close_with_terminal_event(EngineStatus::Stopped, EngineEventKind::Shutdown(view));
   }

   // Torrent scopes

   pub(crate) fn ensure_torrent_scope(&self, info_hash: InfoHash) -> Arc<TorrentScope> {
      let inner = self.inner();
      inner.torrents.get_or_insert_with(info_hash, || {
         TorrentScope::new(info_hash, inner.settings.torrent_event_capacity)
      })
   }

   pub(crate) fn torrent_handle(&self, torrent: InfoHash) -> Option<Torrent> {
      self.inner().torrent_handle(torrent)
   }

   #[cfg(test)]
   pub(crate) fn torrent_view(&self, torrent: InfoHash) -> Option<TorrentView> {
      self
         .inner()
         .torrents
         .get(&torrent)
         .and_then(|scope| scope.live.view())
   }

   pub(crate) fn initialize_torrent_projection(&self, torrent: TorrentView) {
      let scope = self.ensure_torrent_scope(torrent.info_hash);
      let _ = scope.live.replace_view(Some(torrent));
   }

   pub(crate) fn register_torrent_scope(&self, torrent: Torrent) {
      let info_hash = torrent.info_hash();
      let scope = self.ensure_torrent_scope(info_hash);
      if !scope.register(&torrent) {
         return;
      }
      if let Some(view) = scope.live.view() {
         self.replace_torrent_view_and_emit(view, TorrentEventKind::Added);
      }
   }

   pub(crate) fn replace_torrent_view_and_emit(
      &self, torrent: TorrentView, event: TorrentEventKind,
   ) {
      let info_hash = torrent.info_hash;
      let Some(scope) = self.inner().torrents.get(&info_hash) else {
         return;
      };
      let Some(handle) = self.torrent_handle(info_hash) else {
         return;
      };
      let _publication = scope.publication_lock();
      if !scope
         .live
         .replace_view_and_emit(Some(torrent), event.clone())
      {
         return;
      }
      let _ = self
         .inner()
         .engine
         .live
         .emit_without_view_change(EngineEventKind::Torrent {
            torrent: handle,
            event,
         });
   }

   pub(crate) fn emit_health(
      &self, torrent: Option<InfoHash>, level: FrontendHealthLevel, message: impl Into<String>,
   ) {
      let health = FrontendHealth {
         torrent,
         level,
         message: message.into(),
      };
      if let Some(info_hash) = torrent
         && let Some(scope) = self.inner().torrents.get(&info_hash)
      {
         self.emit_without_torrent_view_change(&scope, TorrentEventKind::Health(health));
      } else {
         let _ = self
            .inner()
            .engine
            .live
            .emit_without_view_change(EngineEventKind::Health(health));
      }
   }

   pub(crate) fn remove_torrent_scope(&self, info_hash: InfoHash) {
      let Some(scope) = self.inner().torrents.get(&info_hash) else {
         return;
      };
      let torrent = self.torrent_handle(info_hash);
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
         .live
         .close_with_terminal_event(None, TorrentEventKind::Removed)
      {
         return;
      }
      drop(publication);
      self.inner().torrents.remove(&info_hash);
      if let Some(torrent) = torrent {
         let _ = self
            .inner()
            .engine
            .live
            .emit_without_view_change(EngineEventKind::Torrent {
               torrent,
               event: TorrentEventKind::Removed,
            });
      }
   }

   fn emit_without_torrent_view_change(&self, scope: &TorrentScope, event: TorrentEventKind) {
      let Some(torrent) = self.torrent_handle(scope.info_hash) else {
         return;
      };
      let _publication = scope.publication_lock();
      if !scope.live.emit_without_view_change(event.clone()) {
         return;
      }
      let _ = self
         .inner()
         .engine
         .live
         .emit_without_view_change(EngineEventKind::Torrent { torrent, event });
   }

   // Peer scopes

   pub(crate) fn peer_handles(&self, torrent: InfoHash) -> Vec<PeerHandle> {
      self
         .inner()
         .torrents
         .get(&torrent)
         .map_or_else(Vec::new, |scope| {
            scope
               .peers
               .values()
               .into_iter()
               .map(|inner| PeerHandle { inner })
               .filter(|peer| peer.view().connected)
               .collect()
         })
   }

   pub(crate) fn register_peer_scope(&self, identity: PeerIdentity, view: PeerView) -> PeerHandle {
      let inner = self.inner();
      let scope = self.ensure_torrent_scope(identity.torrent);
      let peer = PeerHandle::new(
         identity,
         view,
         self.downgrade(),
         inner.settings.peer_event_capacity,
      );
      scope.peers.insert(identity.peer, &peer.inner);
      peer
   }

   pub(crate) fn emit_peer_connected(&self, peer: &PeerHandle) {
      let Some(scope) = self.inner().torrents.get(&peer.torrent()) else {
         return;
      };
      if scope.peers.get(&peer.id()).is_some() {
         self.emit_without_torrent_view_change(
            &scope,
            TorrentEventKind::PeerConnected(peer.clone()),
         );
      }
   }

   pub(crate) fn mark_peer_disconnected(&self, peer: &PeerHandle) {
      let Some(scope) = self.inner().torrents.get(&peer.torrent()) else {
         return;
      };
      if scope.peers.remove(&peer.id()).is_none() {
         return;
      }
      self.emit_without_torrent_view_change(
         &scope,
         TorrentEventKind::PeerDisconnected(peer.clone()),
      );
   }

   pub(crate) fn close_peer_scopes_for_torrent_restart(&self, torrent: InfoHash) {
      let Some(scope) = self.inner().torrents.get(&torrent) else {
         return;
      };
      for inner in scope.peers.remove_all() {
         PeerHandle { inner }.close_without_parent_event();
      }
   }

   // Tracker scopes

   pub(crate) fn tracker_handles(&self, torrent: InfoHash) -> Vec<TrackerHandle> {
      self
         .inner()
         .torrents
         .get(&torrent)
         .map_or_else(Vec::new, |scope| {
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
   ) -> TrackerHandle {
      let inner = self.inner();
      let torrent_scope = self.ensure_torrent_scope(torrent);
      if let Some(inner) = torrent_scope.trackers.get(source) {
         return TrackerHandle { inner };
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
      tracker
   }

   pub(crate) fn emit_tracker_event(&self, tracker: &TrackerHandle, event: TrackerEventKind) {
      let Some(scope) = self.inner().torrents.get(&tracker.torrent()) else {
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
      self.emit_without_torrent_view_change(&scope, torrent_event);
   }
}

impl Default for FrontendHub {
   fn default() -> Self {
      Self::new()
   }
}
