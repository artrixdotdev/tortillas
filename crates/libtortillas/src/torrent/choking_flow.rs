use std::collections::HashSet;

use tracing::{trace, warn};

use super::TorrentActor;
use crate::peer::{
   PeerStats,
   commands::{SetChoked, Stats},
};

impl TorrentActor {
   pub(super) async fn rechoke_peers(&mut self) {
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
      let mut snapshots = Vec::with_capacity(self.peers.len());

      for (peer_id, actor) in &self.peers {
         if !actor.is_alive() {
            continue;
         }

         match actor.ask(Stats).await {
            Ok(Some(stats)) => snapshots.push(stats),
            Ok(None) => trace!(%peer_id, "Peer stats unavailable"),
            Err(err) => warn!(?err, %peer_id, "Failed to collect peer stats"),
         }
      }

      snapshots
   }
}
