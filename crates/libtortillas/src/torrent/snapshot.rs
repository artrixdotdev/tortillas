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
   pub sufficient_peers: usize,
   pub peer_count: usize,
   pub tracker_count: usize,
   pub output_path: Option<PathBuf>,
   pub progress: TorrentProgressSnapshot,
   pub transfer: TorrentTransferSnapshot,
}

/// Display-ready torrent progress fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TorrentProgressSnapshot {
   pub total_bytes: Option<usize>,
   pub downloaded_bytes: usize,
   pub bytes_remaining: Option<usize>,
   pub progress_fraction: Option<f64>,
   pub completed_pieces: usize,
   pub partial_pieces: usize,
   pub total_pieces: usize,
}

/// Transfer-rate fields reserved for frontend displays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentTransferSnapshot {
   pub download_rate_bytes_per_second: Option<u64>,
   pub upload_rate_bytes_per_second: Option<u64>,
   pub eta_seconds: Option<u64>,
}
