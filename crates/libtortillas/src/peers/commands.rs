use tokio::sync::mpsc;

use super::{messages::PeerMessages, PeerKey};

#[derive(Debug)]
pub enum PeerCommand {
   /// Just the index of the piece that we're asking the peer for.
   Piece(u32),
}

/// All messages FROM peers must include PeerKeys due to the fact that peers use the broadcast channel instead
/// of an mpsc or oneshot channel.
///
/// (This may shoot us in the foot later -- or more aptly, blow our entire leg off. But it
/// shouldn't.)
#[derive(Debug, Clone)]
pub enum PeerResponse {
   Init {
      from_tx: mpsc::Sender<PeerCommand>,
      peer_key: PeerKey,
   },
   Choking(PeerKey),
   Unchoke(PeerKey),
   Receive {
      message: PeerMessages,
      peer_key: PeerKey,
   },
   /// The message that is sent when a peer fails to retrieve a piece.
   PieceFailure {
      piece_num: u32,
      error_message: String,
      peer_key: PeerKey,
   },
}
