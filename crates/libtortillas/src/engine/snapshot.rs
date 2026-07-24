use serde::{Deserialize, Serialize};

use crate::torrent::TorrentSnapshot;

/// Current persistence schema version for [`EngineSnapshot`].
pub const ENGINE_SNAPSHOT_VERSION: u32 = 1;

/// Serializable state required to restore an engine's torrent sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSnapshot {
   pub version: u32,
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
