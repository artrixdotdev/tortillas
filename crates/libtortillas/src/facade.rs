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
//!    source: TorrentSource::MagnetUri("magnet:?xt=urn:btih:...".to_string()),
//! };
//! ```

use std::{collections::HashSet, net::SocketAddr, path::PathBuf, time::Duration};

use crate::{
   engine::{Engine, EngineExport},
   hashes::InfoHash,
   metainfo::{Info, MetaInfo},
   torrent::{Torrent, TorrentExport, TorrentState},
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

/// Explicit torrent sources accepted by frontend commands.
///
/// Runtime support for every variant is implemented by follow-up work. This
/// type is the stable contract a frontend can use instead of guessing source
/// kind from an arbitrary string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TorrentSource {
   /// A BitTorrent magnet URI.
   MagnetUri(String),
   /// A local `.torrent` file path selected by the frontend.
   TorrentFilePath(PathBuf),
   /// Raw bytes from a `.torrent` file provided by the frontend.
   TorrentFileBytes(Vec<u8>),
   /// A remote HTTP or HTTPS `.torrent` URL.
   RemoteTorrentUrl(String),
}

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
   /// Pause one torrent.
   PauseTorrent { torrent: InfoHash },
   /// Change the output folder for one torrent.
   SetTorrentOutputPath { torrent: InfoHash, path: PathBuf },
   /// Enable or disable autostart for one torrent.
   SetAutostart { torrent: InfoHash, enabled: bool },
   /// Set the peer threshold required before autostart begins downloading.
   SetSufficientPeers { torrent: InfoHash, peers: usize },
}

/// Events a frontend can subscribe to without depending on actor messages.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Frontend snapshot of the engine.
///
/// This intentionally contains only stable frontend concepts. Internal actor
/// references, raw protocol streams, tracker clients, and storage managers do
/// not belong here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineSnapshot {
   /// Torrent snapshots currently known to the engine.
   pub torrents: Vec<TorrentSnapshot>,
}

impl EngineSnapshot {
   /// Builds a frontend snapshot from the current lower-level engine export.
   pub fn from_export(export: EngineExport) -> Self {
      export.into()
   }
}

impl From<EngineExport> for EngineSnapshot {
   fn from(export: EngineExport) -> Self {
      Self {
         torrents: export
            .torrents
            .into_iter()
            .map(TorrentSnapshot::from)
            .collect(),
      }
   }
}

/// Frontend snapshot of a torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentSnapshot {
   /// Stable torrent identifier.
   pub info_hash: InfoHash,
   /// Display name from torrent metadata, when available.
   pub name: Option<String>,
   /// Current torrent lifecycle state.
   pub state: TorrentState,
   /// Download progress and transfer-rate data.
   pub progress: TorrentProgressSnapshot,
   /// Known peers for this torrent.
   pub peers: Vec<PeerSnapshot>,
   /// Known trackers for this torrent.
   pub trackers: Vec<TrackerSnapshot>,
   /// Effective output folder for this torrent, when configured.
   pub output_path: Option<PathBuf>,
}

impl TorrentSnapshot {
   /// Builds a frontend snapshot from the current lower-level torrent export.
   pub fn from_export(export: TorrentExport) -> Self {
      export.into()
   }
}

impl From<TorrentExport> for TorrentSnapshot {
   fn from(export: TorrentExport) -> Self {
      let info = snapshot_info(&export.metainfo, export.info_dict.as_ref());
      let total_bytes = info.map_or(0, |info| info.total_length() as u64);
      let piece_length = info.map_or(0, |info| info.piece_length);
      let verified_bytes = export
         .bitfield
         .iter()
         .by_vals()
         .enumerate()
         .filter(|(_, is_verified)| *is_verified)
         .map(|(piece_index, _)| {
            let piece_start = (piece_index as u64).saturating_mul(piece_length);
            total_bytes.saturating_sub(piece_start).min(piece_length)
         })
         .fold(0_u64, u64::saturating_add);
      let mut tracker_urls = HashSet::new();

      Self {
         info_hash: export.info_hash,
         name: info.map(|info| info.name.clone()),
         state: export.state,
         progress: TorrentProgressSnapshot {
            verified_bytes,
            total_bytes,
            download_rate_bytes_per_second: 0,
            upload_rate_bytes_per_second: 0,
            eta: None,
         },
         peers: Vec::new(),
         trackers: export
            .metainfo
            .announce_list()
            .into_iter()
            .filter_map(|tracker| {
               let announce_url = tracker.uri();
               tracker_urls
                  .insert(announce_url.clone())
                  .then_some(TrackerSnapshot {
                     announce_url,
                     status: TrackerStatus::Pending,
                     peers_returned: None,
                     last_error: None,
                  })
            })
            .collect(),
         output_path: export.output_path,
      }
   }
}

/// Frontend-friendly progress and transfer metrics for a torrent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TorrentProgressSnapshot {
   /// Number of bytes verified and available locally.
   pub verified_bytes: u64,
   /// Total bytes expected for the torrent.
   pub total_bytes: u64,
   /// Current download rate in bytes per second.
   pub download_rate_bytes_per_second: u64,
   /// Current upload rate in bytes per second.
   pub upload_rate_bytes_per_second: u64,
   /// Estimated time until completion.
   pub eta: Option<Duration>,
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

fn snapshot_info<'a>(metainfo: &'a MetaInfo, info_dict: Option<&'a Info>) -> Option<&'a Info> {
   info_dict.or(match metainfo {
      MetaInfo::Torrent(torrent_file) => Some(&torrent_file.info),
      MetaInfo::MagnetUri(_) => None,
   })
}
