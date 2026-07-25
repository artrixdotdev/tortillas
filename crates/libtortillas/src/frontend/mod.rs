//! Transport-agnostic live application API.
//!
//! Terminal interfaces, HTTP or WebSocket servers, websites, and desktop
//! applications all consume this same API. Rendering, transport, input, and
//! application routing policy remain outside `libtortillas`.
//!
//! # Public model
//!
//! The module is organized by the way an application reads it:
//!
//! - [`EngineView`], [`TorrentView`], [`PeerView`], and [`TrackerView`] are
//!   current presentation state.
//! - Shared measurements live in [`crate::metrics`] and are re-exported here.
//! - Event enums describe discrete changes.
//! - [`EventSubscription`] is events only; [`EventListener`] pairs events with
//!   a coherent current view.
//! - [`PeerHandle`] and [`TrackerHandle`] provide scoped identity and access.
//! - The private hub owns the complete live projection tree and coordinates
//!   publication.
//!
//! [`crate::engine::Engine`] and [`crate::torrent::Torrent`] remain the sole
//! public command API. There is no parallel command enum or generic `send`
//! method for application operations.
//!
//! # Listening to an engine
//!
//! Create a listener before starting operations when the application must not
//! miss their events. Use [`EventListener::view`] for initial rendering and
//! lag recovery, and [`EventListener::recv`] for future changes.
//!
//! ```no_run
//! use libtortillas::prelude::{Engine, EngineEventKind, EventStreamError};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let engine = Engine::default();
//! let mut listener = engine.listener();
//! let initial_view = listener.view();
//!
//! loop {
//!    match listener.recv().await {
//!       Ok(event) => {
//!          let current_view = listener.view();
//!          // Render, serialize, or forward `current_view` and `event`.
//!          let _ = current_view;
//!          if matches!(event.kind, EngineEventKind::Shutdown(_)) {
//!             break;
//!          }
//!       }
//!       Err(EventStreamError::Lagged(_)) => {
//!          // Discard adapter-local assumptions and redraw from current state.
//!          let current_view = listener.view();
//!          let _ = current_view;
//!       }
//!       Err(EventStreamError::Closed) => break,
//!    }
//! }
//! # let _ = initial_view;
//! # Ok(())
//! # }
//! ```
//!
//! Every [`crate::torrent::Torrent`] has its own `listener()` and
//! `subscribe()` methods. Peers and trackers returned by `Torrent::peers()` and
//! `Torrent::trackers()` follow the same pattern. A scoped listener receives
//! only that scope's events; it does not filter the engine stream.
//!
//! Engine listeners receive [`EngineEventKind::Torrent`], whose nested event
//! uses the same [`TorrentEventKind`] vocabulary as the torrent listener.
//! Peer and tracker lifecycle events carry public handles, allowing an
//! application to descend into detailed streams only when needed.
//!
//! Use `subscribe()` when only discrete events are needed. Use `listener()`
//! when initial rendering or recovery requires a current view as well.
//!
//! # Ownership and source of truth
//!
//! ```text
//! EngineActor ── owns operational engine state
//! FrontendHub
//! ├── engine lifecycle and event publisher
//! └── keyed torrent scopes
//!     └── torrent view and event publisher
//!         ├── keyed peer scopes
//!         └── keyed tracker scopes
//!
//! EngineView = engine lifecycle + views derived from current torrent scopes
//! ```
//!
//! The engine never caches a second `Vec<TorrentView>`. [`EngineListener`]
//! derives [`EngineView`] on read from current torrent scopes and sorts them by
//! info hash. Peer-only changes therefore touch one peer scope and cannot make
//! a copied engine projection drift from the torrent projection.
//!
//! The private scope registry is a policy wrapper around `DashMap`, not a
//! replacement concurrent map. It prevents shard guards from escaping by
//! returning cloned `Arc` values or owned vectors. Peer and tracker registries
//! are nested under their torrent, making lookup and removal proportional to
//! that torrent's children.
//!
//! # Architectural invariants
//!
//! These rules define the live API's source of truth:
//!
//! 1. Actors own operational domain state.
//! 2. A live scope owns only its frontend projection.
//! 3. Parent views are derived from child scopes; they do not maintain manually
//!    synchronized child-view copies.
//! 4. Every scope has one view-and-event publication entry point.
//! 5. Peer and tracker events do not implicitly rebuild torrent or engine
//!    state.
//! 6. A scope closes exactly once, only when it cannot restart.
//! 7. Snapshot schema validation runs once at the authoritative engine restore
//!    boundary.
//! 8. Actor and hub back-references are weak; the ownership graph contains no
//!    strong cycle.
//! 9. Synchronous lock order is registry shard, scope publication/state, then
//!    event sender.
//! 10. Actor communication, filesystem work, arbitrary callbacks, and `.await`
//!     never occur while a synchronous lock is held.
//!
//! # Event delivery and lifecycle
//!
//! Channels are allocated lazily on first subscription. Defaults retain 256
//! engine or torrent events and 64 peer or tracker events; all capacities are
//! configurable with [`crate::settings::FrontendSettings`]. A slow consumer
//! receives [`EventStreamError::Lagged`] instead of causing unbounded memory
//! growth. Sequence numbers increase monotonically within each scope.
//!
//! [`LivePublisher`] mutation names describe their full effect:
//! [`LivePublisher::replace_view`] changes only the projection,
//! [`LivePublisher::replace_view_and_emit`] performs a coherent view/event
//! transition, [`LivePublisher::emit_without_view_change`] emits a discrete
//! event, and [`LivePublisher::close_with_terminal_event`] performs the one
//! irreversible close transition.
//!
//! Supervised torrent and tracker actors publish a restarting state after
//! abnormal termination and keep their streams open. Final ownership teardown
//! publishes the terminal state once, closes the scope tree, and rejects late
//! actor updates.
//!
//! # Locking and publication
//!
//! Registry methods release their `DashMap` shard guard before acquiring a
//! scope lock. Scope construction occurs before shard entry acquisition, so
//! callbacks never execute under a registry lock. A scope publication lock
//! serializes its view transition, scoped event, and corresponding root event.
//! [`LivePublisher`] then acquires its state lock before its optional sender
//! lock. No path acquires a registry guard while holding a child scope lock,
//! and no synchronous lock crosses an `.await`.
//!
//! # Views and persistence
//!
//! Views are presentation contracts suitable for rendering, API responses,
//! and transport serialization. [`crate::engine::EngineSnapshot`] and
//! [`crate::torrent::TorrentSnapshot`] are durable persistence contracts.
//! Applications must not poll snapshots to refresh a frontend. See
//! [`crate::torrent`] for restore validation and storage reconciliation rules.
//!
//! Application-specific action routing can use an adapter-owned Tokio channel
//! whose consumer invokes methods on `Engine` and `Torrent`. That keeps UI or
//! server commands outside the library without duplicating its public API.

mod event;
mod handle;
mod hub;
mod stream;
#[cfg(test)]
mod tests;
mod view;

pub use event::{
   EngineEvent, EngineEventKind, FrontendHealth, FrontendHealthLevel, PeerEvent, PeerEventKind,
   SequencedEvent, TorrentEvent, TorrentEventKind, TrackerEvent, TrackerEventKind,
};
pub(crate) use handle::PeerIdentity;
pub use handle::{PeerHandle, PeerListener, TrackerHandle, TrackerId, TrackerListener};
pub(crate) use hub::{FrontendHub, FrontendHubInner};
pub use stream::{
   EngineListener, EventListener, EventStreamError, EventSubscription, LivePublisher,
   TorrentListener,
};
pub use view::{EngineView, PeerView, TorrentView, TrackerStatus, TrackerView};

pub use crate::metrics::{
   ByteCount, BytesPerSecond, ContentProgress, HasTransferMetrics, Seconds, TorrentMetrics,
   TrafficTotals, TransferMetrics, TransferRates,
};
