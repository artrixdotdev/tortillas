use serde::{Deserialize, Serialize};

use crate::torrent::TorrentSnapshot;

/// Stable, frontend-ready view of the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineSnapshot {
   pub status: EngineStatus,
   pub torrent_count: usize,
   pub torrents: Vec<TorrentSnapshot>,
}

/// Coarse engine status for frontend displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineStatus {
   Running,
}
