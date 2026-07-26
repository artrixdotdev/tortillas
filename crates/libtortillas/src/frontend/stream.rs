//! Generic current-view and future-event stream machinery.
//!
//! This module owns the complete stream lifecycle: lazy channel allocation,
//! ordered publication, terminal closure, subscriptions, and listeners.

use std::{
   fmt,
   pin::Pin,
   sync::{Arc, Mutex, MutexGuard},
   task::{Context, Poll},
};

use futures::{Stream, future::poll_fn};
use thiserror::Error;
use tokio::sync::broadcast;
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};

use super::{EngineEventKind, EngineView, SequencedEvent, TorrentEventKind, TorrentView};

fn mutex_lock<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
   lock
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Generic current-state and event publisher for live application APIs.
///
/// The same primitive backs engine, torrent, peer, and tracker listeners. It
/// can also be reused by future protocol integrations without introducing
/// another channel or listener implementation.
#[derive(Debug, Clone)]
pub struct LivePublisher<V, E> {
   state: Arc<Mutex<LiveState<V>>>,
   channel: Arc<LiveChannel<E>>,
}

#[derive(Debug)]
struct LiveState<V> {
   view: V,
   sequence: u64,
   closed: bool,
}

#[derive(Debug)]
struct LiveChannel<E> {
   capacity: usize,
   sender: Mutex<Option<broadcast::Sender<SequencedEvent<E>>>>,
}

impl<V, E> LivePublisher<V, E>
where
   V: Clone + Send + Sync + 'static,
   E: Clone + Send + 'static,
{
   /// Creates a publisher with an initial view and bounded event capacity.
   ///
   /// A zero capacity is normalized to one so configuration mistakes cannot
   /// panic a public operation.
   #[must_use]
   pub fn new(initial_view: V, event_capacity: usize) -> Self {
      Self {
         state: Arc::new(Mutex::new(LiveState {
            view: initial_view,
            sequence: 0,
            closed: false,
         })),
         channel: Arc::new(LiveChannel {
            capacity: event_capacity.max(1),
            sender: Mutex::new(None),
         }),
      }
   }

   /// Subscribes to all future events from this publisher.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription<E> {
      let state = mutex_lock(&self.state);
      if state.closed {
         return {
            let (sender, receiver) = broadcast::channel(1);
            let weak = sender.downgrade();
            drop(sender);
            EventSubscription::from_receiver(receiver, weak)
         };
      }
      let mut slot = mutex_lock(&self.channel.sender);
      let sender = slot.get_or_insert_with(|| {
         let (sender, _) = broadcast::channel(self.channel.capacity);
         sender
      });
      EventSubscription::from_receiver(sender.subscribe(), sender.downgrade())
   }

   #[cfg(test)]
   pub(crate) fn has_event_channel(&self) -> bool {
      mutex_lock(&self.channel.sender).is_some()
   }

   #[cfg(test)]
   pub(crate) fn allocated_event_slots(&self) -> usize {
      usize::from(self.has_event_channel()) * self.channel.capacity
   }

   #[cfg(test)]
   pub(crate) fn allocation_lower_bound_bytes(&self) -> usize {
      std::mem::size_of_val(self.state.as_ref())
         + std::mem::size_of_val(self.channel.as_ref())
         + self
            .allocated_event_slots()
            .saturating_mul(std::mem::size_of::<SequencedEvent<E>>())
   }

   /// Creates a stream-compatible listener paired with the current view.
   #[must_use]
   pub fn listener(&self) -> EventListener<V, E> {
      let state = Arc::clone(&self.state);
      EventListener::new(self.subscribe(), move || mutex_lock(&state).view.clone())
   }

   /// Clones the latest coherent view.
   #[must_use]
   pub fn view(&self) -> V {
      mutex_lock(&self.state).view.clone()
   }

   /// Replaces the current view without emitting an event.
   ///
   /// Returns `false` when the publisher has already closed.
   pub fn replace_view(&self, view: V) -> bool {
      let mut state = mutex_lock(&self.state);
      if state.closed {
         return false;
      }
      state.view = view;
      true
   }

   /// Replaces the current view and emits the corresponding event.
   ///
   /// Returns `false` when the publisher has already closed.
   pub fn replace_view_and_emit(&self, view: V, event: E) -> bool {
      self.apply_and_emit(|current| *current = view, event)
   }

   /// Emits an event using this publisher's monotonic sequence.
   ///
   /// Returns `false` when the publisher has already closed.
   pub fn emit_without_view_change(&self, event: E) -> bool {
      self.apply_and_emit(|_| {}, event)
   }

   /// Atomically updates the view and permanently closes this publisher after
   /// delivering one terminal event.
   ///
   /// Returns `false` if another caller already closed the publisher.
   pub fn close_with_terminal_event(&self, view: V, event: E) -> bool {
      let mut state = mutex_lock(&self.state);
      if state.closed {
         return false;
      }
      state.view = view;
      state.sequence = state.sequence.saturating_add(1);
      state.closed = true;
      let mut sender = mutex_lock(&self.channel.sender);
      if let Some(sender) = sender.take() {
         let _ = sender.send(SequencedEvent {
            sequence: state.sequence,
            kind: event,
         });
      }
      true
   }

   fn apply_and_emit(&self, edit: impl FnOnce(&mut V), event: E) -> bool {
      let mut state = mutex_lock(&self.state);
      if state.closed {
         return false;
      }
      edit(&mut state.view);
      state.sequence = state.sequence.saturating_add(1);
      self.send_if_subscribed(&state, event);
      true
   }

   fn send_if_subscribed(&self, state: &LiveState<V>, event: E) {
      if let Some(sender) = mutex_lock(&self.channel.sender).as_ref() {
         let _ = sender.send(SequencedEvent {
            sequence: state.sequence,
            kind: event,
         });
      }
   }
}

