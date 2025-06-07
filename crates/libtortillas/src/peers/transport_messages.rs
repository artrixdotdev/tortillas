use std::net::SocketAddr;

use tokio::sync::oneshot;

use crate::errors::PeerTransportError;

use super::{messages::PeerMessages, Peer, PeerKey};

/// An enum for commands sent to [handle_commands()] (from [TransportHandler])
///
/// A oneshot channel is included on all enums as to ensure simple communication between threads.
#[derive(Debug)]
pub enum TransportCommand {
   Connect {
      peer: Peer,
      oneshot_tx: oneshot::Sender<Result<TransportResponse, PeerTransportError>>,
   },
   Receive {
      peer_key: PeerKey,
      oneshot_tx: oneshot::Sender<Result<TransportResponse, PeerTransportError>>,
   },
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
