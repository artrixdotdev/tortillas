use std::sync::{
   Arc, RwLock, RwLockReadGuard, RwLockWriteGuard,
   atomic::{AtomicU64, Ordering},
};

use tokio::sync::broadcast;

use super::{
   CoreEventKind, EngineView, EventListener, EventSubscription, FrontendHealth,
   FrontendHealthLevel, PeerView, Sequenced, TorrentView, TrackerView,
};
use crate::{engine::EngineStatus, hashes::InfoHash, torrent::TorrentState};

/// Number of discrete frontend events retained for each listener.
pub const DEFAULT_EVENT_CAPACITY: usize = 256;

/// Generic current-state and event publisher for live application APIs.
///
/// The same primitive backs engine, torrent, peer, and tracker listeners. It
/// can also be reused by future protocol integrations without introducing
/// another channel or listener implementation.
#[derive(Debug, Clone)]
pub struct LivePublisher<V, E> {
   inner: Arc<LivePublisherInner<V, E>>,
}

#[derive(Debug)]
struct LivePublisherInner<V, E> {
   events: broadcast::Sender<Sequenced<E>>,
   view: RwLock<V>,
   sequence: AtomicU64,
}

impl<V, E> LivePublisher<V, E>
where
   V: Clone + Send + Sync + 'static,
   E: Clone + Send + 'static,
{
   /// Creates a publisher with an initial view and bounded event capacity.
   #[must_use]
   pub fn new(initial_view: V, event_capacity: usize) -> Self {
      let (events, _) = broadcast::channel(event_capacity);
      Self {
         inner: Arc::new(LivePublisherInner {
            events,
            view: RwLock::new(initial_view),
            sequence: AtomicU64::new(0),
         }),
      }
   }

   /// Subscribes to all future events from this publisher.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription<E> {
      EventSubscription::new(self.inner.events.clone(), None)
   }

   pub(crate) fn subscribe_where(
      &self, filter: impl Fn(&E) -> bool + Send + Sync + 'static,
   ) -> EventSubscription<E> {
      EventSubscription::new(self.inner.events.clone(), Some(Arc::new(filter)))
   }

   /// Creates a stream-compatible listener paired with the current view.
   #[must_use]
   pub fn listener(&self) -> EventListener<V, E> {
      let publisher = self.clone();
      EventListener::new(self.subscribe(), move || publisher.view())
   }

   /// Clones the latest coherent view.
   #[must_use]
   pub fn view(&self) -> V {
      self.read_view().clone()
   }

   /// Replaces the current view without emitting an event.
   pub fn set_view(&self, view: V) {
      *self.write_view() = view;
   }

   /// Replaces the current view and emits the corresponding event.
   pub fn update(&self, view: V, event: E) {
      self.set_view(view);
      self.publish(event);
   }

   /// Emits an event using this publisher's monotonic sequence.
   pub fn publish(&self, kind: E) {
      let sequence = self.inner.sequence.fetch_add(1, Ordering::Relaxed) + 1;
      let _ = self.inner.events.send(Sequenced { sequence, kind });
   }

   pub(crate) fn edit_view<R>(&self, edit: impl FnOnce(&mut V) -> R) -> R {
      edit(&mut self.write_view())
   }

   fn read_view(&self) -> RwLockReadGuard<'_, V> {
      self
         .inner
         .view
         .read()
         .unwrap_or_else(std::sync::PoisonError::into_inner)
   }

   fn write_view(&self) -> RwLockWriteGuard<'_, V> {
      self
         .inner
         .view
         .write()
         .unwrap_or_else(std::sync::PoisonError::into_inner)
   }
}

/// Shared live-state publisher used by the engine actor hierarchy.
#[derive(Debug, Clone)]
pub(crate) struct FrontendPublisher {
   live: LivePublisher<EngineView, CoreEventKind>,
}

impl FrontendPublisher {
   pub(crate) fn new() -> Self {
      Self::with_event_capacity(DEFAULT_EVENT_CAPACITY)
   }

   fn with_event_capacity(event_capacity: usize) -> Self {
      Self {
         live: LivePublisher::new(
            EngineView {
               status: EngineStatus::Starting,
               torrent_count: 0,
               torrents: Vec::new(),
            },
            event_capacity,
         ),
      }
   }

   pub(crate) fn subscribe(&self) -> EventSubscription {
      self.live.subscribe()
   }

