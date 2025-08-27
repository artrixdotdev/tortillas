pub mod engine;
pub mod errors;
pub mod hashes;
pub mod metainfo;
pub mod peer;
pub mod protocol;
pub mod torrent;
pub mod tracker;
pub(crate) mod util;

/// The prelude for this crate.
///
/// This module re-exports the most commonly used types, traits, and functions
/// so that you can conveniently import them all at once:
///
/// ```
/// use libtortillas::prelude::*;
/// ```
pub mod prelude {
   pub use crate::{
      engine::*,
      errors::*,
      hashes::InfoHash,
      metainfo::*,
      peer::{Peer, PeerId},
      torrent::*,
      tracker::Tracker,
   };
}
