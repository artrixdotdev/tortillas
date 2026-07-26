//! Identity-bearing peer and tracker access handles.
//!
//! Handles expose only identity, the current projection, and scoped event
//! access. Actor ownership and mutation commands remain outside this module.

use std::{
   fmt,
   net::SocketAddr,
   sync::{Arc, Weak},
};

use serde::{Deserialize, Serialize};

use super::{
   EventListener, EventSubscription, Hub, HubInner, LivePublisher, PeerEventKind, PeerView,
   TrackerEventKind, TrackerStatus, TrackerView,
};
use crate::{hashes::InfoHash, metrics::TrackerMetrics, peer::PeerId};

/// Shared live state behind an identity-bearing protocol handle.
pub(crate) struct LiveScope<I, V, E> {
   pub(crate) identity: I,
   hub: Weak<HubInner>,
   pub(crate) live: LivePublisher<V, E>,
}

impl<I, V, E> LiveScope<I, V, E>
where
   V: Clone + Send + Sync + 'static,
   E: Clone + Send + 'static,
{
   fn new(identity: I, view: V, hub: Weak<HubInner>, event_capacity: usize) -> Self {
      Self {
         identity,
         hub,
         live: LivePublisher::new(view, event_capacity),
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

   fn frontend(&self) -> Option<Hub> {
      self.hub.upgrade().map(Hub::from_inner)
   }
}

impl<I: fmt::Debug, V, E> fmt::Debug for LiveScope<I, V, E> {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter
         .debug_struct("LiveScope")
         .field("identity", &self.identity)
         .finish_non_exhaustive()
   }
}

// Peer

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PeerIdentity {
   pub(crate) torrent: InfoHash,
   pub(crate) peer: PeerId,
}

/// Public identity and live frontend access for one connected peer.
#[derive(Clone)]
pub struct PeerHandle {
   pub(crate) inner: Arc<LiveScope<PeerIdentity, PeerView, PeerEventKind>>,
}

impl PeerHandle {
   pub(crate) fn new(
      identity: PeerIdentity, view: PeerView, hub: Weak<HubInner>, event_capacity: usize,
   ) -> Self {
      Self {
         inner: Arc::new(LiveScope::new(identity, view, hub, event_capacity)),
      }
   }

   #[must_use]
   pub fn torrent(&self) -> InfoHash {
      self.inner.identity.torrent
   }

   #[must_use]
   pub fn id(&self) -> PeerId {
      self.inner.identity.peer
   }

   #[must_use]
   pub fn address(&self) -> Option<SocketAddr> {
      self.view().address
   }

   #[must_use]
   pub fn subscribe(&self) -> EventSubscription<PeerEventKind> {
      self.inner.subscribe()
   }

   #[must_use]
   pub fn listener(&self) -> PeerListener {
      self.inner.listener()
   }

   #[must_use]
   pub fn view(&self) -> PeerView {
      self.inner.view()
   }

   pub(crate) fn identity(&self) -> PeerIdentity {
      self.inner.identity
   }

   pub(crate) fn publish_state(&self, view: PeerView) {
      let _ = self
         .inner
         .live
         .replace_view_and_emit(view, PeerEventKind::StateChanged);
   }

   pub(crate) fn publish_metrics(&self, view: PeerView) {
      let metrics = view.metrics;
      let _ = self
         .inner
         .live
         .replace_view_and_emit(view, PeerEventKind::MetricsChanged(metrics));
   }

   pub(crate) fn disconnected(&self) {
      let mut view = self.view();
      view.connected = false;
      if self
         .inner
         .live
         .close_with_terminal_event(view, PeerEventKind::Disconnected)
         && let Some(frontend) = self.inner.frontend()
      {
         frontend.mark_peer_disconnected(self);
      }
   }

   pub(crate) fn close_without_parent_event(&self) {
      let mut view = self.view();
      view.connected = false;
      let _ = self
         .inner
         .live
         .close_with_terminal_event(view, PeerEventKind::Disconnected);
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
      self.identity() == other.identity()
   }
}

impl Eq for PeerHandle {}

pub type PeerListener = EventListener<PeerView, PeerEventKind>;

// Tracker

#[derive(
   Debug,
   Clone,
   Copy,
   PartialEq,
   Eq,
   PartialOrd,
   Ord,
   Hash,
   Serialize,
   Deserialize
)]
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
pub(crate) struct TrackerIdentity {
   pub(crate) torrent: InfoHash,
   pub(crate) id: TrackerId,
}

/// Public identity and live frontend access for one tracker.
#[derive(Clone)]
pub struct TrackerHandle {
   pub(crate) inner: Arc<LiveScope<TrackerIdentity, TrackerView, TrackerEventKind>>,
}

impl TrackerHandle {
   pub(crate) fn new(
      identity: TrackerIdentity, view: TrackerView, hub: Weak<HubInner>, event_capacity: usize,
   ) -> Self {
      Self {
         inner: Arc::new(LiveScope::new(identity, view, hub, event_capacity)),
      }
   }

   #[must_use]
   pub fn torrent(&self) -> InfoHash {
      self.inner.identity.torrent
   }

   #[must_use]
   pub fn id(&self) -> TrackerId {
      self.inner.identity.id
   }

