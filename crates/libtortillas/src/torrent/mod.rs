//! Torrent lifecycle, transfer coordination, storage, and persistence.
//!
//! # Lifecycle
//!
//! Torrents with metadata begin as [`TorrentState::Added`]; magnet sources
//! without metadata begin as [`TorrentState::ResolvingMetadata`]. Once metadata
//! and the configured peer threshold are available, a torrent becomes
//! [`TorrentState::Ready`] when autostart is disabled or moves into
//! [`TorrentState::Downloading`] when transfer begins.
//!
//! Completed downloads transition to [`TorrentState::Seeding`], while paused
//! torrents remain paused until explicitly resumed.
//!
//! # Persistence boundary
//!
//! Live views and persistence snapshots are separate. Restoration runs in this
//! order:
//!
//! ```text
//! schema validation
//!   -> storage reconciliation
//!   -> actor-state installation
//!   -> optional transfer resumption
//! ```
//!
//! [`RestoreVerification::Full`] is the safe default. It verifies completed
//! payload hashes, demotes missing or corrupt pieces, and clears partial-block
//! bits whose referenced bytes are absent.
//! [`RestoreVerification::TrustSnapshot`] skips payload verification and must
//! only be used when the application can independently guarantee storage
//! integrity.
//!
//! Custom piece managers cannot be snapshotted unless they have a durable
//! representation.

mod actor;
mod block;
mod choking;
mod choking_flow;
mod discovery;
mod handle;
mod messages;
mod piece_flow;
mod snapshot;
mod state;
mod storage;
mod swarm;

pub(crate) use actor::{TorrentActor, TorrentActorArgs};
pub use block::{BLOCK_SIZE, BlockMap};
pub use discovery::AnnounceFrom;
pub use handle::Torrent;
#[cfg(feature = "live")]
pub(crate) use handle::TorrentInner;
pub(crate) use messages::*;
pub use snapshot::{
   PieceBlockSnapshot, RestoreVerification, TORRENT_SNAPSHOT_VERSION, TorrentSnapshot,
};
pub(crate) use snapshot::{ValidatedTorrentSnapshot, ValidatedTorrentState};
pub use state::TorrentState;
pub use storage::PieceStorageStrategy;

pub mod util;
