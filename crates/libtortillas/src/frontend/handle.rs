use std::{fmt, net::SocketAddr};

use super::{
   DEFAULT_EVENT_CAPACITY, EventListener, EventSubscription, FrontendPublisher, LivePublisher,
   PeerEventKind, PeerView, TrackerEventKind, TrackerView,
};
use crate::{hashes::InfoHash, peer::PeerId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PeerScope {
   pub(crate) torrent: InfoHash,
   pub(crate) peer: PeerId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TrackerScope {
   pub(crate) torrent: InfoHash,
   pub(crate) endpoint: String,
}

/// Public identity and live frontend access for one connected peer.
#[derive(Clone)]
pub struct PeerHandle {
   scope: PeerScope,
   frontend: FrontendPublisher,
   live: LivePublisher<PeerView, PeerEventKind>,
}

impl PeerHandle {
   pub(crate) fn new(scope: PeerScope, view: PeerView, frontend: FrontendPublisher) -> Self {
      Self {
         scope,
         frontend,
         live: LivePublisher::new(view, DEFAULT_EVENT_CAPACITY),
      }
   }

   /// Torrent that owns this peer connection.
   #[must_use]
   pub const fn torrent(&self) -> InfoHash {
      self.scope.torrent
   }

   /// Handshaked peer identifier.
   #[must_use]
   pub const fn id(&self) -> PeerId {
      self.scope.peer
   }

   /// Latest known network address.
   #[must_use]
   pub fn address(&self) -> Option<SocketAddr> {
      self.live_view().address
   }

   /// Subscribes to events for this peer only.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription<PeerEventKind> {
      self.live.subscribe()
   }

   /// Creates a stream-compatible listener for this peer.
   #[must_use]
   pub fn listener(&self) -> PeerListener {
      self.live.listener()
   }

   /// Returns the latest peer view, including its terminal disconnected state.
   #[must_use]
   pub fn live_view(&self) -> PeerView {
      self.live.view()
   }

   pub(crate) const fn scope(&self) -> PeerScope {
      self.scope
   }

   pub(crate) fn update(&self, view: PeerView) {
      self.live.update(view, PeerEventKind::Updated);
      self.frontend.peer_updated(self);
   }

   pub(crate) fn disconnected(&self) {
      let mut view = self.live_view();
      if !view.connected {
         return;
      }
      view.connected = false;
      self.live.update(view, PeerEventKind::Disconnected);
      self.frontend.peer_disconnected(self.clone());
   }
}

impl fmt::Debug for PeerHandle {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter
         .debug_struct("PeerHandle")
         .field("torrent", &self.scope.torrent)
         .field("peer", &self.scope.peer)
         .finish_non_exhaustive()
   }
}

impl PartialEq for PeerHandle {
   fn eq(&self, other: &Self) -> bool {
      self.scope == other.scope
   }
}

impl Eq for PeerHandle {}

/// Public identity and live frontend access for one tracker.
#[derive(Clone)]
pub struct TrackerHandle {
   scope: TrackerScope,
   frontend: FrontendPublisher,
   live: LivePublisher<TrackerView, TrackerEventKind>,
}

impl TrackerHandle {
   pub(crate) fn new(scope: TrackerScope, view: TrackerView, frontend: FrontendPublisher) -> Self {
      Self {
         scope,
         frontend,
         live: LivePublisher::new(view, DEFAULT_EVENT_CAPACITY),
      }
   }

   /// Torrent that owns this tracker.
   #[must_use]
   pub const fn torrent(&self) -> InfoHash {
      self.scope.torrent
   }

   /// Credential-free tracker endpoint.
   #[must_use]
   pub fn endpoint(&self) -> &str {
      &self.scope.endpoint
   }

   /// Subscribes to events for this tracker only.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription<TrackerEventKind> {
      self.live.subscribe()
   }

   /// Creates a stream-compatible listener for this tracker.
   #[must_use]
   pub fn listener(&self) -> TrackerListener {
      self.live.listener()
   }

   /// Returns the latest tracker view, including its terminal stopped state.
   #[must_use]
   pub fn live_view(&self) -> TrackerView {
      self.live.view()
   }

   pub(crate) fn scope(&self) -> &TrackerScope {
      &self.scope
   }

   pub(crate) fn announce_succeeded(&self, peers_returned: u64) {
      let mut view = self.live_view();
      view.active = true;
      view.healthy = true;
      view.peers_returned = Some(peers_returned);
      self
         .live
         .update(view, TrackerEventKind::AnnounceSucceeded { peers_returned });
      self.frontend.tracker_announce_succeeded(self);
   }

   pub(crate) fn announce_failed(&self) {
      let mut view = self.live_view();
      view.active = true;
      view.healthy = false;
      view.peers_returned = None;
      self.live.update(view, TrackerEventKind::AnnounceFailed);
      self.frontend.tracker_announce_failed(self);
   }

   pub(crate) fn stopped(&self) {
      let mut view = self.live_view();
      if !view.active {
         return;
      }
      view.active = false;
      self.live.update(view, TrackerEventKind::Stopped);
      self.frontend.tracker_stopped(self);
   }
}

impl fmt::Debug for TrackerHandle {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter
         .debug_struct("TrackerHandle")
         .field("torrent", &self.scope.torrent)
         .field("endpoint", &self.scope.endpoint)
         .finish_non_exhaustive()
   }
}

impl PartialEq for TrackerHandle {
   fn eq(&self, other: &Self) -> bool {
      self.scope == other.scope
   }
}

impl Eq for TrackerHandle {}

impl fmt::Display for TrackerHandle {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter.write_str(self.endpoint())
   }
}

/// Live listener scoped to one peer.
pub type PeerListener = EventListener<PeerView, PeerEventKind>;

/// Live listener scoped to one tracker.
pub type TrackerListener = EventListener<TrackerView, TrackerEventKind>;
