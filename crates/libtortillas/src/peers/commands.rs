use tokio::sync::mpsc;

use crate::errors::PeerTransportError;

use super::{messages::PeerMessages, PeerKey};

#[derive(Debug)]
pub enum PeerCommand {
   /// Just the index of the piece that we're asking the peer for.
   Piece(u32),
}

#[derive(Debug)]
pub enum PeerResponse {
   Init(mpsc::Sender<PeerCommand>),
   Choking,
   Receive {
      message: PeerMessages,
      peer_key: PeerKey,
   },
   /// The message that is sent when a peer fails to retrieve a piece.
   PieceFailure {
      piece_num: u32,
      error: PeerTransportError,
   },
   Piece(PeerMessages),
}
