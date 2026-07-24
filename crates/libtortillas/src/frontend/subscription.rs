use std::{
   fmt,
   pin::Pin,
   sync::Arc,
   task::{Context, Poll},
};

use futures::{Stream, future::poll_fn};
use thiserror::Error;
use tokio::sync::broadcast;
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};

use super::{CoreEventKind, Sequenced};

type EventFilter<E> = Arc<dyn Fn(&E) -> bool + Send + Sync>;

/// A generic, lag-aware subscription to events from a live publisher.
///
/// `EventSubscription` implements [`Stream`], so applications can use the
/// standard async stream combinators from `futures` or `tokio-stream`. The
/// inherent [`Self::recv`] method remains available for Tokio-style loops.
pub struct EventSubscription<E = CoreEventKind> {
   sender: broadcast::Sender<Sequenced<E>>,
   stream: BroadcastStream<Sequenced<E>>,
   filter: Option<EventFilter<E>>,
}

impl<E: Clone + Send + 'static> EventSubscription<E> {
   pub(crate) fn new(
      sender: broadcast::Sender<Sequenced<E>>, filter: Option<EventFilter<E>>,
   ) -> Self {
      Self {
         stream: BroadcastStream::new(sender.subscribe()),
         sender,
         filter,
      }
   }

   /// Waits for the next event in this subscription.
   pub async fn recv(&mut self) -> Result<Sequenced<E>, EventStreamError> {
      poll_fn(|context| Pin::new(&mut *self).poll_next(context))
         .await
         .unwrap_or(Err(EventStreamError::Closed))
   }

   /// Creates another subscription beginning at the publisher's current
   /// event position and retaining this subscription's filter.
   #[must_use]
   pub fn resubscribe(&self) -> Self {
      Self::new(self.sender.clone(), self.filter.clone())
   }
}

impl<E: Clone + Send + 'static> Stream for EventSubscription<E> {
   type Item = Result<Sequenced<E>, EventStreamError>;

   fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
      loop {
         match Pin::new(&mut self.stream).poll_next(context) {
            Poll::Ready(Some(Ok(event))) => {
               if self
                  .filter
                  .as_ref()
                  .is_none_or(|filter| filter(&event.kind))
               {
                  return Poll::Ready(Some(Ok(event)));
               }
            }
            Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(events)))) => {
               return Poll::Ready(Some(Err(EventStreamError::Lagged(events))));
            }
            Poll::Ready(None) => return Poll::Ready(None),
            Poll::Pending => return Poll::Pending,
         }
      }
   }
}

impl<E> fmt::Debug for EventSubscription<E> {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter
         .debug_struct("EventSubscription")
         .field("receiver_count", &self.sender.receiver_count())
         .field("filtered", &self.filter.is_some())
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
