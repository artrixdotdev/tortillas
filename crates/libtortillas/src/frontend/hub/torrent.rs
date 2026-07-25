use std::sync::Arc;

use super::super::{
   CoreEventKind, FrontendHealth, FrontendHealthLevel, FrontendPublisher, PeerHandle,
   TorrentEventKind, TorrentScope, TorrentView, TrackerHandle,
};
use crate::{hashes::InfoHash, torrent::Torrent};

impl FrontendPublisher {
   pub(crate) fn ensure_torrent_scope(&self, info_hash: InfoHash) -> Arc<TorrentScope> {
      let hub = self.hub();
      hub.torrents.get_or_insert_with(info_hash, || {
         TorrentScope::new(info_hash, hub.settings.torrent_event_capacity)
      })
   }

   pub(crate) fn torrent_handle(&self, torrent: InfoHash) -> Option<Torrent> {
      self.hub().torrent_handle(torrent)
   }

   #[cfg(test)]
   pub(crate) fn torrent_view(&self, torrent: InfoHash) -> Option<TorrentView> {
      self
         .hub()
         .torrents
         .get(&torrent)
         .and_then(|scope| scope.live.view())
   }

   pub(crate) fn initialize_torrent_projection(&self, torrent: TorrentView) {
      let scope = self.ensure_torrent_scope(torrent.info_hash);
      let _ = scope.live.set_view(Some(torrent));
   }

   pub(crate) fn register_torrent_scope(&self, torrent: Torrent) {
      let info_hash = torrent.info_hash();
      let scope = self.ensure_torrent_scope(info_hash);
      self.hub().handles.insert(info_hash, &torrent.inner);
      scope.register();
      if let Some(view) = scope.live.view() {
         self.replace_torrent_view_and_emit(view, TorrentEventKind::Added);
      }
   }

   pub(crate) fn replace_torrent_view_and_emit(
      &self, torrent: TorrentView, event: TorrentEventKind,
   ) {
      let info_hash = torrent.info_hash;
      let Some(scope) = self.hub().torrents.get(&info_hash) else {
         return;
      };
      let Some(handle) = self.torrent_handle(info_hash) else {
         return;
      };
      let _publication = scope.publication_lock();
      if !scope.live.update(Some(torrent), event.clone()) {
         return;
      }
      let _ = self.hub().engine.live.publish(CoreEventKind::Torrent {
         torrent: handle,
         event,
      });
   }

   pub(crate) fn emit_health(
      &self, torrent: Option<InfoHash>, level: FrontendHealthLevel, message: impl Into<String>,
   ) {
      let health = FrontendHealth {
         torrent,
         level,
         message: message.into(),
      };
      if let Some(info_hash) = torrent
         && let Some(scope) = self.hub().torrents.get(&info_hash)
      {
         self.emit_without_torrent_view_change(&scope, TorrentEventKind::Health(health));
      } else {
         let _ = self
            .hub()
            .engine
            .live
            .publish(CoreEventKind::Health(health));
      }
   }

   pub(crate) fn remove_torrent_scope(&self, info_hash: InfoHash) {
      let Some(scope) = self.hub().torrents.get(&info_hash) else {
         return;
      };
      let torrent = self.torrent_handle(info_hash);
      let peers = scope
         .peers
         .values()
         .into_iter()
         .map(|inner| PeerHandle { inner })
         .collect::<Vec<_>>();
      let trackers = scope
         .trackers
         .values()
         .into_iter()
         .map(|inner| TrackerHandle { inner })
         .collect::<Vec<_>>();
      let publication = scope.publication_lock();

      for peer in peers {
         peer.close_without_parent_event();
      }
      for tracker in trackers {
         tracker.close_without_parent_event();
      }

      if !scope.live.close(None, TorrentEventKind::Removed) {
         return;
      }
      drop(publication);
      self.hub().torrents.remove(&info_hash);
      self.hub().handles.remove(&info_hash);
      if let Some(torrent) = torrent {
         let _ = self.hub().engine.live.publish(CoreEventKind::Torrent {
            torrent,
            event: TorrentEventKind::Removed,
         });
      }
   }

   pub(super) fn emit_without_torrent_view_change(
      &self, scope: &TorrentScope, event: TorrentEventKind,
   ) {
      let Some(torrent) = self.torrent_handle(scope.info_hash) else {
         return;
      };
      let _publication = scope.publication_lock();
      if !scope.live.publish(event.clone()) {
         return;
      }
      let _ = self
         .hub()
         .engine
         .live
         .publish(CoreEventKind::Torrent { torrent, event });
   }
}
