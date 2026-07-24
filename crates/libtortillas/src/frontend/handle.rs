use std::{fmt, net::SocketAddr};

use super::{EventListener, EventSubscription, FrontendPublisher, PeerView, TrackerView};
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

impl PeerHandle {
   pub(crate) const fn new(scope: PeerScope, frontend: FrontendPublisher) -> Self {
      Self { scope, frontend }
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
      self.live_view().and_then(|view| view.address)
   }

   /// Subscribes to events for this peer only.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription {
      self.frontend.subscribe_peer(self.scope)
   }

   /// Creates a stream-compatible listener for this peer.
   #[must_use]
   pub fn listener(&self) -> PeerListener {
      let frontend = self.frontend.clone();
      let scope = self.scope;
      PeerListener::new(self.subscribe(), move || frontend.peer_view(scope))
   }

   /// Returns the latest peer view, or `None` after its torrent is removed.
   #[must_use]
   pub fn live_view(&self) -> Option<PeerView> {
      self.frontend.peer_view(self.scope)
   }

   pub(crate) const fn scope(&self) -> PeerScope {
      self.scope
   }

   pub(crate) fn update(&self, view: PeerView) {
      self.frontend.peer_updated(self, view);
   }

   pub(crate) fn disconnected(&self) {
      self.frontend.peer_disconnected(self.clone());
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

impl TrackerHandle {
   pub(crate) fn new(scope: TrackerScope, frontend: FrontendPublisher) -> Self {
      Self { scope, frontend }
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
   pub fn subscribe(&self) -> EventSubscription {
      self.frontend.subscribe_tracker(self.scope.clone())
   }

   /// Creates a stream-compatible listener for this tracker.
   #[must_use]
   pub fn listener(&self) -> TrackerListener {
      let frontend = self.frontend.clone();
      let scope = self.scope.clone();
      TrackerListener::new(self.subscribe(), move || frontend.tracker_view(&scope))
   }

   /// Returns the current tracker view, or `None` after removal.
   #[must_use]
   pub fn live_view(&self) -> Option<TrackerView> {
      self.frontend.tracker_view(&self.scope)
   }

   pub(crate) fn scope(&self) -> &TrackerScope {
      &self.scope
   }

   pub(crate) fn announce_succeeded(&self, peers_returned: u64) {
      self.frontend.tracker_announce_succeeded(
         self,
         TrackerView {
            endpoint: self.scope.endpoint.clone(),
            active: true,
            healthy: true,
            peers_returned: Some(peers_returned),
         },
      );
   }

   pub(crate) fn announce_failed(&self) {
      self.frontend.tracker_announce_failed(
         self,
         TrackerView {
            endpoint: self.scope.endpoint.clone(),
            active: true,
            healthy: false,
            peers_returned: None,
         },
      );
   }

   pub(crate) fn stopped(&self) {
      self.frontend.tracker_stopped(self);
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
pub type PeerListener = EventListener<Option<PeerView>>;

/// Live listener scoped to one tracker.
pub type TrackerListener = EventListener<Option<TrackerView>>;
