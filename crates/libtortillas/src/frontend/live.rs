use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::broadcast;

use super::{EventListener, EventSubscription, Sequenced};

/// Number of discrete frontend events retained by each live publisher.
pub const DEFAULT_EVENT_CAPACITY: usize = 256;

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
   sender: Mutex<Option<broadcast::Sender<Sequenced<E>>>>,
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
            .saturating_mul(std::mem::size_of::<Sequenced<E>>())
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
         let update_thread = thread::spawn(move || update.update(1, "updated"));
         let close_thread = thread::spawn(move || close.close(2, "closed"));
         let update_accepted = update_thread.join().unwrap();
         let close_accepted = close_thread.join().unwrap();

         assert!(close_accepted);
         assert!(!live.update(3, "late"));
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
      assert!(publisher.publish("event"));
   }
}
