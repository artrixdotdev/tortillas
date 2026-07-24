//! Live, frontend-facing API contracts.
//!
//! This module contains typed events, listeners, publishers, and live views
//! intended for application and UI integrations. Frontends should prefer these
//! types over actor messages, protocol internals, or snapshot polling.

mod event;
mod handle;
mod listener;
mod live;
mod publisher;
mod subscription;
mod view;

pub use event::{
   CoreEvent, CoreEventKind, FrontendHealth, FrontendHealthLevel, PeerEvent, PeerEventKind,
   Sequenced, TorrentEvent, TorrentEventKind, TrackerEvent, TrackerEventKind,
};
pub(crate) use handle::PeerScope;
pub use handle::{PeerHandle, PeerListener, TrackerHandle, TrackerId, TrackerListener};
pub use listener::{EngineListener, EventListener, TorrentListener};
pub use live::{DEFAULT_EVENT_CAPACITY, LivePublisher};
pub(crate) use publisher::{FrontendHub, FrontendPublisher};
pub use subscription::{EventStreamError, EventSubscription};
pub use view::{
   EngineView, PeerView, TorrentProgress, TorrentTransfer, TorrentView, TrackerStatus, TrackerView,
};