// Subscription

/// A generic, lag-aware subscription to events from a live publisher.
///
/// `EventSubscription` implements [`Stream`], so applications can use the
/// standard async stream combinators from `futures` or `tokio-stream`. The
/// inherent [`Self::recv`] method remains available for Tokio-style loops.
pub struct EventSubscription<E = EngineEventKind> {
   sender: broadcast::WeakSender<SequencedEvent<E>>,
   stream: BroadcastStream<SequencedEvent<E>>,
}

impl<E: Clone + Send + 'static> EventSubscription<E> {
   pub(crate) fn new(sender: broadcast::Sender<SequencedEvent<E>>) -> Self {
      Self::from_receiver(sender.subscribe(), sender.downgrade())
   }

   pub(crate) fn from_receiver(
      receiver: broadcast::Receiver<SequencedEvent<E>>,
      sender: broadcast::WeakSender<SequencedEvent<E>>,
   ) -> Self {
      Self {
         stream: BroadcastStream::new(receiver),
         sender,
      }
   }

   fn closed() -> Self {
      let (sender, receiver) = broadcast::channel(1);
      let weak = sender.downgrade();
      drop(sender);
      Self::from_receiver(receiver, weak)
   }

   /// Waits for the next event in this subscription.
   pub async fn recv(&mut self) -> Result<SequencedEvent<E>, EventStreamError> {
      poll_fn(|context| Pin::new(&mut *self).poll_next(context))
         .await
         .unwrap_or(Err(EventStreamError::Closed))
   }

   /// Creates another subscription beginning at the publisher's current
   /// event position.
   #[must_use]
   pub fn resubscribe(&self) -> Self {
      self.sender.upgrade().map_or_else(Self::closed, Self::new)
   }
}

impl<E: Clone + Send + 'static> Stream for EventSubscription<E> {
   type Item = Result<SequencedEvent<E>, EventStreamError>;

   fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
      match Pin::new(&mut self.stream).poll_next(context) {
         Poll::Ready(Some(Ok(event))) => Poll::Ready(Some(Ok(event))),
         Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(events)))) => {
            Poll::Ready(Some(Err(EventStreamError::Lagged(events))))
         }
         Poll::Ready(None) => Poll::Ready(None),
         Poll::Pending => Poll::Pending,
      }
   }
}

impl<E> fmt::Debug for EventSubscription<E> {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter
         .debug_struct("EventSubscription")
         .field(
            "receiver_count",
            &self
               .sender
               .upgrade()
               .map_or(0, |sender| sender.receiver_count()),
         )
         .finish_non_exhaustive()
   }
}

/// Errors produced while receiving live frontend events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EventStreamError {
   /// This consumer fell behind and the specified number of events were
   /// dropped. The subscription remains usable.
   #[error("frontend event subscriber lagged by {0} events")]
   Lagged(u64),
   /// The publisher closed the event stream.
   #[error("frontend event stream closed")]
   Closed,
}

// Listener

/// A generic event stream paired with a synchronous current-state reader.
///
/// The listener itself implements [`Stream`]. Its view type and event type are
/// generic so engine, torrent, peer, tracker, and future protocol integrations
/// all reuse the same implementation.
pub struct EventListener<V, E = EngineEventKind> {
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
   pub async fn recv(&mut self) -> Result<SequencedEvent<E>, EventStreamError> {
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
   type Item = Result<SequencedEvent<E>, EventStreamError>;

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

/// Live engine listener with typed events and current presentation state.
pub type EngineListener = EventListener<EngineView>;

/// Live listener scoped to one torrent.
pub type TorrentListener = EventListener<Option<TorrentView>, TorrentEventKind>;

#[cfg(test)]
mod tests {
   use std::{sync::Arc, thread};

   use super::*;

   #[test]
   fn concurrent_update_and_close_never_accepts_an_update_after_terminal() {
      for _ in 0..100 {
         let live = Arc::new(LivePublisher::new(0_u64, 8));
         let update = Arc::clone(&live);
         let close = Arc::clone(&live);
         let update_thread = thread::spawn(move || update.replace_view_and_emit(1, "updated"));
         let close_thread = thread::spawn(move || close.close_with_terminal_event(2, "closed"));
         let update_accepted = update_thread.join().unwrap();
         let close_accepted = close_thread.join().unwrap();

         assert!(close_accepted);
         assert!(!live.replace_view_and_emit(3, "late"));
         assert_eq!(live.view(), 2);
         if update_accepted {
            assert_eq!(live.view(), 2);
         }
      }
   }

   #[test]
   fn zero_capacity_is_normalized_without_panicking() {
      let publisher = LivePublisher::new(0_u8, 0);
      let _subscription = publisher.subscribe();
      assert!(publisher.emit_without_view_change("event"));
   }

   #[test]
   fn publishers_without_listeners_do_not_allocate_event_channels() {
      let publisher = LivePublisher::<_, ()>::new(0_u8, 8);

      assert!(!publisher.has_event_channel());
      let _listener = publisher.listener();
      assert!(publisher.has_event_channel());
   }
}
