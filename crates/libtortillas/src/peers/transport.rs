use anyhow::{Result, anyhow};
use std::{net::SocketAddr, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, error, info, trace};

use crate::{
   errors::PeerTransportError,
   hashes::{Hash, InfoHash},
   peers::messages::MAGIC_STRING,
};

use super::{
   Peer, PeerKey, PeerStream,
   messages::{Handshake, PeerMessages},
};

/// A struct that allows (peers)[super::Peer] to abstract the "feature" of handling/sending messages
/// to an outside struct.
///
/// More specifically, [Transport] should be treated like a "static" class. No initilization
/// should ever be required.
pub struct PeerTransport {}

impl PeerTransport {
   /// Handshakes with a peer and returns the socket address of the peer. This socket address is
   /// also a (PeerKey)[super::PeerKey].
   pub async fn connect_peer(
      peer: &mut Peer,
      our_id: Arc<Hash<20>>,
      info_hash: Arc<InfoHash>,
      mut stream: PeerStream,
   ) -> Result<PeerKey, PeerTransportError> {
      let handshake = Handshake::new(info_hash.clone(), our_id.clone());
      stream.write_all(&handshake.to_bytes()).await.unwrap();
      trace!("Sent handshake to peer");

      // Calculate expected size for response
      // 1 byte + protocol + reserved + hashes
      const EXPECTED_SIZE: usize = 1 + MAGIC_STRING.len() + 8 + 40;
      let mut buf = [0u8; EXPECTED_SIZE];

      // Read response handshake
      stream.read_exact(&mut buf).await.map_err(|e| {
         error!("Failed to read handshake from peer {}: {}", peer, e);
         PeerTransportError::ConnectionFailed(peer.socket_addr().to_string())
      })?;

      let handshake =
         Handshake::from_bytes(&buf).map_err(|e| PeerTransportError::Other(anyhow!("{e}")))?;

      let (_, new_peer) =
         Self::validate_handshake(handshake, peer.socket_addr(), info_hash, our_id).unwrap();

      // Store peer information
      let our_id = new_peer.id.unwrap();
      peer.id = Some(our_id);

      info!(%peer, "Peer connected");

      Ok(peer.socket_addr())
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
   async fn receive_handshake(
      &mut self,
      info_hash: Arc<InfoHash>,
      id: Arc<Hash<20>>,
      mut stream: PeerStream,
   ) -> Result<(PeerKey, Vec<u8>), PeerTransportError> {
      // First 4 bytes is the big endian encoded length field and the 5th byte is a PeerMessage tag
      let mut buf = vec![0; 5];

      stream.read_exact(&mut buf).await.map_err(|e| {
         error!("Error occurred when reading the peer's response: {e}");
         PeerTransportError::InvalidPeerResponse("Error occured".into())
      })?;
      let addr = stream.remote_addr().unwrap();

      trace!(message_type = buf[4], ip = %addr, "Recieved message headers, requesting rest...");
      let is_handshake = PeerTransport::is_handshake(&buf);

      let length = if is_handshake {
         // This is a handshake.
         // The length of a handshake is always 68 and we already have the
         // first 5 bytes of it, so we need 68 - 5 bytes (the current buffer length)

         68 - buf.len() as u32
      } else {
         // This is not a handshake
         // Non handshake messages have a length field from bytes 0-4
         return Err(PeerTransportError::InvalidPeerResponse(
            "Invalid Handshake".into(),
         ));
      };

      let mut rest = vec![0; length as usize];

      stream.read_exact(&mut rest).await.map_err(|e| {
         error!("Error occurred when reading the peer's response: {e}");
         PeerTransportError::InvalidPeerResponse("Error occured".into())
      })?;
      let full_length = length + buf.len() as u32;

      debug!(
         "Read {} action ({} bytes) from {} ",
         buf[4], full_length, addr
      );
      buf.extend_from_slice(&rest);

      // TODO: Update timing fields on peer
      // if let Some(mutex) = self.peers.get_mut(&addr) {
      //    let peer = &mut mutex.lock().await.0;

      //    // Completely chat gippity generated code, do not trust
      //    // Update total bytes uploaded
      //    peer.bytes_uploaded += full_length as u64;

      //    // Calculate upload rate based on a time window
      //    if let Some(last_time) = peer.last_message_received {
      //       let elapsed_secs = last_time.elapsed().as_secs();
      //       if elapsed_secs > 0 {
      //          let current_rate = full_length as u64 / elapsed_secs;
      //          peer.download_rate = current_rate;
      //       }
      //    }

      //    let now = Instant::now();
      //    peer.last_message_received = Some(now);
      // };

      let handshake =
         Handshake::from_bytes(&buf).map_err(|e| PeerTransportError::Other(anyhow!("{e}")))?;
      let (handshake, peer) = Self::validate_handshake(handshake, addr, info_hash, id)?;

      trace!(peer_id = %peer.id.unwrap(), "Successfully validated handshake");

      // TODO
      // Creates a new handshake and sends it
      // let message = PeerMessages::Handshake(handshake);

      // self.send_data(addr, message.to_bytes().unwrap()).await?;

      info!(%peer, "Peer connected");

      Ok((addr, buf))
   }
   /// Takes in a received handshake and returns the handshake we should respond with as well as the new peer. It preassigns the our_id to the peer.
   pub fn validate_handshake(
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

   /// Checks to see if a peer message is a handshake using the first 5 bytes.
   pub(super) fn is_handshake(buf: &[u8]) -> bool {
      buf[0] as usize == MAGIC_STRING.len() && buf[1..5] == MAGIC_STRING[0..4]
   }
}
