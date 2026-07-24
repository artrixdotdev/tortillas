//! Live, frontend-facing API contracts.
//!
//! This module contains typed events, listeners, publishers, and live views
//! intended for application and UI integrations. Frontends should prefer these
//! types over actor messages, protocol internals, or snapshot polling.

mod event;
mod handle;
mod listener;
mod publisher;
mod subscription;
mod view;

pub use event::{
   CoreEvent, CoreEventKind, FrontendHealth, FrontendHealthLevel, PeerEvent, PeerEventKind,
   Sequenced, TorrentEvent, TorrentEventKind, TrackerEvent, TrackerEventKind,
};
pub use handle::{PeerHandle, PeerListener, TrackerHandle, TrackerListener};
pub(crate) use handle::{PeerScope, TrackerScope};
pub use listener::{EngineListener, EventListener, TorrentListener};
pub(crate) use publisher::FrontendPublisher;
pub use publisher::{DEFAULT_EVENT_CAPACITY, LivePublisher};
pub use subscription::{EventStreamError, EventSubscription};
pub use view::{EngineView, PeerView, TorrentProgress, TorrentTransfer, TorrentView, TrackerView};
