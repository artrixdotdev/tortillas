use std::{
   fmt,
   net::SocketAddr,
   sync::{Arc, Weak},
};

use super::LiveHandle;
use crate::{
   frontend::{EventListener, EventSubscription, FrontendHub, PeerEventKind, PeerView},
   hashes::InfoHash,
   peer::PeerId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PeerScope {
   pub(crate) torrent: InfoHash,
   pub(crate) peer: PeerId,
}

/// Public identity and live frontend access for one connected peer.
#[derive(Clone)]
pub struct PeerHandle {
   pub(crate) inner: Arc<LiveHandle<PeerScope, PeerView, PeerEventKind>>,
}

impl PeerHandle {
   pub(crate) fn new(
      scope: PeerScope, view: PeerView, hub: Weak<FrontendHub>, event_capacity: usize,
   ) -> Self {
      Self {
         inner: Arc::new(LiveHandle::new(scope, view, hub, event_capacity)),
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

   pub(crate) fn scope(&self) -> PeerScope {
      self.inner.identity
   }

   pub(crate) fn publish_state(&self, view: PeerView) {
      let _ = self
         .inner
         .replace_view_and_emit(view, PeerEventKind::StateChanged);
   }

   pub(crate) fn publish_metrics(&self, view: PeerView) {
      let metrics = view.transfer;
      let _ = self
         .inner
         .replace_view_and_emit(view, PeerEventKind::MetricsChanged(metrics));
   }

   pub(crate) fn disconnected(&self) {
      let mut view = self.view();
      view.connected = false;
      if self
         .inner
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
      self.scope() == other.scope()
   }
}

impl Eq for PeerHandle {}

pub type PeerListener = EventListener<PeerView, PeerEventKind>;
