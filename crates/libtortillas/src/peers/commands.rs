use tokio::sync::mpsc;

use super::{messages::PeerMessages, PeerKey};

pub enum PeerCommand {}

pub enum PeerResponse {
   Init(mpsc::Sender<PeerCommand>),
   Receive {
      message: PeerMessages,
      peer_key: PeerKey,
   },
}
