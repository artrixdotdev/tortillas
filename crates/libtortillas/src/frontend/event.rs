use serde::{Deserialize, Serialize};

use super::{EngineView, PeerHandle, TorrentProgress, TrackerHandle};
use crate::{
   hashes::InfoHash,
   torrent::{Torrent, TorrentState},
};

/// A sequenced event emitted by a live publisher.
///
/// Sequence numbers are engine-local and strictly increase for every event.
/// A frontend can use them to preserve event order or detect a gap after
/// reconnecting a consumer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sequenced<E> {
   /// Engine-local sequence number for this event.
   pub sequence: u64,
   /// The typed change represented by this event.
   pub kind: E,
}

/// A sequenced event emitted by the engine's frontend publisher.
pub type CoreEvent = Sequenced<CoreEventKind>;
/// A sequenced event emitted by a torrent's live publisher.
pub type TorrentEvent = Sequenced<TorrentEventKind>;
/// A sequenced event emitted by a peer's live publisher.
pub type PeerEvent = Sequenced<PeerEventKind>;
/// A sequenced event emitted by a tracker's live publisher.
pub type TrackerEvent = Sequenced<TrackerEventKind>;

impl Sequenced<CoreEventKind> {
   /// Returns the torrent associated with this event, when applicable.
   #[must_use]
   pub fn torrent(&self) -> Option<InfoHash> {
      self.kind.torrent()
   }
}

/// Typed changes a frontend can react to without actor internals or polling.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CoreEventKind {
   /// The engine finished starting and is ready for operations.
   EngineStarted(EngineView),
   /// A torrent was added to the engine.
   TorrentAdded(Torrent),
   /// A torrent was removed from the engine.
   TorrentRemoved(Torrent),
   /// A torrent changed lifecycle state.
   TorrentStateChanged {
      torrent: InfoHash,
      previous: TorrentState,
      current: TorrentState,
   },
   /// Display-oriented torrent configuration or counts changed.
   TorrentUpdated(Torrent),
   /// Metadata for a magnet torrent was resolved.
   MetadataResolved(Torrent),
   /// Download progress changed.
   ProgressChanged {
      torrent: InfoHash,
      progress: TorrentProgress,
   },
   /// A peer connection became available to a torrent.
   PeerConnected { torrent: InfoHash, peer: PeerHandle },
   /// A connected peer's protocol state or transfer metrics changed.
   PeerUpdated { torrent: InfoHash, peer: PeerHandle },
   /// A peer connection was removed from a torrent.
   PeerDisconnected { torrent: InfoHash, peer: PeerHandle },
   /// A tracker announce completed successfully.
   TrackerAnnounceSucceeded {
      torrent: InfoHash,
      tracker: TrackerHandle,
   },
   /// A tracker announce failed.
   TrackerAnnounceFailed {
      torrent: InfoHash,
      tracker: TrackerHandle,
   },
   /// A tracker actor stopped.
   TrackerStopped {
      torrent: InfoHash,
      tracker: TrackerHandle,
   },
   /// A frontend-relevant health report was emitted.
   Health(FrontendHealth),
   /// The engine and its managed torrents stopped.
   Shutdown(EngineView),
}

/// Events emitted by one torrent's independent live publisher.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TorrentEventKind {
   Updated,
   StateChanged {
      previous: TorrentState,
      current: TorrentState,
   },
   MetadataResolved,
   ProgressChanged(TorrentProgress),
   PeerConnected(PeerHandle),
   PeerUpdated(PeerHandle),
   PeerDisconnected(PeerHandle),
   TrackerAnnounceSucceeded(TrackerHandle),
   TrackerAnnounceFailed(TrackerHandle),
   TrackerStopped(TrackerHandle),
   Health(FrontendHealth),
   Removed,
}

/// Events emitted by one peer's independent live publisher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PeerEventKind {
   Updated,
   Disconnected,
}

/// Events emitted by one tracker's independent live publisher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrackerEventKind {
   AnnounceSucceeded { peers_returned: u64 },
   AnnounceFailed,
   Stopped,
}

impl CoreEventKind {
   /// Returns the torrent associated with this event, when applicable.
   #[must_use]
   pub fn torrent(&self) -> Option<InfoHash> {
      match self {
         Self::EngineStarted(_) | Self::Shutdown(_) => None,
         Self::TorrentAdded(torrent)
         | Self::TorrentUpdated(torrent)
         | Self::TorrentRemoved(torrent)
         | Self::MetadataResolved(torrent) => Some(torrent.info_hash()),
         Self::TorrentStateChanged { torrent, .. }
         | Self::ProgressChanged { torrent, .. }
         | Self::PeerConnected { torrent, .. }
         | Self::PeerUpdated { torrent, .. }
         | Self::PeerDisconnected { torrent, .. }
         | Self::TrackerAnnounceSucceeded { torrent, .. }
         | Self::TrackerAnnounceFailed { torrent, .. }
         | Self::TrackerStopped { torrent, .. } => Some(*torrent),
         Self::Health(health) => health.torrent,
      }
   }
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
