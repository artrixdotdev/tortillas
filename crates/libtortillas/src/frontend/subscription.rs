use thiserror::Error;
use tokio::sync::broadcast;

use super::CoreEvent;
use crate::hashes::InfoHash;

/// A lag-aware subscription to the engine's typed frontend events.
///
/// The stream is bounded so a stalled UI cannot cause unbounded memory use.
/// If [`Self::recv`] reports [`EventStreamError::Lagged`], redraw from the
/// latest watched snapshot and continue receiving events.
#[derive(Debug)]
pub struct EventSubscription {
   receiver: broadcast::Receiver<CoreEvent>,
   torrent: Option<InfoHash>,
}

impl EventSubscription {
   pub(crate) fn engine(receiver: broadcast::Receiver<CoreEvent>) -> Self {
      Self {
         receiver,
         torrent: None,
      }
   }

   pub(crate) fn torrent(receiver: broadcast::Receiver<CoreEvent>, torrent: InfoHash) -> Self {
      Self {
         receiver,
         torrent: Some(torrent),
      }
   }

   /// Waits for the next event in this subscription.
   ///
   /// Torrent subscriptions skip unrelated events while preserving the
   /// original engine-local sequence numbers.
   pub async fn recv(&mut self) -> Result<CoreEvent, EventStreamError> {
      loop {
         let event = self.receiver.recv().await.map_err(EventStreamError::from)?;
         if self
            .torrent
            .is_none_or(|torrent| event.torrent() == Some(torrent))
         {
            return Ok(event);
         }
      }
   }

   /// Creates another subscription beginning at the current event position.
   #[must_use]
   pub fn resubscribe(&self) -> Self {
      Self {
         receiver: self.receiver.resubscribe(),
         torrent: self.torrent,
      }
   }
}

/// Errors produced while receiving live frontend events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EventStreamError {
   /// This consumer fell behind and the specified number of events were
   /// dropped. The subscription remains usable.
   #[error("frontend event subscriber lagged by {0} events")]
   Lagged(u64),
   /// The engine closed the event stream.
   #[error("frontend event stream closed")]
   Closed,
}

impl From<broadcast::error::RecvError> for EventStreamError {
   fn from(error: broadcast::error::RecvError) -> Self {
      match error {
         broadcast::error::RecvError::Closed => Self::Closed,
         broadcast::error::RecvError::Lagged(events) => Self::Lagged(events),
      }
   }
}
