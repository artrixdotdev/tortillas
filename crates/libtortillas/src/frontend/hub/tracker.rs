use std::sync::atomic::Ordering;

use super::super::{
   FrontendPublisher, TorrentEventKind, TrackerEventKind, TrackerHandle, TrackerView,
   handle::{TrackerId, TrackerScope},
};
use crate::{hashes::InfoHash, tracker::Tracker};

impl FrontendPublisher {
   pub(crate) fn tracker_handles(&self, torrent: InfoHash) -> Vec<TrackerHandle> {
      self
         .hub()
         .torrents
         .get(&torrent)
         .map_or_else(Vec::new, |scope| {
            scope
               .trackers
               .values()
               .into_iter()
               .map(|inner| TrackerHandle { inner })
               .collect()
         })
   }

   pub(crate) fn register_tracker_scope(
      &self, torrent: InfoHash, source: &Tracker, view: TrackerView,
   ) -> TrackerHandle {
      let hub = self.hub();
      let torrent_scope = self.ensure_torrent_scope(torrent);
      if let Some(inner) = torrent_scope.tracker_sources.get(source) {
         return TrackerHandle { inner };
      }
      let id = TrackerId::new(hub.next_tracker_id.fetch_add(1, Ordering::Relaxed));
      let identity = TrackerScope { torrent, id };
      let tracker = TrackerHandle::new(
         identity,
         view,
         self.downgrade(),
         hub.settings.tracker_event_capacity,
      );
      torrent_scope.trackers.insert(id, &tracker.inner);
      torrent_scope
         .tracker_sources
         .insert(source.clone(), &tracker.inner);
      tracker
   }

   pub(crate) fn emit_tracker_event(&self, tracker: &TrackerHandle, event: TrackerEventKind) {
      let Some(scope) = self.hub().torrents.get(&tracker.torrent()) else {
         return;
      };
      if scope.trackers.get(&tracker.id()).is_none() {
         return;
      }
      let torrent_event = match event {
         TrackerEventKind::AnnounceSucceeded { .. } => {
            TorrentEventKind::TrackerAnnounceSucceeded(tracker.clone())
         }
         TrackerEventKind::AnnounceFailed => {
            TorrentEventKind::TrackerAnnounceFailed(tracker.clone())
         }
         TrackerEventKind::Restarting => TorrentEventKind::TrackerRestarting(tracker.clone()),
         TrackerEventKind::Stopped => TorrentEventKind::TrackerStopped(tracker.clone()),
      };
      self.emit_without_torrent_view_change(&scope, torrent_event);
   }
}