   #[must_use]
   pub fn endpoint(&self) -> String {
      self.view().endpoint
   }

   #[must_use]
   pub fn subscribe(&self) -> EventSubscription<TrackerEventKind> {
      self.inner.subscribe()
   }

   #[must_use]
   pub fn listener(&self) -> TrackerListener {
      self.inner.listener()
   }

   #[must_use]
   pub fn view(&self) -> TrackerView {
      self.inner.view()
   }

   pub(crate) fn identity(&self) -> TrackerIdentity {
      self.inner.identity
   }

   pub(crate) fn publish_metrics(&self, metrics: TrackerMetrics) {
      let mut view = self.view();
      view.metrics = metrics;
      let _ = self.inner.live.replace_view(view);
   }

   pub(crate) fn announce_succeeded(&self, metrics: TrackerMetrics) {
      let mut view = self.view();
      view.status = TrackerStatus::Healthy;
      view.metrics = metrics;
      let peers_returned = metrics.latest_peers_returned.unwrap_or_default();
      let event = TrackerEventKind::AnnounceSucceeded { peers_returned };
      if self.inner.live.replace_view_and_emit(view, event)
         && let Some(frontend) = self.inner.frontend()
      {
         frontend.emit_tracker_event(self, event);
      }
   }

   pub(crate) fn announce_failed(&self, metrics: TrackerMetrics) {
      let mut view = self.view();
      view.status = TrackerStatus::Degraded;
      view.metrics = metrics;
      if self
         .inner
         .live
         .replace_view_and_emit(view, TrackerEventKind::AnnounceFailed)
         && let Some(frontend) = self.inner.frontend()
      {
         frontend.emit_tracker_event(self, TrackerEventKind::AnnounceFailed);
      }
   }

   pub(crate) fn restarting(&self) {
      let mut view = self.view();
      view.status = TrackerStatus::Restarting;
      if self
         .inner
         .live
         .replace_view_and_emit(view, TrackerEventKind::Restarting)
         && let Some(frontend) = self.inner.frontend()
      {
         frontend.emit_tracker_event(self, TrackerEventKind::Restarting);
      }
   }

   pub(crate) fn stopped(&self) {
      let mut view = self.view();
      view.status = TrackerStatus::Stopped;
      if self
         .inner
         .live
         .close_with_terminal_event(view, TrackerEventKind::Stopped)
         && let Some(frontend) = self.inner.frontend()
      {
         frontend.emit_tracker_event(self, TrackerEventKind::Stopped);
      }
   }

   pub(crate) fn close_without_parent_event(&self) {
      let mut view = self.view();
      view.status = TrackerStatus::Stopped;
      let _ = self
         .inner
         .live
         .close_with_terminal_event(view, TrackerEventKind::Stopped);
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
      self.identity() == other.identity()
   }
}

impl Eq for TrackerHandle {}

impl fmt::Display for TrackerHandle {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter.write_str(&self.endpoint())
   }
}

pub type TrackerListener = EventListener<TrackerView, TrackerEventKind>;

#[cfg(test)]
mod tests {
   use std::net::{Ipv4Addr, SocketAddr};

   use super::{super::EventStreamError, *};
   use crate::{
      metrics::{ByteCount, PeerMetrics, TrafficTotals},
      peer::PeerId,
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

   fn peer_handle(frontend: &Hub) -> PeerHandle {
      frontend.register_peer_scope(
         PeerIdentity {
            torrent: InfoHash::from_bytes([1; 20]),
            peer: PeerId::Unknown([2; 20]),
         },
         connected_peer_view(),
      )
   }

   #[tokio::test]
   async fn peer_handle_when_updated_then_only_its_listener_receives_event() {
      let frontend = Hub::new();
      let peer = peer_handle(&frontend);
      let mut listener = peer.listener();
      let mut updated = peer.view();
      updated.metrics.transfer.totals = TrafficTotals {
         downloaded: ByteCount(16),
         uploaded: ByteCount::ZERO,
      };

      peer.publish_metrics(updated);

      let event = listener.recv().await.unwrap();
      assert!(matches!(event.kind, PeerEventKind::MetricsChanged(_)));
      assert_eq!(
         listener.view().metrics.transfer.totals.downloaded,
         ByteCount(16)
      );
   }

   #[tokio::test]
   async fn disconnected_peer_rejects_late_actor_updates() {
      let frontend = Hub::new();
      let peer = peer_handle(&frontend);
      let mut listener = peer.listener();
      let mut late = peer.view();

      peer.disconnected();
      late.metrics.transfer.totals.downloaded = ByteCount(32);
      peer.publish_metrics(late);

      assert_eq!(
         listener.recv().await.unwrap().kind,
         PeerEventKind::Disconnected
      );
      assert_eq!(listener.recv().await, Err(EventStreamError::Closed));
      assert!(!listener.view().connected);
      assert_eq!(
         listener.view().metrics.transfer.totals.downloaded,
         ByteCount::ZERO
      );
   }

   #[test]
   fn live_handles_do_not_keep_their_hub_alive() {
      let frontend = Hub::new();
      let hub = frontend.downgrade();
      let peer = peer_handle(&frontend);

      drop(frontend);

      assert!(hub.upgrade().is_none());
      assert!(peer.view().connected);
   }
}
