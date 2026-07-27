//! Application-facing facade for `libtortillas`.
//!
//! This module defines the stable surface applications should prefer over
//! actor, protocol, tracker, and storage internals. Lower-level
//! modules remain public for advanced integrations, but a terminal UI, web
//! server, browser backend, desktop app, or other consumer can model user
//! intent, observe progress, and hold handles through the same types.
//!
//! # Example
//!
//! ```no_run
//! use libtortillas::facade::{Engine, TorrentSource};
//!
//! let engine = Engine::default();
//! let source = TorrentSource::magnet("magnet:?xt=urn:btih:...");
//! # let _ = (engine, source);
//! ```

pub use crate::{
   engine::{Engine, EngineSnapshot, EngineStatus, TorrentSource},
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
   torrent::{RestoreVerification, Torrent, TorrentSnapshot},
};
