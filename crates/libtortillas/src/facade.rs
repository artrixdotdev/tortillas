//! Application-facing facade for `libtortillas`.
//!
//! Re-exports the handles, snapshots, and live types most applications need.

pub use crate::{
   engine::{Engine, EngineSnapshot, EngineStatus, TorrentSource},
   torrent::{RestoreVerification, Torrent, TorrentSnapshot},
   tracker::Tracker,
};
#[cfg(feature = "live")]
pub use crate::{
   live::{
      EngineEvent, EngineEventKind, EngineListener, EngineView, EventListener, EventStreamError,
      EventSubscription, LiveHealth, LiveHealthLevel, LivePublisher, PeerEvent, PeerEventKind,
      PeerHandle, PeerListener, PeerView, SequencedEvent, TorrentEvent, TorrentEventKind,
      TorrentListener, TorrentView, TrackerEvent, TrackerEventKind, TrackerHandle, TrackerId,
      TrackerListener, TrackerStatus, TrackerView,
   },
   metrics::{
      ByteCount, BytesPerSecond, ContentProgress, HasTransferMetrics, PeerMetrics, Seconds,
      TorrentMetrics, TrackerMetrics, TrafficTotals, TransferMetrics, TransferRates,
      TransferSample,
   },
};
