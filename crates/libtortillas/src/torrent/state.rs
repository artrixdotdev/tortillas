use kameo::Reply;
use serde::{Deserialize, Serialize};

/// The current lifecycle state of a torrent.
///
/// Expected transition shape:
/// `Added` or `ResolvingMetadata` -> `Ready` -> `Downloading` -> `Seeding`.
/// Future frontend commands may also move a torrent through `Paused`,
/// `Stopping`, `Stopped`, or `Failed` without collapsing those states into a
/// generic inactive bucket.
#[derive(
   Debug,
   Default,
   Clone,
   Copy,
   PartialEq,
   Eq,
   PartialOrd,
   Ord,
   Serialize,
   Deserialize,
   Reply
)]
pub enum TorrentState {
   /// Torrent is registered but not ready to transfer yet.
   #[default]
   Added,
   /// Torrent was added from a metadata source such as a magnet URI and is
   /// waiting for the info dict before it can start.
   ResolvingMetadata,
   /// Torrent has enough metadata and peers to start, but autostart is off.
   Ready,
   /// Torrent is downloading new pieces actively.
   Downloading,
   /// Torrent is intentionally paused by a frontend or caller.
   Paused,
   /// Torrent is seeding and has already completed the file.
   Seeding,
   /// Torrent is shutting down actors, peers, or trackers.
   Stopping,
   /// Torrent has completed shutdown.
   Stopped,
   /// Torrent hit an unrecoverable runtime failure.
   Failed,
}

impl TorrentState {
   /// Returns `true` when peers should be rechoked and piece requests may flow.
   pub const fn is_transfer_active(self) -> bool {
      matches!(self, Self::Downloading | Self::Seeding)
   }

   /// Returns `true` when the torrent can transition to `Ready` or start.
   pub const fn can_become_ready(self) -> bool {
      matches!(self, Self::Added | Self::ResolvingMetadata | Self::Ready)
   }

   /// Returns `true` when `start` should attempt to enter active transfer.
   pub const fn can_start(self) -> bool {
      matches!(
         self,
         Self::Added | Self::ResolvingMetadata | Self::Ready | Self::Paused
      )
   }
}

#[cfg(test)]
mod tests {
   use super::TorrentState;

   #[test]
   fn torrent_state_only_treats_transfer_states_as_active() {
      assert!(TorrentState::Downloading.is_transfer_active());
      assert!(TorrentState::Seeding.is_transfer_active());

      assert!(!TorrentState::Added.is_transfer_active());
      assert!(!TorrentState::Ready.is_transfer_active());
      assert!(!TorrentState::Paused.is_transfer_active());
      assert!(!TorrentState::Stopped.is_transfer_active());
   }

   #[test]
   fn torrent_state_distinguishes_ready_candidates_from_paused_torrents() {
      assert!(TorrentState::Added.can_become_ready());
      assert!(TorrentState::ResolvingMetadata.can_become_ready());
      assert!(TorrentState::Ready.can_become_ready());

      assert!(!TorrentState::Paused.can_become_ready());
      assert!(!TorrentState::Downloading.can_become_ready());
      assert!(!TorrentState::Failed.can_become_ready());
   }

   #[test]
   fn torrent_state_allows_manual_start_from_paused() {
      assert!(TorrentState::Paused.can_start());

      assert!(!TorrentState::Downloading.can_start());
      assert!(!TorrentState::Seeding.can_start());
      assert!(!TorrentState::Stopping.can_start());
      assert!(!TorrentState::Stopped.can_start());
   }
}
