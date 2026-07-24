use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{errors::EngineError, torrent::TorrentSnapshot};

/// Current persistence schema version for [`EngineSnapshot`].
pub const ENGINE_SNAPSHOT_VERSION: u32 = 1;

/// Serializable state required to restore an engine's torrent sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSnapshot {
   pub version: u32,
   pub torrents: Vec<TorrentSnapshot>,
}

impl EngineSnapshot {
   /// Validates the engine schema and every contained torrent before restore.
   pub fn validate(&self) -> Result<(), EngineError> {
      if self.version != ENGINE_SNAPSHOT_VERSION {
         return Err(EngineError::InvalidSnapshot {
            reason: format!(
               "unsupported version {}; expected {}",
               self.version, ENGINE_SNAPSHOT_VERSION
            ),
         });
      }
      let mut unique = HashSet::with_capacity(self.torrents.len());
      for torrent in &self.torrents {
         torrent.validate()?;
         if !unique.insert(torrent.info_hash) {
            return Err(EngineError::InvalidSnapshot {
               reason: "snapshot contains duplicate torrent info hashes".to_string(),
            });
         }
      }
      Ok(())
   }
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
