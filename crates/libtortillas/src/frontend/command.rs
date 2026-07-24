use std::path::PathBuf;

use crate::{engine::TorrentSource, hashes::InfoHash, torrent::Torrent};

/// Typed message accepted by [`Engine::send`](crate::engine::Engine::send).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CoreCommand {
   AddTorrent { source: TorrentSource },
   StartAll,
   StartTorrent { torrent: InfoHash },
   ResumeTorrent { torrent: InfoHash },
   PauseTorrent { torrent: InfoHash },
   StopTorrent { torrent: InfoHash },
   RemoveTorrent { torrent: InfoHash },
   Shutdown,
   SetTorrentOutputPath { torrent: InfoHash, path: PathBuf },
   SetAutostart { torrent: InfoHash, enabled: bool },
   SetSufficientPeers { torrent: InfoHash, peers: usize },
}

/// Typed message accepted by [`Torrent::send`](crate::torrent::Torrent::send).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TorrentCommand {
   Start,
   Resume,
   Pause,
   Stop,
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