   pub(crate) fn subscribe_torrent(&self, torrent: InfoHash) -> EventSubscription {
      self
         .live
         .subscribe_where(move |event| event.torrent() == Some(torrent))
   }

   pub(crate) fn view(&self) -> EngineView {
      self.live.view()
   }

   pub(crate) fn torrent_view(&self, torrent: InfoHash) -> Option<TorrentView> {
      self
         .live
         .view()
         .torrents
         .into_iter()
         .find(|view| view.info_hash == torrent)
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
      if self.update_torrent_entry(torrent.clone()) {
         self.publish(CoreEventKind::TorrentUpdated(torrent));
      }
   }

   pub(crate) fn metadata_resolved(&self, torrent: TorrentView) {
      if self.update_torrent_entry(torrent.clone()) {
         self.publish(CoreEventKind::MetadataResolved(torrent));
      }
   }

   pub(crate) fn progress_changed(&self, torrent: TorrentView) {
      let info_hash = torrent.info_hash;
      let progress = torrent.progress.clone();
      if self.update_torrent_entry(torrent) {
         self.publish(CoreEventKind::ProgressChanged {
            torrent: info_hash,
            progress,
         });
      }
   }

   pub(crate) fn peer_connected(&self, torrent: TorrentView, peer: PeerView) {
      let info_hash = torrent.info_hash;
      if self.update_torrent_entry(torrent) {
         self.publish(CoreEventKind::PeerConnected {
            torrent: info_hash,
            peer,
         });
      }
   }

   pub(crate) fn peer_disconnected(&self, torrent: TorrentView, peer: PeerView) {
      let info_hash = torrent.info_hash;
      if self.update_torrent_entry(torrent) {
         self.publish(CoreEventKind::PeerDisconnected {
            torrent: info_hash,
            peer,
         });
      }
   }

   pub(crate) fn tracker_announce_succeeded(&self, torrent: TorrentView, tracker: TrackerView) {
      let info_hash = torrent.info_hash;
      if self.update_torrent_entry(torrent) {
         self.publish(CoreEventKind::TrackerAnnounceSucceeded {
            torrent: info_hash,
            tracker,
         });
      }
   }

   pub(crate) fn tracker_announce_failed(&self, torrent: TorrentView, tracker: TrackerView) {
      let info_hash = torrent.info_hash;
      if self.update_torrent_entry(torrent) {
         self.publish(CoreEventKind::TrackerAnnounceFailed {
            torrent: info_hash,
            tracker,
         });
      }
   }

   pub(crate) fn health(
      &self, torrent: Option<InfoHash>, level: FrontendHealthLevel, message: impl Into<String>,
   ) {
      self.publish(CoreEventKind::Health(FrontendHealth {
         torrent,
         level,
         message: message.into(),
      }));
   }

   pub(crate) fn torrent_state_changed(&self, previous: TorrentState, torrent: TorrentView) {
      let info_hash = torrent.info_hash;
      let current = torrent.state;
      if self.update_torrent_entry(torrent) {
         self.publish(CoreEventKind::TorrentStateChanged {
            torrent: info_hash,
            previous,
            current,
         });
      }
   }

   pub(crate) fn torrent_removed(&self, torrent: InfoHash) {
      self.live.edit_view(|view| {
         view
            .torrents
            .retain(|candidate| candidate.info_hash != torrent);
         view.torrent_count = u64::try_from(view.torrents.len()).unwrap_or(u64::MAX);
      });
      self.publish(CoreEventKind::TorrentRemoved { torrent });
   }

   pub(crate) fn publish(&self, kind: CoreEventKind) {
      self.live.publish(kind);
   }

   fn set_engine_status(&self, status: EngineStatus) -> EngineView {
      self.live.edit_view(|view| {
         view.status = status;
         view.clone()
      })
   }

   fn replace_torrent(&self, torrent: TorrentView) {
      self.live.edit_view(|view| {
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
      });
   }

   fn update_torrent_entry(&self, torrent: TorrentView) -> bool {
      self.live.edit_view(|view| {
         let Some(current) = view
            .torrents
            .iter_mut()
            .find(|candidate| candidate.info_hash == torrent.info_hash)
         else {
            return false;
         };
         *current = torrent;
         true
      })
   }
}

impl Default for FrontendPublisher {
   fn default() -> Self {
      Self::new()
   }
}
