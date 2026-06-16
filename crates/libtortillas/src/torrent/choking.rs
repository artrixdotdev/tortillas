use std::time::Duration;

use crate::{
   peer::{PeerId, PeerStats},
   torrent::TorrentState,
};

pub(crate) const DEFAULT_UPLOAD_SLOTS: usize = 4;
pub(crate) const RECHOKE_INTERVAL: Duration = Duration::from_secs(10);
pub(crate) const OPTIMISTIC_UNCHOKE_ROUNDS: usize = 3;

#[derive(Clone, Debug)]
pub(crate) struct ChokingScheduler {
   upload_slots: usize,
   optimistic_unchoke_rounds: usize,
   optimistic_index: usize,
   rounds_since_optimistic_rotation: usize,
}

impl Default for ChokingScheduler {
   fn default() -> Self {
      Self::new(DEFAULT_UPLOAD_SLOTS, OPTIMISTIC_UNCHOKE_ROUNDS)
   }
}

impl ChokingScheduler {
   pub(crate) fn new(upload_slots: usize, optimistic_unchoke_rounds: usize) -> Self {
      Self {
         upload_slots,
         optimistic_unchoke_rounds: optimistic_unchoke_rounds.max(1),
         optimistic_index: 0,
         rounds_since_optimistic_rotation: 0,
      }
   }

