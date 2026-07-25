use std::sync::{
   Mutex, MutexGuard,
   atomic::{AtomicBool, AtomicU64, Ordering},
};

use super::{
   CoreEventKind, LivePublisher, PeerEventKind, PeerView, TorrentEventKind, TorrentView,
   TrackerEventKind, TrackerView,
   handle::{LiveHandle, PeerScope, TrackerId, TrackerScope},
   registry::ScopeRegistry,
};
use crate::{
   engine::EngineStatus,
   hashes::InfoHash,
   peer::PeerId,
   settings::FrontendSettings,
   torrent::{Torrent, TorrentInner},
   tracker::Tracker,
};

mod engine;
mod peer;
mod torrent;
mod tracker;

#[derive(Debug)]
pub(crate) struct EngineScope {
   pub(crate) live: LivePublisher<EngineStatus, CoreEventKind>,
}

/// One self-contained torrent projection tree.
#[derive(Debug)]
pub(crate) struct TorrentScope {
   pub(crate) info_hash: InfoHash,
   pub(crate) live: LivePublisher<Option<TorrentView>, TorrentEventKind>,
   pub(crate) peers: ScopeRegistry<PeerId, LiveHandle<PeerScope, PeerView, PeerEventKind>>,
   pub(crate) trackers:
      ScopeRegistry<TrackerId, LiveHandle<TrackerScope, TrackerView, TrackerEventKind>>,
   pub(crate) tracker_sources:
      ScopeRegistry<Tracker, LiveHandle<TrackerScope, TrackerView, TrackerEventKind>>,
   registered: AtomicBool,
   publication: Mutex<()>,
}

impl TorrentScope {
   pub(crate) fn new(info_hash: InfoHash, event_capacity: usize) -> Self {
      Self {
         info_hash,
         live: LivePublisher::new(None, event_capacity),
         peers: ScopeRegistry::new(),
         trackers: ScopeRegistry::new(),
         tracker_sources: ScopeRegistry::new(),
         registered: AtomicBool::new(false),
         publication: Mutex::new(()),
      }
   }

   pub(crate) fn register(&self) {
      self.registered.store(true, Ordering::Release);
   }

   pub(crate) fn is_registered(&self) -> bool {
      self.registered.load(Ordering::Acquire)
   }

   pub(crate) fn publication_lock(&self) -> MutexGuard<'_, ()> {
      self
         .publication
         .lock()
         .unwrap_or_else(std::sync::PoisonError::into_inner)
   }
}

/// Ownership root for transport-agnostic live projections.
#[derive(Debug)]
pub(crate) struct FrontendHub {
   pub(crate) engine: EngineScope,
   pub(crate) torrents: ScopeRegistry<InfoHash, TorrentScope>,
   pub(crate) handles: ScopeRegistry<InfoHash, TorrentInner>,
   pub(crate) settings: FrontendSettings,
   pub(crate) next_tracker_id: AtomicU64,
}

impl FrontendHub {
   pub(crate) fn torrent_handle(&self, info_hash: InfoHash) -> Option<Torrent> {
      self.handles.get(&info_hash).map(|inner| Torrent { inner })
   }
}
