//! Live, frontend-facing API contracts.
//!
//! This module contains typed events, listeners, publishers, and live views
//! intended for application and UI integrations. Frontends should prefer these
//! types over actor messages, protocol internals, or snapshot polling.

mod event;
mod handle;
mod hub;
mod listener;
mod live;
mod metrics;
mod publisher;
mod registry;
mod subscription;
mod view;

pub use event::{
   CoreEvent, CoreEventKind, FrontendHealth, FrontendHealthLevel, PeerEvent, PeerEventKind,
   Sequenced, TorrentEvent, TorrentEventKind, TrackerEvent, TrackerEventKind,
};
pub(crate) use handle::PeerScope;
pub use handle::{PeerHandle, PeerListener, TrackerHandle, TrackerId, TrackerListener};
pub(crate) use hub::{FrontendHub, TorrentScope};
pub use listener::{EngineListener, EventListener, TorrentListener};
pub use live::{DEFAULT_EVENT_CAPACITY, LivePublisher};
pub(crate) use metrics::TransferSample;
pub use metrics::{
   ByteCount, BytesPerSecond, ContentProgress, HasTransferMetrics, Seconds, TorrentMetrics,
   TrafficTotals, TransferMetrics, TransferRates,
};
pub(crate) use publisher::FrontendPublisher;
pub use subscription::{EventStreamError, EventSubscription};
pub use view::{EngineView, PeerView, TorrentView, TrackerStatus, TrackerView};
