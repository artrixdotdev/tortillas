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
