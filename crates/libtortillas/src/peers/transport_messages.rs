use std::net::SocketAddr;

use super::{Peer, PeerKey};

/// An enum for commands sent to [handle_commands()] (from [TransportHandler])
#[derive(Debug, Clone)]
pub enum TransportCommand {
   Connect { peer: Peer },
   Receive,
}

/// An enum for responses sent from [handle_commands()] (from [TransportHandler])
#[derive(Debug, Clone)]
pub enum TransportResponse {
   Connect(SocketAddr),
   Receive((PeerKey, Vec<u8>)),
}
