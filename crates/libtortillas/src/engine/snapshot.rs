use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::{errors::EngineError, torrent::TorrentSnapshot};

/// Current persistence schema version for [`EngineSnapshot`].
pub const ENGINE_SNAPSHOT_VERSION: u32 = 2;

/// Serializable state required to restore an engine's torrent sessions.
#[derive(Debug, Clone, Serialize)]
pub struct EngineSnapshot {
   pub version: u32,
   pub torrents: Vec<TorrentSnapshot>,
}

#[derive(Deserialize)]
struct EngineSnapshotWire {
   version: u32,
   torrents: Vec<TorrentSnapshot>,
}

impl<'de> Deserialize<'de> for EngineSnapshot {
   fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
      let wire = EngineSnapshotWire::deserialize(deserializer)?;
      Ok(Self {
         version: if wire.version == 1 {
            ENGINE_SNAPSHOT_VERSION
         } else {
            wire.version
         },
         torrents: wire.torrents,
      })
   }
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

/// Coarse engine status for live-state consumers.
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
