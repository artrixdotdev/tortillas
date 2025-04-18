use super::Peer;

#[derive(Debug, Clone)]
pub enum TransportCommand {
   Connect { peer: Peer },
}