   pub(crate) fn decide(
      &mut self, peers: &[PeerStats], torrent_state: TorrentState,
   ) -> ChokingDecision {
      let decision = select_unchoked_peers(
         peers,
         torrent_state,
         self.upload_slots,
         self.optimistic_index,
      );

      if decision.optimistic.is_some() {
         self.rounds_since_optimistic_rotation += 1;
         if self.rounds_since_optimistic_rotation >= self.optimistic_unchoke_rounds {
            self.rounds_since_optimistic_rotation = 0;
            self.optimistic_index = self.optimistic_index.wrapping_add(1);
         }
      } else {
         self.rounds_since_optimistic_rotation = 0;
         self.optimistic_index = 0;
      }

      decision
   }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChokingDecision {
   pub(crate) unchoked: Vec<PeerId>,
   pub(crate) optimistic: Option<PeerId>,
}

pub(crate) fn select_unchoked_peers(
   peers: &[PeerStats], torrent_state: TorrentState, upload_slots: usize, optimistic_index: usize,
) -> ChokingDecision {
   let mut candidates: Vec<_> = peers
      .iter()
      .filter(|peer| peer.interested)
      .copied()
      .collect();
   candidates.sort_by(|left, right| {
      rate_for(right, torrent_state)
         .cmp(&rate_for(left, torrent_state))
         .then_with(|| left.id.id().cmp(right.id.id()))
   });

   if upload_slots == 0 {
      return ChokingDecision {
         unchoked: Vec::new(),
         optimistic: None,
      };
   }

   let reserve_optimistic_slot = candidates.len() > upload_slots;
   let regular_slots = if reserve_optimistic_slot {
      upload_slots.saturating_sub(1)
   } else {
      upload_slots
   };

   let mut unchoked: Vec<_> = candidates
      .iter()
      .take(regular_slots)
      .map(|peer| peer.id)
      .collect();

   let optimistic = if reserve_optimistic_slot {
      let optimistic_candidates = &candidates[regular_slots..];
      let peer = optimistic_candidates[optimistic_index % optimistic_candidates.len()].id;
      unchoked.push(peer);
      Some(peer)
   } else {
      None
   };

   ChokingDecision {
      unchoked,
      optimistic,
   }
}

fn rate_for(peer: &PeerStats, torrent_state: TorrentState) -> usize {
   match torrent_state {
      TorrentState::Downloading => peer.download_rate,
      TorrentState::Seeding => peer.upload_rate,
      TorrentState::Inactive => 0,
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   fn peer_id(value: u8) -> PeerId {
      PeerId::from([value; 20])
   }

   fn stats(id: u8) -> PeerStats {
      PeerStats {
         id: peer_id(id),
         interested: true,
         choked: true,
         download_rate: 0,
         upload_rate: 0,
         bytes_downloaded: 0,
         bytes_uploaded: 0,
      }
   }

   fn with_rates(id: u8, download_rate: usize, upload_rate: usize) -> PeerStats {
      PeerStats {
         download_rate,
         upload_rate,
         ..stats(id)
      }
   }

   #[test]
   fn selector_only_includes_interested_peers() {
      let mut not_interested = stats(2);
      not_interested.interested = false;
      let peers = [stats(1), not_interested, stats(3)];

      let decision =
         select_unchoked_peers(&peers, TorrentState::Downloading, DEFAULT_UPLOAD_SLOTS, 0);

      assert_eq!(decision.unchoked, vec![peer_id(1), peer_id(3)]);
      assert_eq!(decision.optimistic, None);
   }

   #[test]
   fn selector_respects_upload_slot_limit() {
      let peers = [stats(1), stats(2), stats(3), stats(4), stats(5)];

      let decision =
         select_unchoked_peers(&peers, TorrentState::Downloading, DEFAULT_UPLOAD_SLOTS, 0);

      assert_eq!(decision.unchoked.len(), DEFAULT_UPLOAD_SLOTS);
      assert_eq!(decision.optimistic, Some(peer_id(4)));
      assert_eq!(
         decision.unchoked,
         vec![peer_id(1), peer_id(2), peer_id(3), peer_id(4)]
      );
   }

   #[test]
   fn selector_chokes_everyone_without_upload_slots() {
      let peers = [stats(1), stats(2), stats(3)];

      let decision = select_unchoked_peers(&peers, TorrentState::Downloading, 0, 0);

      assert!(decision.unchoked.is_empty());
      assert_eq!(decision.optimistic, None);
   }

   #[test]
   fn selector_ranks_downloading_peers_by_download_rate() {
      let peers = [
         with_rates(1, 30, 500),
         with_rates(2, 90, 10),
         with_rates(3, 60, 1_000),
      ];

      let decision = select_unchoked_peers(&peers, TorrentState::Downloading, 2, 0);

      assert_eq!(decision.unchoked, vec![peer_id(2), peer_id(3)]);
      assert_eq!(decision.optimistic, Some(peer_id(3)));
   }

   #[test]
   fn selector_ranks_seeding_peers_by_upload_rate() {
      let peers = [
         with_rates(1, 100, 20),
         with_rates(2, 10, 80),
         with_rates(3, 60, 40),
      ];

      let decision = select_unchoked_peers(&peers, TorrentState::Seeding, 2, 0);

      assert_eq!(decision.unchoked, vec![peer_id(2), peer_id(3)]);
      assert_eq!(decision.optimistic, Some(peer_id(3)));
   }

   #[test]
   fn selector_rotates_optimistic_unchoke() {
      let peers = [
         with_rates(1, 100, 0),
         with_rates(2, 90, 0),
         with_rates(3, 80, 0),
         with_rates(4, 70, 0),
      ];

      let first = select_unchoked_peers(&peers, TorrentState::Downloading, 2, 0);
      let second = select_unchoked_peers(&peers, TorrentState::Downloading, 2, 1);

      assert_eq!(first.unchoked, vec![peer_id(1), peer_id(2)]);
      assert_eq!(first.optimistic, Some(peer_id(2)));
      assert_eq!(second.unchoked, vec![peer_id(1), peer_id(3)]);
      assert_eq!(second.optimistic, Some(peer_id(3)));
   }

   #[test]
   fn scheduler_rotates_optimistic_unchoke_after_configured_rounds() {
      let peers = [
         with_rates(1, 100, 0),
         with_rates(2, 90, 0),
         with_rates(3, 80, 0),
         with_rates(4, 70, 0),
      ];
      let mut scheduler = ChokingScheduler::new(2, 3);

      let first = scheduler.decide(&peers, TorrentState::Downloading);
      let second = scheduler.decide(&peers, TorrentState::Downloading);
      let third = scheduler.decide(&peers, TorrentState::Downloading);
      let fourth = scheduler.decide(&peers, TorrentState::Downloading);

      assert_eq!(first.optimistic, Some(peer_id(2)));
      assert_eq!(second.optimistic, Some(peer_id(2)));
      assert_eq!(third.optimistic, Some(peer_id(2)));
      assert_eq!(fourth.optimistic, Some(peer_id(3)));
   }
}
