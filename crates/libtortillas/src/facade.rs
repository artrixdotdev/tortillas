//! Frontend-facing facade for `libtortillas`.
//!
//! This module defines the stable surface that a TUI or another frontend should
//! prefer over actor, protocol, tracker, and storage internals. Lower-level
//! modules remain public for advanced integrations, but frontend code should be
//! able to model user intent, observe progress, and hold handles through the
//! types in this module.
//!
//! # Example
//!
//! ```no_run
//! use libtortillas::facade::{CoreCommand, EngineHandle, TorrentSource};
//!
//! let engine = EngineHandle::default();
//! let command = CoreCommand::AddTorrent {
//!    source: TorrentSource::magnet("magnet:?xt=urn:btih:..."),
//! };
//! ```

use std::{net::SocketAddr, path::PathBuf};

use crate::{engine::Engine, hashes::InfoHash, torrent::Torrent};
pub use crate::{
   engine::{EngineSnapshot, EngineStatus, TorrentSource},
   torrent::{TorrentProgressSnapshot, TorrentSnapshot, TorrentTransferSnapshot},
};

/// Stable handle used by frontends to manage the torrent engine.
///
/// This is currently backed by [`Engine`]. Frontends should import the alias
/// from the facade so future internal handle changes do not require reaching
/// into the engine module directly.
pub type EngineHandle = Engine;

/// Stable handle used by frontends to inspect and control one torrent.
///
/// This is currently backed by [`Torrent`]. Frontends should import the alias
/// from the facade so future internal handle changes do not require reaching
/// into the torrent module directly.
pub type TorrentHandle = Torrent;

/// Commands a frontend can model before sending work to the engine.
///
/// The existing handle methods remain the runtime API today. This enum gives
/// future TUIs and tests a single typed vocabulary for user intent while
/// command dispatch is filled in by follow-up issues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreCommand {
   /// Add a torrent from an explicit source.
   AddTorrent { source: TorrentSource },
   /// Start every torrent managed by the engine.
   StartAll,
   /// Start one torrent.
   StartTorrent { torrent: InfoHash },
   /// Resume one torrent.
   ResumeTorrent { torrent: InfoHash },
   /// Pause one torrent.
   PauseTorrent { torrent: InfoHash },
   /// Stop one torrent.
   StopTorrent { torrent: InfoHash },
   /// Remove one torrent from the engine.
   RemoveTorrent { torrent: InfoHash },
   /// Gracefully shut down the engine.
   Shutdown,
   /// Change the output folder for one torrent.
   SetTorrentOutputPath { torrent: InfoHash, path: PathBuf },
   /// Enable or disable autostart for one torrent.
   SetAutostart { torrent: InfoHash, enabled: bool },
   /// Set the peer threshold required before autostart begins downloading.
   SetSufficientPeers { torrent: InfoHash, peers: usize },
}

/// Events a frontend can subscribe to without depending on actor messages.
#[derive(Debug, Clone, PartialEq)]
pub enum CoreEvent {
   /// The engine has an updated aggregate snapshot.
   EngineUpdated(EngineSnapshot),
   /// A torrent has been added.
   TorrentAdded(TorrentSnapshot),
   /// A torrent has changed state or metrics.
   TorrentUpdated(TorrentSnapshot),
   /// A torrent has been removed from the engine.
   TorrentRemoved { torrent: InfoHash },
   /// A frontend-relevant error occurred.
   Error { message: String },
}

/// Frontend snapshot of a connected or discovered peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSnapshot {
   /// Network address for the peer, when known.
   pub address: Option<SocketAddr>,
   /// Peer client identifier, redacted or formatted for display.
   pub client: Option<String>,
   /// Whether the peer is currently connected.
   pub connected: bool,
   /// Bytes downloaded from this peer.
   pub downloaded_bytes: u64,
   /// Bytes uploaded to this peer.
   pub uploaded_bytes: u64,
}

/// Frontend snapshot of a tracker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerSnapshot {
   /// Announce URL or frontend label for the tracker.
   pub announce_url: String,
   /// Last known tracker status.
   pub status: TrackerStatus,
   /// Number of peers most recently returned by this tracker.
   pub peers_returned: Option<usize>,
   /// Last frontend-safe error message reported by this tracker.
   pub last_error: Option<String>,
}

/// Frontend status for tracker health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerStatus {
   /// The tracker has not been contacted yet.
   Pending,
   /// The last tracker request succeeded.
   Healthy,
   /// The tracker is temporarily unreachable or returned an error.
   Degraded,
   /// The tracker is unsupported or permanently unusable.
   Unusable,
}
