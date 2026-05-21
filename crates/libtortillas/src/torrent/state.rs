use serde::{Deserialize, Serialize};

/// The current state of the torrent.
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
   Deserialize
)]
pub enum TorrentState {
   /// Torrent is downloading new pieces actively.
   Downloading,
   /// Torrent is seeding and has already completed the file.
   Seeding,
   /// Torrent is paused or currently inactive.
   #[default]
   Inactive,
}
