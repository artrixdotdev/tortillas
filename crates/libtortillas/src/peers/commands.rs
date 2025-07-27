use tokio::sync::mpsc;

use super::{
   messages::{ExtendedMessage, PeerMessages},
   PeerKey,
};

#[derive(Debug)]
pub enum PeerCommand {
   /// Just the index of the piece that we're asking the peer for.
   Piece(u32),
   Extended(u32, Option<ExtendedMessage>),
}

/// All messages FROM peers must include PeerKeys due to the fact that peers use the broadcast channel instead
/// of an mpsc or oneshot channel.
///
/// (This may shoot us in the foot later -- or more aptly, blow our entire leg off. But it
/// shouldn't.)
#[derive(Debug, Clone)]
pub enum PeerResponse {
   /// An initialization message to be sent when [handle_peer](super::Peer::handle_peer) is called.
   /// We do NOT establish a connection with the peer before sending this message.
   Init {
      from_engine_tx: mpsc::Sender<PeerCommand>,
      peer_key: PeerKey,
   },
   /// A message for the metadata (info dict) retrieved from an Extended message (see (BEP 0010)[https://www.bittorrent.org/beps/bep_0010.html] and (BEP 0009)[https://www.bittorrent.org/beps/bep_0009.html])
   /// These bytes MUST be checked before sending this message, like so:
   /// ```
   ///   let info_dict: Info = serde_bencode::from_bytes(&info_bytes).unwrap();
   ///
   ///   if info_dict.hash().unwrap() != info_hash {
   ///      error!("Info dict's hash was not equal to ours!");
   ///      return;
   ///   }
   ///
   ///   // Send PeerResponse::Info(info_bytes) here
   /// ```
   Info {
      bytes: Vec<u8>,
      from_engine_tx: mpsc::Sender<PeerCommand>,
      peer_key: PeerKey,
   },
   /// A message to notify the engine that a given peer is choking.
   Choking {
      from_engine_tx: mpsc::Sender<PeerCommand>,
      peer_key: PeerKey,
   },
   /// A message to notify the engine that a given peer is unchoked.
   Unchoke {
      from_engine_tx: mpsc::Sender<PeerCommand>,
      peer_key: PeerKey,
   },
   /// A generic receive message with a slot for a PeerMessage. This does force the engine to
   /// handle and manage exactly what PeerMessage variant we're receiving, but IMO, this is a
   /// better way of handling messages.
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
