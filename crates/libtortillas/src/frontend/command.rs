use std::path::PathBuf;

use crate::{engine::TorrentSource, hashes::InfoHash, torrent::Torrent};

/// Typed message accepted by [`Engine::send`](crate::engine::Engine::send).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CoreCommand {
   AddTorrent {
      source: TorrentSource,
   },
   StartAll,
   /// Sends an existing torrent command through the engine by info hash.
   Torrent {
      torrent: InfoHash,
      command: TorrentCommand,
   },
   RemoveTorrent {
      torrent: InfoHash,
   },
   Shutdown,
}

/// Typed message accepted by [`Torrent::send`](crate::torrent::Torrent::send).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TorrentCommand {
   Start,
   Pause,
   SetOutputPath(PathBuf),
   SetAutostart(bool),
   SetSufficientPeers(usize),
}

/// Result of sending a [`CoreCommand`] to an engine.
#[derive(Debug, Clone)]
#[must_use]
pub enum CoreCommandResult {
   /// The command was applied and has no new handle to return.
   Applied,
   /// An add command created this torrent handle.
   TorrentAdded(Torrent),
}
