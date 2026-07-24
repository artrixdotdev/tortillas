use serde::{Deserialize, Serialize};

use crate::torrent::TorrentSnapshot;

/// Stable, frontend-ready view of the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineSnapshot {
   pub status: EngineStatus,
   pub torrent_count: u64,
   pub torrents: Vec<TorrentSnapshot>,
}

/// Coarse engine status for frontend displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineStatus {
   /// Runtime resources are still being initialized.
   Starting,
   /// The engine is accepting commands and managing torrents.
   Running,
   /// Graceful shutdown is in progress.
   Stopping,
   /// The engine and its managed torrents have stopped.
   Stopped,
}
