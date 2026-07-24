use std::{
   fmt,
   hash::Hash,
   net::SocketAddr,
   sync::{Arc, Weak},
};

use serde::{Deserialize, Serialize};

use super::{
   DEFAULT_EVENT_CAPACITY, EventListener, EventSubscription, FrontendHub, FrontendPublisher,
   LivePublisher, PeerEventKind, PeerView, TrackerEventKind, TrackerStatus, TrackerView,
};
use crate::{hashes::InfoHash, peer::PeerId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PeerScope {
   pub(crate) torrent: InfoHash,
   pub(crate) peer: PeerId,
}

/// Opaque identity for one tracker actor within an engine.
///
/// Tracker URLs can contain private passkeys and are not suitable identifiers:
/// sanitized URLs can collide while complete URLs must not be exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrackerId(u64);

impl TrackerId {
   pub(crate) const fn new(value: u64) -> Self {
      Self(value)
   }
}

impl fmt::Display for TrackerId {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      self.0.fmt(formatter)
   }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TrackerScope {
   pub(crate) torrent: InfoHash,
   pub(crate) id: TrackerId,
}

/// Shared implementation for identity-bearing live protocol handles.
pub(crate) struct LiveHandle<I, V, E> {
   identity: I,
   hub: Weak<FrontendHub>,
   live: LivePublisher<V, E>,
}

impl<I, V, E> LiveHandle<I, V, E>
where
   V: Clone + Send + Sync + 'static,
   E: Clone + Send + 'static,
{
   fn new(identity: I, view: V, hub: Weak<FrontendHub>) -> Self {
      Self {
         identity,
         hub,
         live: LivePublisher::new(view, DEFAULT_EVENT_CAPACITY),
      }
   }

   fn subscribe(&self) -> EventSubscription<E> {
      self.live.subscribe()
   }

   fn listener(&self) -> EventListener<V, E> {
      self.live.listener()
   }

   fn view(&self) -> V {
      self.live.view()
   }

   fn update(&self, view: V, event: E) -> bool {
      self.live.update(view, event)
   }

   fn close(&self, view: V, event: E) -> bool {
      self.live.close(view, event)
   }

   fn frontend(&self) -> Option<FrontendPublisher> {
      self.hub.upgrade().map(FrontendPublisher::from_hub)
   }
}

impl<I: fmt::Debug, V, E> fmt::Debug for LiveHandle<I, V, E> {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter
         .debug_struct("LiveHandle")
         .field("identity", &self.identity)
         .finish_non_exhaustive()
   }
}

/// Public identity and live frontend access for one connected peer.
#[derive(Clone)]
pub struct PeerHandle {
   pub(crate) inner: Arc<LiveHandle<PeerScope, PeerView, PeerEventKind>>,
}

impl PeerHandle {
   pub(crate) fn new(scope: PeerScope, view: PeerView, hub: Weak<FrontendHub>) -> Self {
      Self {
         inner: Arc::new(LiveHandle::new(scope, view, hub)),
      }
   }

   /// Torrent that owns this peer connection.
   #[must_use]
   pub fn torrent(&self) -> InfoHash {
      self.inner.identity.torrent
   }

   /// Handshaked peer identifier.
   #[must_use]
   pub fn id(&self) -> PeerId {
      self.inner.identity.peer
   }

   /// Latest known network address.
   #[must_use]
   pub fn address(&self) -> Option<SocketAddr> {
      self.live_view().address
   }

   /// Subscribes to events for this peer only.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription<PeerEventKind> {
      self.inner.subscribe()
   }

   /// Creates a stream-compatible listener for this peer.
   #[must_use]
   pub fn listener(&self) -> PeerListener {
      self.inner.listener()
   }

   /// Returns the latest peer view, including its terminal disconnected state.
   #[must_use]
   pub fn live_view(&self) -> PeerView {
      self.inner.view()
   }

   pub(crate) fn scope(&self) -> PeerScope {
      self.inner.identity
   }

   pub(crate) fn update(&self, view: PeerView) {
      if self.inner.update(view, PeerEventKind::Updated)
         && let Some(frontend) = self.inner.frontend()
      {
         frontend.peer_event(self, PeerEventKind::Updated);
      }
   }

   pub(crate) fn disconnected(&self) {
      let mut view = self.live_view();
      view.connected = false;
      if self.inner.close(view, PeerEventKind::Disconnected)
         && let Some(frontend) = self.inner.frontend()
      {
         frontend.peer_event(self, PeerEventKind::Disconnected);
      }
   }
}

