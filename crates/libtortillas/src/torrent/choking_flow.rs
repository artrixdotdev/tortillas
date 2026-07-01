use std::collections::HashSet;

use futures::{StreamExt, stream};
use kameo::actor::ActorRef;
use tokio::time::timeout;
use tracing::{trace, warn};

use super::TorrentActor;
use crate::{
   peer::{
      PeerActor, PeerStats,
      commands::{SetChoked, Stats},
   },
   torrent::TorrentState,
};

impl TorrentActor {
   pub(super) async fn rechoke_peers(&mut self) {
      if !matches!(
         self.state,
         TorrentState::Downloading | TorrentState::Seeding
      ) {
         trace!(state = ?self.state, "Skipping rechoke while torrent is inactive");
         return;
      }

      let peer_stats = self.peer_stats().await;
      let decision = self.choking_scheduler.decide(&peer_stats, self.state);
      let unchoked: HashSet<_> = decision.unchoked.iter().copied().collect();

      trace!(
         unchoked = decision.unchoked.len(),
         optimistic = ?decision.optimistic,
         "Applying choking decision"
      );

      for stats in peer_stats {
         let choked = !unchoked.contains(&stats.id);
         if stats.choked == choked {
            continue;
         }

         let Some(actor) = self.peers.get(&stats.id) else {
            continue;
         };

         if let Err(err) = actor.tell(SetChoked { choked }).await {
            warn!(?err, peer_id = %stats.id, choked, "Failed to update peer choke state");
         }
      }
   }

   async fn peer_stats(&self) -> Vec<PeerStats> {
      let peer_stats_timeout = self.settings.torrent.peer_stats_timeout;
      let actor_refs: Vec<(crate::peer::PeerId, ActorRef<PeerActor>)> = self
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
         .buffer_unordered(self.settings.torrent.peer_stats_concurrency)
         .filter_map(std::future::ready)
         .collect()
         .await
   }
}
