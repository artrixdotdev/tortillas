use std::net::SocketAddr;

use super::{messages::PeerMessages, Peer, PeerKey};

/// An enum for commands sent to [handle_commands()] (from [TransportHandler])
#[derive(Debug, Clone)]
pub enum TransportCommand {
   Connect { peer: Peer },
   Receive { peer_key: PeerKey },
}

/// An enum for responses sent from [handle_commands()] (from [TransportHandler])
#[derive(Debug, Clone)]
pub enum TransportResponse {
   Connect(SocketAddr),
   Receive {
      message: PeerMessages,
      peer_key: PeerKey,
   },
}
