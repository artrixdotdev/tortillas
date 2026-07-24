//! Live, frontend-facing API contracts.
//!
//! This module contains the typed events, commands, subscriptions, and
//! snapshots intended for application and UI integrations. Frontends should
//! prefer these types over actor messages and protocol internals.

mod command;
mod event;
mod listener;
mod publisher;
mod subscription;
mod view;

pub use command::{CoreCommand, CoreCommandResult, TorrentCommand};
pub use event::{CoreEvent, CoreEventKind, FrontendHealth, FrontendHealthLevel};
pub use listener::{EngineListener, TorrentListener};
pub use publisher::DEFAULT_EVENT_CAPACITY;
pub(crate) use publisher::FrontendPublisher;
pub use subscription::{EventStreamError, EventSubscription};
pub use view::{EngineView, PeerView, TorrentProgress, TorrentTransfer, TorrentView, TrackerView};
