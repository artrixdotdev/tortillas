use std::{
   fmt,
   sync::{Arc, Weak},
};

use serde::{Deserialize, Serialize};

use super::LiveHandle;
use crate::{
   frontend::{
      EventListener, EventSubscription, FrontendHub, TrackerEventKind, TrackerStatus, TrackerView,
   },
   hashes::InfoHash,
};

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
pub(crate) struct TrackerScope {
   pub(crate) torrent: InfoHash,
   pub(crate) id: TrackerId,
}

/// Public identity and live frontend access for one tracker.
#[derive(Clone)]
pub struct TrackerHandle {
   pub(crate) inner: Arc<LiveHandle<TrackerScope, TrackerView, TrackerEventKind>>,
}

impl TrackerHandle {
   pub(crate) fn new(
      scope: TrackerScope, view: TrackerView, hub: Weak<FrontendHub>, event_capacity: usize,
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

   pub(crate) fn scope(&self) -> TrackerScope {
      self.inner.identity
   }

   pub(crate) fn announce_succeeded(&self, peers_returned: u64) {
      let mut view = self.view();
      view.status = TrackerStatus::Healthy;
      view.peers_returned = Some(peers_returned);
      let event = TrackerEventKind::AnnounceSucceeded { peers_returned };
      if self.inner.replace_view_and_emit(view, event)
         && let Some(frontend) = self.inner.frontend()
      {
         frontend.emit_tracker_event(self, event);
      }
   }

   pub(crate) fn announce_failed(&self) {
      let mut view = self.view();
      view.status = TrackerStatus::Degraded;
      view.peers_returned = None;
      if self
         .inner
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
      self.scope() == other.scope()
   }
}

impl Eq for TrackerHandle {}

impl fmt::Display for TrackerHandle {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter.write_str(&self.endpoint())
   }
}

pub type TrackerListener = EventListener<TrackerView, TrackerEventKind>;
