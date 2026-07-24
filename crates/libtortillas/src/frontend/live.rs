use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use tokio::sync::broadcast;

use super::{EventListener, EventSubscription, Sequenced};

/// Number of discrete frontend events retained by each live publisher.
pub const DEFAULT_EVENT_CAPACITY: usize = 256;

pub(crate) fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
   lock
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn write_lock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
   lock
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
   sender: Mutex<Option<broadcast::Sender<Sequenced<E>>>>,
}

impl<V, E> LivePublisher<V, E>
where
   V: Clone + Send + Sync + 'static,
   E: Clone + Send + 'static,
{
   /// Creates a publisher with an initial view and bounded event capacity.
   ///
   /// # Panics
   ///
   /// Panics when `event_capacity` is zero.
   #[must_use]
   pub fn new(initial_view: V, event_capacity: usize) -> Self {
      assert!(event_capacity > 0, "event capacity must be non-zero");
      let (events, _) = broadcast::channel(event_capacity);
      Self {
         state: Arc::new(Mutex::new(LiveState {
            view: initial_view,
            sequence: 0,
            closed: false,
         })),
         channel: Arc::new(LiveChannel {
            sender: Mutex::new(Some(events)),
         }),
      }
   }

   /// Subscribes to all future events from this publisher.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription<E> {
      let sender = mutex_lock(&self.channel.sender);
      match sender.as_ref() {
         Some(sender) => EventSubscription::from_receiver(sender.subscribe(), sender.downgrade()),
         None => {
            let (sender, receiver) = broadcast::channel(1);
            let weak = sender.downgrade();
            drop(sender);
            EventSubscription::from_receiver(receiver, weak)
         }
      }
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
   pub fn set_view(&self, view: V) -> bool {
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
   pub fn update(&self, view: V, event: E) -> bool {
      self.mutate(|current| *current = view, event)
   }

   /// Emits an event using this publisher's monotonic sequence.
   ///
   /// Returns `false` when the publisher has already closed.
   pub fn publish(&self, kind: E) -> bool {
      self.mutate(|_| {}, kind)
   }

   /// Atomically updates the view and permanently closes this publisher after
   /// delivering one terminal event.
   ///
   /// Returns `false` if another caller already closed the publisher.
   pub fn close(&self, view: V, event: E) -> bool {
      let mut state = mutex_lock(&self.state);
      if state.closed {
         return false;
      }
      state.view = view;
      state.sequence = state.sequence.saturating_add(1);
      state.closed = true;
      let mut sender = mutex_lock(&self.channel.sender);
      if let Some(sender) = sender.take() {
         let _ = sender.send(Sequenced {
            sequence: state.sequence,
            kind: event,
         });
      }
      true
   }

   pub(crate) fn edit_and_publish(&self, edit: impl FnOnce(&mut V) -> E) -> bool {
      let mut state = mutex_lock(&self.state);
      if state.closed {
         return false;
      }
      let event = edit(&mut state.view);
      state.sequence = state.sequence.saturating_add(1);
      self.send(&state, event);
      true
   }

   pub(crate) fn edit_if_and_publish(&self, edit: impl FnOnce(&mut V) -> bool, event: E) -> bool {
      let mut state = mutex_lock(&self.state);
      if state.closed || !edit(&mut state.view) {
         return false;
      }
      state.sequence = state.sequence.saturating_add(1);
      self.send(&state, event);
      true
   }

   pub(crate) fn edit_view<R>(&self, edit: impl FnOnce(&mut V) -> R) -> Option<R> {
      let mut state = mutex_lock(&self.state);
      (!state.closed).then(|| edit(&mut state.view))
   }

   fn mutate(&self, edit: impl FnOnce(&mut V), event: E) -> bool {
      let mut state = mutex_lock(&self.state);
      if state.closed {
         return false;
      }
      edit(&mut state.view);
      state.sequence = state.sequence.saturating_add(1);
      self.send(&state, event);
      true
   }

   fn send(&self, state: &LiveState<V>, event: E) {
      if let Some(sender) = mutex_lock(&self.channel.sender).as_ref() {
         let _ = sender.send(Sequenced {
            sequence: state.sequence,
            kind: event,
         });
      }
   }
}
