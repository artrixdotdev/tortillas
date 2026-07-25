use super::super::{FrontendPublisher, PeerHandle, PeerView, TorrentEventKind, handle::PeerScope};
use crate::hashes::InfoHash;

impl FrontendPublisher {
   pub(crate) fn peer_handles(&self, torrent: InfoHash) -> Vec<PeerHandle> {
      self
         .hub()
         .torrents
         .get(&torrent)
         .map_or_else(Vec::new, |scope| {
            scope
               .peers
               .values()
               .into_iter()
               .map(|inner| PeerHandle { inner })
               .filter(|peer| peer.view().connected)
               .collect()
         })
   }

   pub(crate) fn register_peer_scope(&self, identity: PeerScope, view: PeerView) -> PeerHandle {
      let hub = self.hub();
      let scope = self.ensure_torrent_scope(identity.torrent);
      let peer = PeerHandle::new(
         identity,
         view,
         self.downgrade(),
         hub.settings.peer_event_capacity,
      );
      scope.peers.insert(identity.peer, &peer.inner);
      peer
   }

   pub(crate) fn emit_peer_connected(&self, peer: &PeerHandle) {
      let Some(scope) = self.hub().torrents.get(&peer.torrent()) else {
         return;
      };
      if scope.peers.get(&peer.id()).is_some() {
         self.emit_without_torrent_view_change(
            &scope,
            TorrentEventKind::PeerConnected(peer.clone()),
         );
      }
   }

   pub(crate) fn mark_peer_disconnected(&self, peer: &PeerHandle) {
      let Some(scope) = self.hub().torrents.get(&peer.torrent()) else {
         return;
      };
      if scope.peers.remove(&peer.id()).is_none() {
         return;
      }
      self.emit_without_torrent_view_change(
         &scope,
         TorrentEventKind::PeerDisconnected(peer.clone()),
      );
   }

   pub(crate) fn close_peer_scopes_for_torrent_restart(&self, torrent: InfoHash) {
      let Some(scope) = self.hub().torrents.get(&torrent) else {
         return;
      };
      for inner in scope.peers.remove_all() {
         PeerHandle { inner }.close_without_parent_event();
      }
   }
}
