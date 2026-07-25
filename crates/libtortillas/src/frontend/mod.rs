//! Transport-agnostic live application API.
//!
//! The module is intentionally organized by the way a consumer reads it:
//!
//! - [`EngineView`], [`TorrentView`], [`PeerView`], and [`TrackerView`] are
//!   current presentation state.
//! - Shared measurements live in [`crate::metrics`] and are re-exported here.
//! - Event enums describe discrete changes.
//! - [`EventSubscription`] is events only; [`EventListener`] pairs events with
//!   a current view.
//! - [`PeerHandle`] and [`TrackerHandle`] provide scoped identity and access.
//! - `hub` is the single internal ownership and publication coordinator.
//!
//! Terminal interfaces, HTTP/WebSocket servers, web backends, and desktop
//! applications all consume this same API. Rendering, transport, and input
//! policy remain outside `libtortillas`.

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
