//! Live, frontend-facing API contracts.
//!
//! This module contains the typed events, commands, listeners, and live views
//! intended for application and UI integrations. Frontends should prefer these
//! types over actor messages, protocol internals, or snapshot polling.

mod command;
mod event;
mod handle;
mod listener;
mod publisher;
mod subscription;
mod view;

pub use command::{CoreCommand, CoreCommandResult, TorrentCommand};
pub use event::{CoreEvent, CoreEventKind, FrontendHealth, FrontendHealthLevel, Sequenced};
pub use handle::{PeerHandle, PeerListener, TrackerHandle, TrackerListener};
pub(crate) use handle::{PeerScope, TrackerScope};
pub use listener::{EngineListener, EventListener, TorrentListener};
pub(crate) use publisher::FrontendPublisher;
pub use publisher::{DEFAULT_EVENT_CAPACITY, LivePublisher};
pub use subscription::{EventStreamError, EventSubscription};
pub use view::{EngineView, PeerView, TorrentProgress, TorrentTransfer, TorrentView, TrackerView};
