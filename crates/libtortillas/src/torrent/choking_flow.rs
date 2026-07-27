use std::collections::HashSet;

use futures::{StreamExt, stream};
use kameo::actor::ActorRef;
use tokio::time::timeout;
use tracing::{trace, warn};

use super::TorrentActor;
#[cfg(feature = "live")]
use crate::live::TorrentEventKind;
use crate::peer::{
   PeerActor, PeerId, PeerStats,
   commands::{SetChoked, Stats},
};

impl TorrentActor {
   pub(super) async fn rechoke_peers(&mut self) {
      if !self.state.is_transfer_active() {
         trace!(state = ?self.state, "Skipping rechoke while torrent is not transferring");
         return;
      }

      let peer_stats = self.peer_stats().await;
      let expired_requests = self
         .piece_scheduler
         .release_stale_requests(self.settings.torrent.peer_request_timeout);
      if expired_requests > 0 {
         trace!(expired_requests, "Released unanswered peer requests");
      }
      // Peer actors publish their own high-frequency samples. The torrent
      // publishes one coalesced aggregate after the collection interval.
      #[cfg(feature = "live")]
      self.publish_live_view(|view| TorrentEventKind::MetricsChanged(view.metrics.clone()));
      self.try_update_tracker_progress();
      let decision = self.choking_scheduler.decide(&peer_stats, self.state);
      let unchoked: HashSet<_> = decision.unchoked.iter().copied().collect();

      trace!(
         unchoked = decision.unchoked.len(),
         optimistic = ?decision.optimistic,
         "Applying choking decision"
      );

      for stats in peer_stats {
         let choked = !unchoked.contains(&stats.id);
         if stats.client_choking() == choked {
            continue;
         }

         let Some(actor) = self.peers.get(&stats.id) else {
            continue;
         };

         if let Err(err) = actor.tell(SetChoked { choked }).await {
            warn!(?err, peer_id = %stats.id, choked, "Failed to update peer choke state");
         }
      }

      // Recover work released by peers that rejected requests or disconnected
      // between collection intervals. Filling to a target size is idempotent,
      // so this cannot grow a peer beyond its configured request window.
      self.fill_all_peer_request_windows();
   }

   async fn peer_stats(&self) -> Vec<PeerStats> {
      let peer_stats_timeout = self.settings.torrent.peer_stats_timeout;
      let peer_stats_concurrency = self.settings.torrent.peer_stats_concurrency.max(1);
      let actor_refs: Vec<(PeerId, ActorRef<PeerActor>)> = self
         .peers
         .iter()
         .filter(|(_, actor)| actor.is_alive())
         .map(|(peer_id, actor)| (*peer_id, actor.clone()))
         .collect();

      stream::iter(actor_refs)
         .map(|(peer_id, actor)| async move {
            match timeout(peer_stats_timeout, actor.ask(Stats)).await {
               Ok(Ok(Some(stats))) => Some(stats),
               Ok(Ok(None)) => {
                  trace!(%peer_id, "Peer stats unavailable");
                  None
               }
               Ok(Err(err)) => {
                  warn!(?err, %peer_id, "Failed to collect peer stats");
                  None
               }
               Err(_) => {
                  trace!(%peer_id, "Timed out collecting peer stats");
                  None
               }
            }
         })
         .buffer_unordered(peer_stats_concurrency)
         .filter_map(std::future::ready)
         .collect()
         .await
   }
}
