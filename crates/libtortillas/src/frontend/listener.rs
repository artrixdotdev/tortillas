use std::{
   fmt,
   pin::Pin,
   sync::Arc,
   task::{Context, Poll},
};

use futures::Stream;

use super::{
   CoreEventKind, EngineView, EventStreamError, EventSubscription, Sequenced, TorrentView,
};

/// A generic event stream paired with a synchronous current-state reader.
///
/// The listener itself implements [`Stream`]. Its view type and event type are
/// generic so engine, torrent, peer, tracker, and future protocol integrations
/// all reuse the same implementation.
pub struct EventListener<V, E = CoreEventKind> {
   events: EventSubscription<E>,
   read_view: Arc<dyn Fn() -> V + Send + Sync>,
}

impl<V, E: Clone + Send + 'static> EventListener<V, E> {
   pub(crate) fn new(
      events: EventSubscription<E>, read_view: impl Fn() -> V + Send + Sync + 'static,
   ) -> Self {
      Self {
         events,
         read_view: Arc::new(read_view),
      }
   }

   /// Waits for the next live event.
   pub async fn recv(&mut self) -> Result<Sequenced<E>, EventStreamError> {
      self.events.recv().await
   }

   /// Reads the latest coherent state without creating a persistence snapshot.
   pub fn view(&self) -> V {
      (self.read_view)()
   }

   /// Returns the underlying event subscription.
   #[must_use]
   pub const fn subscription(&self) -> &EventSubscription<E> {
      &self.events
   }
}

impl<V, E: Clone + Send + 'static> Stream for EventListener<V, E> {
   type Item = Result<Sequenced<E>, EventStreamError>;

   fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
      Pin::new(&mut self.events).poll_next(context)
   }
}

impl<V, E> fmt::Debug for EventListener<V, E> {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter
         .debug_struct("EventListener")
         .field("events", &self.events)
         .finish_non_exhaustive()
   }
}

/// Live engine listener with typed events and current display state.
pub type EngineListener = EventListener<EngineView>;

/// Live listener scoped to one torrent.
pub type TorrentListener = EventListener<Option<TorrentView>>;
