use crate::errors::PeerTransportError;

use super::{Peer, PeerKey};

#[derive(Debug, Clone)]
pub enum TransportCommand {
   Connect { peer: Peer },
}

#[derive(Debug)]
pub enum TransportOutput {
   Handshake(Result<PeerKey, PeerTransportError>),
}
