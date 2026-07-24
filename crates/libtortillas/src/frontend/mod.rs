//! Live, frontend-facing API contracts.
//!
//! This module contains the typed events, commands, subscriptions, and
//! snapshots intended for application and UI integrations. Frontends should
//! prefer these types over actor messages and protocol internals.

mod event;

pub use event::{
   CoreEvent, CoreEventKind, FrontendHealth, FrontendHealthLevel, PeerSnapshot, TrackerSnapshot,
};
