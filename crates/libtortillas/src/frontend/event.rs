use serde::{Deserialize, Serialize};

use super::{EngineView, PeerHandle, TorrentProgress, TrackerHandle};
use crate::{
   hashes::InfoHash,
   torrent::{Torrent, TorrentState},
};

/// A sequenced event emitted by a live publisher.
///
/// Sequence numbers are local to one publisher and strictly increase for every
/// event it emits. A frontend can use them to preserve scoped event order or
/// detect a gap after reconnecting a consumer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sequenced<E> {
   /// Publisher-local sequence number for this event.
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
   /// A change emitted by one managed torrent.
   ///
   /// The same canonical event is delivered to both the torrent listener and
   /// the engine listener, avoiding parallel event vocabularies that can drift
   /// apart as protocols are added.
   Torrent {
      torrent: Torrent,
      event: TorrentEventKind,
   },
   /// An engine-wide frontend health report was emitted.
   Health(FrontendHealth),
   /// The engine and its managed torrents stopped.
   Shutdown(EngineView),
}

/// Events emitted by one torrent's independent live publisher.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TorrentEventKind {
   Added,
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
         Self::Torrent { torrent, .. } => Some(torrent.info_hash()),
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
