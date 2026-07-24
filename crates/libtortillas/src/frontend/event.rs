use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::{
   engine::EngineSnapshot,
   hashes::InfoHash,
   torrent::{TorrentProgressSnapshot, TorrentSnapshot, TorrentState},
};

/// A sequenced event emitted by the live frontend API.
///
/// Sequence numbers are engine-local and strictly increase for every event.
/// A frontend can use them to preserve event order or detect a gap after
/// reconnecting a consumer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreEvent {
   /// Engine-local sequence number for this event.
   pub sequence: u64,
   /// The typed change represented by this event.
   pub kind: CoreEventKind,
}

impl CoreEvent {
   /// Returns the torrent associated with this event, when applicable.
   #[must_use]
   pub const fn torrent(&self) -> Option<InfoHash> {
      self.kind.torrent()
   }
}

/// Typed changes a frontend can react to without actor internals or polling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CoreEventKind {
   /// The engine finished starting and is ready for commands.
   EngineStarted(EngineSnapshot),
   /// A torrent was added to the engine.
   TorrentAdded(TorrentSnapshot),
   /// A torrent was removed from the engine.
   TorrentRemoved { torrent: InfoHash },
   /// A torrent changed lifecycle state.
   TorrentStateChanged {
      torrent: InfoHash,
      previous: TorrentState,
      current: TorrentState,
   },
   /// Metadata for a magnet torrent was resolved.
   MetadataResolved(TorrentSnapshot),
   /// Download progress changed.
   ProgressChanged {
      torrent: InfoHash,
      progress: TorrentProgressSnapshot,
   },
   /// A peer connection became available to a torrent.
   PeerConnected {
      torrent: InfoHash,
      peer: PeerSnapshot,
   },
   /// A peer connection was removed from a torrent.
   PeerDisconnected {
      torrent: InfoHash,
      peer: PeerSnapshot,
   },
   /// A tracker announce completed successfully.
   TrackerAnnounceSucceeded {
      torrent: InfoHash,
      tracker: TrackerSnapshot,
   },
   /// A tracker announce failed.
   TrackerAnnounceFailed {
      torrent: InfoHash,
      tracker: TrackerSnapshot,
   },
   /// A frontend-relevant health report was emitted.
   Health(FrontendHealth),
   /// The engine and its managed torrents stopped.
   Shutdown(EngineSnapshot),
}

impl CoreEventKind {
   /// Returns the torrent associated with this event, when applicable.
   #[must_use]
   pub const fn torrent(&self) -> Option<InfoHash> {
      match self {
         Self::EngineStarted(_) | Self::Shutdown(_) => None,
         Self::TorrentAdded(snapshot) | Self::MetadataResolved(snapshot) => {
            Some(snapshot.info_hash)
         }
         Self::TorrentRemoved { torrent }
         | Self::TorrentStateChanged { torrent, .. }
         | Self::ProgressChanged { torrent, .. }
         | Self::PeerConnected { torrent, .. }
         | Self::PeerDisconnected { torrent, .. }
         | Self::TrackerAnnounceSucceeded { torrent, .. }
         | Self::TrackerAnnounceFailed { torrent, .. } => Some(*torrent),
         Self::Health(health) => health.torrent,
      }
   }
}

/// Frontend snapshot of a connected or recently disconnected peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerSnapshot {
   /// Network address for the peer, when known.
   pub address: Option<SocketAddr>,
   /// Parsed peer-client family, when known.
   pub client: Option<String>,
   /// Whether this peer is currently connected.
   pub connected: bool,
}

/// Frontend-safe tracker identity and latest announce outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerSnapshot {
   /// Credential-free tracker endpoint label.
   pub endpoint: String,
   /// Whether the latest announce succeeded.
   pub healthy: bool,
   /// Number of peers returned by the latest successful announce.
   pub peers_returned: Option<u64>,
}

/// A recoverable or terminal health report intended for user interfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendHealth {
   /// Torrent associated with the report, or `None` for engine-wide health.
   pub torrent: Option<InfoHash>,
   /// Severity suitable for presentation and filtering.
   pub level: FrontendHealthLevel,
   /// Frontend-safe description without internal actor details.
   pub message: String,
}

/// Severity of a frontend health report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrontendHealthLevel {
   /// The operation recovered but may merit user attention.
   Warning,
   /// The engine or torrent could not recover the operation.
   Error,
}
