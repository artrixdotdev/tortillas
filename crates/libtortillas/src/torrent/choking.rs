use crate::{
   peer::{PeerId, PeerStats},
   torrent::TorrentState,
};

pub(crate) const DEFAULT_UPLOAD_SLOTS: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChokingDecision {
   pub(crate) unchoked: Vec<PeerId>,
}

pub(crate) fn select_unchoked_peers(
   peers: &[PeerStats], _torrent_state: TorrentState, upload_slots: usize,
) -> ChokingDecision {
   ChokingDecision {
      unchoked: peers
         .iter()
         .filter(|peer| peer.interested)
         .take(upload_slots)
         .map(|peer| peer.id)
         .collect(),
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

   #[test]
   fn selector_only_includes_interested_peers() {
      let mut not_interested = stats(2);
      not_interested.interested = false;
      let peers = [stats(1), not_interested, stats(3)];

      let decision = select_unchoked_peers(&peers, TorrentState::Downloading, DEFAULT_UPLOAD_SLOTS);

      assert_eq!(decision.unchoked, vec![peer_id(1), peer_id(3)]);
   }

   #[test]
   fn selector_respects_upload_slot_limit() {
      let peers = [stats(1), stats(2), stats(3), stats(4), stats(5)];

      let decision = select_unchoked_peers(&peers, TorrentState::Downloading, DEFAULT_UPLOAD_SLOTS);

      assert_eq!(decision.unchoked.len(), DEFAULT_UPLOAD_SLOTS);
      assert_eq!(
         decision.unchoked,
         vec![peer_id(1), peer_id(2), peer_id(3), peer_id(4)]
      );
   }
}
