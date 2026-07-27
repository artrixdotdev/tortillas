//! One torrent's lifecycle, transfer coordination, storage, and persistence.
//!
//! # Operational ownership
//!
//! `TorrentActor` is the authoritative owner of torrent state. It coordinates
//! one peer actor per connection, one tracker actor per endpoint, piece
//! scheduling, verified progress, and storage. The public [`Torrent`] handle
//! exposes commands while the live torrent listener exposes the current
//! projection and typed events.
//!
//! High-frequency peer protocol state remains local to peer scopes. Torrent
//! transfer metrics are aggregated after periodic peer-stat collection rather
//! than republishing the complete hierarchy for every wire message. Tracker
//! progress is likewise sampled; final lifecycle announcements receive a
//! reliable current value.
//!
//! The piece scheduler fills a bounded request window for each peer, considers
//! that peer's advertised bitfield, releases requests when a peer disconnects
//! or rejects them, and makes unanswered requests eligible for reassignment
//! after [`crate::settings::TorrentSettings::peer_request_timeout`]. Piece
//! completion refills the consumed window slot, so a long download cannot
//! drain its pipeline one piece at a time.
//!
//! # Lifecycle
//!
//! Torrents with metadata begin as [`TorrentState::Added`]; magnet sources
//! without metadata begin as [`TorrentState::ResolvingMetadata`]. Once metadata
//! and the configured peer threshold are available, a torrent becomes
//! [`TorrentState::Ready`] when autostart is disabled or moves into
//! [`TorrentState::Downloading`] when transfer begins.
//!
//! Completed downloads transition to [`TorrentState::Seeding`].
//! [`TorrentState::Paused`] is an explicit user state and is not eligible for
//! autostart. Shutdown, supervision, and failure paths remain distinct through
//! `Restarting`, `Stopping`, `Stopped`, and `Failed` states.
//!
//! # Persistence boundary
//!
//! Live views and persistence snapshots are deliberately separate. Restoration
//! follows one ordered transaction:
//!
//! ```text
//! schema validation
//!   -> storage reconciliation
//!   -> actor-state installation
//!   -> optional transfer resumption
//! ```
//!
//! Snapshot schema validation occurs once in the engine actor. Internal restore
//! APIs accept a validated wrapper so torrent actors cannot repeat or bypass
//! that boundary.
//!
//! A `.torrent` source stores its `Info` dictionary only inside
//! [`crate::metainfo::MetaInfo`]. Only a resolved magnet stores separate
//! resolved metadata, and [`TorrentSnapshot::resolved_info`] is the canonical
//! resolver used by validation and restoration.
//!
//! [`RestoreVerification::Full`] is the safe default. It verifies completed
//! payload hashes, demotes missing or corrupt pieces, and clears partial-block
//! bits whose referenced bytes are absent.
//! [`RestoreVerification::TrustSnapshot`] skips payload verification and must
//! only be used when the application can independently guarantee storage
//! integrity.
//!
//! Arbitrary custom piece-manager trait objects have no implicit persistence
//! representation. Snapshotting one returns a typed unsupported error instead
//! of silently restoring it as a different storage implementation.
//!
//! Snapshot JSON is a versioned durable contract: portable numeric fields use
//! `u64`, keyed scheduler state is sorted, and bitfields serialize as
//! `Vec<bool>` rather than implementation-specific concurrent collections.
//! Migrations are explicit and supported versions have golden fixtures.
//!
//! # Choking
//!
//! Active torrents periodically collect peer transfer samples and recalculate
//! upload slots. Downloads prefer interested peers with the highest recent
//! download rate; seeds prefer recent upload rate. One slot rotates as an
//! optimistic unchoke when the interested set exceeds available slots.
//! `PeerActor` remains responsible for the corresponding wire-level `Choke`
//! and `Unchoke` messages.

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
