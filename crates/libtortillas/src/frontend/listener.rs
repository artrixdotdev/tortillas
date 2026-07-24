use super::{
   CoreEvent, EngineView, EventStreamError, EventSubscription, FrontendPublisher, TorrentView,
};
use crate::hashes::InfoHash;

/// Live engine listener with typed events and current display state.
///
/// [`Self::recv`] waits for discrete changes. [`Self::view`] reads the latest
/// coherent live state directly from the engine publisher, including after a
/// lag report.
#[derive(Debug)]
pub struct EngineListener {
   events: EventSubscription,
   frontend: FrontendPublisher,
}

impl EngineListener {
   pub(crate) fn new(frontend: FrontendPublisher) -> Self {
      Self {
         events: frontend.subscribe(),
         frontend,
      }
   }

   /// Waits for the next live engine or torrent event.
   pub async fn recv(&mut self) -> Result<CoreEvent, EventStreamError> {
      self.events.recv().await
   }

   /// Returns the latest coherent engine view without persistence snapshots.
   #[must_use]
   pub fn view(&self) -> EngineView {
      self.frontend.view()
   }
}

/// Live listener scoped to one torrent.
#[derive(Debug)]
pub struct TorrentListener {
   torrent: InfoHash,
   events: EventSubscription,
   frontend: FrontendPublisher,
}

impl TorrentListener {
   pub(crate) fn new(frontend: FrontendPublisher, torrent: InfoHash) -> Self {
      Self {
         torrent,
         events: frontend.subscribe_torrent(torrent),
         frontend,
      }
   }

   /// Waits for the next live event associated with this torrent.
   pub async fn recv(&mut self) -> Result<CoreEvent, EventStreamError> {
      self.events.recv().await
   }

   /// Returns the latest torrent view, or `None` after removal.
   #[must_use]
   pub fn view(&self) -> Option<TorrentView> {
      self.frontend.torrent_view(self.torrent)
   }
}
