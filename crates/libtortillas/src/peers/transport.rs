use anyhow::Result;
use std::{net::SocketAddr, str::FromStr, sync::Arc};
use tracing::{error, trace};

use crate::{
   errors::PeerTransportError,
   hashes::{Hash, InfoHash},
   peers::messages::MAGIC_STRING,
};

use super::{
   messages::{Handshake, PeerMessages},
   Peer, PeerKey, PeerStream,
};

/// A struct that allows (peers)[super::Peer] to abstract the "feature" of handling/sending messages
/// to an outside struct.
///
/// More specifically, [Transport] should be treated like a "static" class. No initilization
/// should ever be required.
pub struct Transport {}

impl Transport {
   /// Handshakes with a peer and returns the socket address of the peer. This socket address is
   /// also a (PeerKey)[super::PeerKey].
   async fn connect_peer(
      &mut self,
      peer: &mut Peer,
      peer_id: Arc<Hash<20>>,
      info_hash: Arc<InfoHash>,
      stream: PeerStream,
   ) -> Result<PeerKey, PeerTransportError> {
      Ok(PeerKey::from_str("").unwrap())
   }

   async fn send_data(
      &mut self,
      to: PeerKey,
      data: Vec<u8>,
      stream: PeerStream,
   ) -> Result<(), PeerTransportError> {
      Ok(())
   }

   /// Receives data from a peers stream. In other words, if you wish to directly contact a peer,
   /// use this function.
   async fn receive_from_peer(
      &mut self,
      peer: PeerKey,
      stream: PeerStream,
   ) -> Result<PeerMessages, PeerTransportError> {
      Ok(PeerMessages::from_bytes(vec![]).unwrap())
   }

   /// Receives data from any incoming peer. Generally used for accepting a handshake.
   async fn receive_data(
      &mut self,
      info_hash: Arc<InfoHash>,
      id: Arc<Hash<20>>,
      stream: PeerStream,
   ) -> Result<(PeerKey, Vec<u8>), PeerTransportError> {
      Ok((PeerKey::from_str("").unwrap(), vec![]))
   }

   /// Takes in a received handshake and returns the handshake we should respond with as well as the new peer. It preassigns the peer_id to the peer.
   fn validate_handshake(
      &mut self,
      received_handshake: Handshake,
      peer_addr: SocketAddr,
      info_hash: Arc<InfoHash>,
      id: Arc<Hash<20>>,
   ) -> Result<(Handshake, Peer), PeerTransportError> {
      let peer_id = received_handshake.peer_id;

      // Validate protocol string
      if MAGIC_STRING != received_handshake.protocol {
         error!("Invalid magic string received from peer {}", peer_addr);
         return Err(PeerTransportError::InvalidMagicString {
            received: String::from_utf8_lossy(&received_handshake.protocol).into(),
            expected: String::from_utf8_lossy(MAGIC_STRING).into(),
         });
      }

      // Validate info hash
      if info_hash.clone() != received_handshake.info_hash {
         error!("Invalid info hash received from peer {}", peer_addr);
         return Err(PeerTransportError::InvalidInfoHash {
            received: received_handshake.info_hash.to_hex(),
            expected: info_hash.clone().to_hex(),
         });
      }

      trace!("Received valid handshake from {}", peer_addr);

      let mut peer = Peer::from_socket_addr(peer_addr);
      peer.id = Some(*peer_id);

      let handshake = Handshake::new(info_hash.clone(), id.clone());
      Ok((handshake, peer))
   }
}
