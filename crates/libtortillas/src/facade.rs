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
//! use libtortillas::facade::{EngineHandle, TorrentSource};
//!
//! let engine = EngineHandle::default();
//! let source = TorrentSource::magnet("magnet:?xt=urn:btih:...");
//! # let _ = (engine, source);
//! ```

use crate::{engine::Engine, torrent::Torrent};
pub use crate::{
   engine::{EngineSnapshot, EngineStatus, TorrentSource},
   frontend::{
      CoreEvent, CoreEventKind, DEFAULT_EVENT_CAPACITY, EngineListener, EngineView, EventListener,
      EventStreamError, EventSubscription, FrontendHealth, FrontendHealthLevel, LivePublisher,
      PeerEvent, PeerEventKind, PeerHandle, PeerListener, PeerView, Sequenced, TorrentEvent,
      TorrentEventKind, TorrentListener, TorrentProgress, TorrentTransfer, TorrentView,
      TrackerEvent, TrackerEventKind, TrackerHandle, TrackerId, TrackerListener, TrackerStatus,
      TrackerView,
   },
   torrent::TorrentSnapshot,
};

/// Facade-level name for the public [`Engine`] handle.
pub type EngineHandle = Engine;

/// Facade-level name for the public [`Torrent`] handle.
pub type TorrentHandle = Torrent;
