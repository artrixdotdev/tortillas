use std::{fmt, sync::Weak};

use super::{EventListener, EventSubscription, FrontendHub, FrontendPublisher, LivePublisher};

mod peer;
mod tracker;

pub(crate) use peer::PeerScope;
pub use peer::{PeerHandle, PeerListener};
pub(crate) use tracker::TrackerScope;
pub use tracker::{TrackerHandle, TrackerId, TrackerListener};

/// Shared guard-free storage for identity-bearing live protocol handles.
pub(crate) struct LiveHandle<I, V, E> {
   pub(crate) identity: I,
   hub: Weak<FrontendHub>,
   pub(crate) live: LivePublisher<V, E>,
}

impl<I, V, E> LiveHandle<I, V, E>
where
   V: Clone + Send + Sync + 'static,
   E: Clone + Send + 'static,
{
   fn new(identity: I, view: V, hub: Weak<FrontendHub>, event_capacity: usize) -> Self {
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

   fn replace_view_and_emit(&self, view: V, event: E) -> bool {
      self.live.update(view, event)
   }

   fn close_with_terminal_event(&self, view: V, event: E) -> bool {
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
