use bytes::Bytes;

mod piece_manager;
mod piece_scheduler;
mod piece_store;

pub use piece_manager::*;
pub(crate) use piece_scheduler::*;
pub(crate) use piece_store::*;

/// A piece that we have received from a peer.
///
/// [BEP 0003](https://www.bittorrent.org/beps/bep_0003.html) describes the
/// piece message. The `name` field lets application code differentiate between
/// pieces in separate files or folders.
#[derive(Debug, Clone)]
pub struct StreamedPiece {
   /// The name of the file the piece belongs to.
   pub name: String,

   /// The index of the piece.
   pub index: usize,

   /// The offset of the piece.
   pub offset: usize,

   /// The raw bytes of the piece.
   pub data: Bytes,
}
