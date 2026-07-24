use std::{net::SocketAddr, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{engine::EngineStatus, hashes::InfoHash, peer::Peer, torrent::TorrentState};

/// Current live engine state maintained by a frontend listener.
///
/// Unlike persistence snapshots, views are display-oriented and updated by
/// applying live [`CoreEvent`](super::CoreEvent) values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineView {
   pub status: EngineStatus,
   pub torrent_count: u64,
   pub torrents: Vec<TorrentView>,
}

/// Current live state of one torrent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TorrentView {
   pub info_hash: InfoHash,
   pub name: String,
   pub state: TorrentState,
   pub has_metadata: bool,
   pub is_ready: bool,
   pub auto_start: bool,
   pub sufficient_peers: u64,
   pub peer_count: u64,
   pub tracker_count: u64,
   pub output_path: Option<PathBuf>,
   pub progress: TorrentProgress,
   pub transfer: TorrentTransfer,
}

/// Live torrent progress intended for frontend rendering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TorrentProgress {
   pub total_bytes: Option<u64>,
   pub downloaded_bytes: u64,
   pub bytes_remaining: Option<u64>,
   pub progress_fraction: Option<f64>,
   pub completed_pieces: u64,
   pub partial_pieces: u64,
   pub total_pieces: u64,
}

/// Live torrent transfer metrics intended for frontend rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentTransfer {
   pub download_rate_bytes_per_second: Option<u64>,
   pub upload_rate_bytes_per_second: Option<u64>,
   pub eta_seconds: Option<u64>,
}

/// Live view of a connected or recently disconnected peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerView {
   /// Network address for the peer, when known.
   pub address: Option<SocketAddr>,
   /// Parsed peer-client family, when known.
   pub client: Option<String>,
   /// Whether this peer is currently connected.
   pub connected: bool,
   pub peer_choking: bool,
   pub peer_interested: bool,
   pub client_choking: bool,
   pub client_interested: bool,
   pub available_pieces: u64,
   pub download_rate_bytes_per_second: u64,
   pub upload_rate_bytes_per_second: u64,
   pub downloaded_bytes: u64,
   pub uploaded_bytes: u64,
}

impl PeerView {
   pub(crate) fn from_peer(peer: &Peer, connected: bool) -> Self {
      Self {
         address: Some(peer.socket_addr()),
         client: peer.id.map(|id| id.client_name().to_string()),
         connected,
         peer_choking: peer.am_choked(),
         peer_interested: peer.interested(),
         client_choking: peer.choked(),
         client_interested: peer.am_interested(),
         available_pieces: u64::try_from(peer.pieces.count_ones()).unwrap_or(u64::MAX),
         download_rate_bytes_per_second: u64::try_from(peer.download_rate())
            .unwrap_or(u64::MAX)
            .saturating_mul(1024),
         upload_rate_bytes_per_second: u64::try_from(peer.upload_rate())
            .unwrap_or(u64::MAX)
            .saturating_mul(1024),
         downloaded_bytes: u64::try_from(peer.bytes_downloaded()).unwrap_or(u64::MAX),
         uploaded_bytes: u64::try_from(peer.bytes_uploaded()).unwrap_or(u64::MAX),
      }
   }
}

/// Frontend-safe live tracker identity and latest announce outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerView {
   /// Credential-free tracker endpoint label.
   pub endpoint: String,
   /// Current actor and announce lifecycle.
   pub status: TrackerStatus,
   /// Number of peers returned by the latest successful announce.
   pub peers_returned: Option<u64>,
}

/// Lifecycle and latest announce outcome for a tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackerStatus {
   /// The tracker actor is running but has not completed an announce.
   Pending,
   /// The latest announce completed successfully.
   Healthy,
   /// The latest announce failed while the actor remained available.
   Degraded,
   /// The tracker actor stopped and will emit no more events.
   Stopped,
}

impl TrackerStatus {
   /// Whether the tracker actor can still produce announcements.
   #[must_use]
   pub const fn is_active(self) -> bool {
      !matches!(self, Self::Stopped)
   }
}
