use std::sync::{
   Arc, RwLock, RwLockReadGuard, RwLockWriteGuard,
   atomic::{AtomicU64, Ordering},
};

use tokio::sync::broadcast;

use super::{CoreEvent, CoreEventKind, EngineView, EventSubscription, TorrentView};
use crate::{engine::EngineStatus, hashes::InfoHash, torrent::TorrentState};

/// Number of discrete frontend events retained for each listener.
pub const DEFAULT_EVENT_CAPACITY: usize = 256;

/// Shared live-state publisher used by the engine actor hierarchy.
#[derive(Debug, Clone)]
pub(crate) struct FrontendPublisher {
   inner: Arc<PublisherInner>,
}

#[derive(Debug)]
struct PublisherInner {
   events: broadcast::Sender<CoreEvent>,
   view: RwLock<EngineView>,
   sequence: AtomicU64,
}

impl FrontendPublisher {
   pub(crate) fn new() -> Self {
      Self::with_event_capacity(DEFAULT_EVENT_CAPACITY)
   }

   fn with_event_capacity(event_capacity: usize) -> Self {
      let (events, _) = broadcast::channel(event_capacity);
      Self {
         inner: Arc::new(PublisherInner {
            events,
            view: RwLock::new(EngineView {
               status: EngineStatus::Starting,
               torrent_count: 0,
               torrents: Vec::new(),
            }),
            sequence: AtomicU64::new(0),
         }),
      }
   }

   pub(crate) fn subscribe(&self) -> EventSubscription {
      EventSubscription::engine(self.inner.events.subscribe())
   }

   pub(crate) fn subscribe_torrent(&self, torrent: InfoHash) -> EventSubscription {
      EventSubscription::torrent(self.inner.events.subscribe(), torrent)
   }

   pub(crate) fn view(&self) -> EngineView {
      self.read_view().clone()
   }

   pub(crate) fn torrent_view(&self, torrent: InfoHash) -> Option<TorrentView> {
      self
         .read_view()
         .torrents
         .iter()
         .find(|view| view.info_hash == torrent)
         .cloned()
   }

   pub(crate) fn engine_started(&self) {
      let view = self.set_engine_status(EngineStatus::Running);
      self.publish(CoreEventKind::EngineStarted(view));
   }

   pub(crate) fn engine_stopping(&self) {
      self.set_engine_status(EngineStatus::Stopping);
   }

   pub(crate) fn engine_stopped(&self) {
      let view = self.set_engine_status(EngineStatus::Stopped);
      self.publish(CoreEventKind::Shutdown(view));
   }

   pub(crate) fn torrent_added(&self, torrent: TorrentView) {
      self.replace_torrent(torrent.clone());
      self.publish(CoreEventKind::TorrentAdded(torrent));
   }

   pub(crate) fn update_torrent(&self, torrent: TorrentView) {
      self.replace_torrent(torrent);
   }

   pub(crate) fn metadata_resolved(&self, torrent: TorrentView) {
      self.replace_torrent(torrent.clone());
      self.publish(CoreEventKind::MetadataResolved(torrent));
   }

   pub(crate) fn torrent_state_changed(&self, previous: TorrentState, torrent: TorrentView) {
      let info_hash = torrent.info_hash;
      let current = torrent.state;
      self.replace_torrent(torrent);
      self.publish(CoreEventKind::TorrentStateChanged {
         torrent: info_hash,
         previous,
         current,
      });
   }

   pub(crate) fn torrent_removed(&self, torrent: InfoHash) {
      let mut view = self.write_view();
      view
         .torrents
         .retain(|candidate| candidate.info_hash != torrent);
      view.torrent_count = u64::try_from(view.torrents.len()).unwrap_or(u64::MAX);
      drop(view);
      self.publish(CoreEventKind::TorrentRemoved { torrent });
   }

   pub(crate) fn publish(&self, kind: CoreEventKind) {
      let sequence = self.inner.sequence.fetch_add(1, Ordering::Relaxed) + 1;
      let _ = self.inner.events.send(CoreEvent { sequence, kind });
   }

   fn set_engine_status(&self, status: EngineStatus) -> EngineView {
      let mut view = self.write_view();
      view.status = status;
      view.clone()
   }

   fn replace_torrent(&self, torrent: TorrentView) {
      let mut view = self.write_view();
      match view
         .torrents
         .iter_mut()
         .find(|candidate| candidate.info_hash == torrent.info_hash)
      {
         Some(current) => *current = torrent,
         None => view.torrents.push(torrent),
      }
      view
         .torrents
         .sort_by(|left, right| left.info_hash.as_bytes().cmp(right.info_hash.as_bytes()));
      view.torrent_count = u64::try_from(view.torrents.len()).unwrap_or(u64::MAX);
   }

   fn read_view(&self) -> RwLockReadGuard<'_, EngineView> {
      self
         .inner
         .view
         .read()
         .unwrap_or_else(std::sync::PoisonError::into_inner)
   }

   fn write_view(&self) -> RwLockWriteGuard<'_, EngineView> {
      self
         .inner
         .view
         .write()
         .unwrap_or_else(std::sync::PoisonError::into_inner)
   }
}

impl Default for FrontendPublisher {
   fn default() -> Self {
      Self::new()
   }
}
