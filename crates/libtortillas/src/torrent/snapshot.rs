use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::TorrentState;
use crate::hashes::InfoHash;

/// Stable, frontend-ready view of a torrent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TorrentSnapshot {
   pub info_hash: InfoHash,
   pub name: String,
   pub state: TorrentState,
   pub has_metadata: bool,
   pub is_ready: bool,
   pub auto_start: bool,
   pub sufficient_peers: u64,
   pub peer_count: u64,
   pub tracker_count: u64,
   pub output_path: Option<PathBuf>,
   pub progress: TorrentProgressSnapshot,
   pub transfer: TorrentTransferSnapshot,
}

/// Display-ready torrent progress fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TorrentProgressSnapshot {
   pub total_bytes: Option<u64>,
   pub downloaded_bytes: u64,
   pub bytes_remaining: Option<u64>,
   pub progress_fraction: Option<f64>,
   pub completed_pieces: u64,
   pub partial_pieces: u64,
   pub total_pieces: u64,
}

/// Transfer-rate fields reserved for frontend displays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentTransferSnapshot {
   pub download_rate_bytes_per_second: Option<u64>,
   pub upload_rate_bytes_per_second: Option<u64>,
   pub eta_seconds: Option<u64>,
}