impl fmt::Debug for PeerHandle {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter
         .debug_struct("PeerHandle")
         .field("torrent", &self.torrent())
         .field("peer", &self.id())
         .finish_non_exhaustive()
   }
}

impl PartialEq for PeerHandle {
   fn eq(&self, other: &Self) -> bool {
      self.scope() == other.scope()
   }
}

impl Eq for PeerHandle {}

/// Public identity and live frontend access for one tracker.
#[derive(Clone)]
pub struct TrackerHandle {
   pub(crate) inner: Arc<LiveHandle<TrackerScope, TrackerView, TrackerEventKind>>,
}

impl TrackerHandle {
   pub(crate) fn new(scope: TrackerScope, view: TrackerView, hub: Weak<FrontendHub>) -> Self {
      Self {
         inner: Arc::new(LiveHandle::new(scope, view, hub)),
      }
   }

   /// Torrent that owns this tracker.
   #[must_use]
   pub fn torrent(&self) -> InfoHash {
      self.inner.identity.torrent
   }

   /// Opaque identity that remains distinct when sanitized endpoints collide.
   #[must_use]
   pub fn id(&self) -> TrackerId {
      self.inner.identity.id
   }

   /// Credential-free tracker endpoint.
   #[must_use]
   pub fn endpoint(&self) -> String {
      self.live_view().endpoint
   }

   /// Subscribes to events for this tracker only.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription<TrackerEventKind> {
      self.inner.subscribe()
   }

   /// Creates a stream-compatible listener for this tracker.
   #[must_use]
   pub fn listener(&self) -> TrackerListener {
      self.inner.listener()
   }

   /// Returns the latest tracker view, including its terminal stopped state.
   #[must_use]
   pub fn live_view(&self) -> TrackerView {
      self.inner.view()
   }

   pub(crate) fn scope(&self) -> TrackerScope {
      self.inner.identity
   }

   pub(crate) fn announce_succeeded(&self, peers_returned: u64) {
      let mut view = self.live_view();
      view.status = TrackerStatus::Healthy;
      view.peers_returned = Some(peers_returned);
      let event = TrackerEventKind::AnnounceSucceeded { peers_returned };
      if self.inner.update(view, event)
         && let Some(frontend) = self.inner.frontend()
      {
         frontend.tracker_event(self, event);
      }
   }

   pub(crate) fn announce_failed(&self) {
      let mut view = self.live_view();
      view.status = TrackerStatus::Degraded;
      view.peers_returned = None;
      if self.inner.update(view, TrackerEventKind::AnnounceFailed)
         && let Some(frontend) = self.inner.frontend()
      {
         frontend.tracker_event(self, TrackerEventKind::AnnounceFailed);
      }
   }

   pub(crate) fn stopped(&self) {
      let mut view = self.live_view();
      view.status = TrackerStatus::Stopped;
      if self.inner.close(view, TrackerEventKind::Stopped)
         && let Some(frontend) = self.inner.frontend()
      {
         frontend.tracker_event(self, TrackerEventKind::Stopped);
      }
   }
}

impl fmt::Debug for TrackerHandle {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter
         .debug_struct("TrackerHandle")
         .field("torrent", &self.torrent())
         .field("id", &self.id())
         .field("endpoint", &self.endpoint())
         .finish_non_exhaustive()
   }
}

impl PartialEq for TrackerHandle {
   fn eq(&self, other: &Self) -> bool {
      self.scope() == other.scope()
   }
}

impl Eq for TrackerHandle {}

impl fmt::Display for TrackerHandle {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter.write_str(&self.endpoint())
   }
}

/// Live listener scoped to one peer.
pub type PeerListener = EventListener<PeerView, PeerEventKind>;

/// Live listener scoped to one tracker.
pub type TrackerListener = EventListener<TrackerView, TrackerEventKind>;
