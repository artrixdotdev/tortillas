use anyhow::{Result, anyhow};
use bitvec::prelude::*;

use crate::{errors::PeerTransportError, hashes::Hash};
use std::sync::Arc;

pub const MAGIC_STRING: &[u8] = b"BitTorrent protocol";

#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
/// Represents messages exchanged between peers in the BitTorrent protocol.
///
/// See the "peer messages" section of the BitTorrent specification:
/// <https://www.bittorrent.org/beps/bep_0003.html#peer-messages>
pub enum PeerMessages {
   /// Refers to peer choke status. Choking occurs when a peer is "throttled" in order to maintain a constant download rate, among other reasons.
   Choke = 0u8,
   /// Refers to the inverse of the choke status. See `Self::Choke`
   Unchoke = 1u8,
   /// Refers to the interest state of the peer. Data transfer occurs when one side is interested and the other side is not choking (<https://www.bittorrent.org/beps/bep_0003.html>)
   Interested = 2u8,
   /// Indicates that the peer is not interested in receiving data.
   ///
   /// This is the opposite of [Interested](Self::Interested).
   NotInterested = 3u8,
   /// Indicates that a peer has successfully downloaded and verified a piece.
   ///
   /// Includes the index of the completed piece. The index refers to the
   /// piece within the torrent file.
   Have(u32) = 4u8,

   /// A bitfield representing the pieces that the peer has.
   ///
   /// Each boolean in the vector corresponds to a piece in the torrent. A
   /// `true` value indicates that the peer has the piece, while `false`
   /// indicates that it does not. This is typically sent after the handshake.
   Bitfield(BitVec<u8>) = 5u8,
   /// A request for a specific block of data from a piece.
   /// Length is usually a power of 2.
   ///
   ///
   /// # Binary Layout
   ///
   /// |   0-4   |   4-8   |   8-12   |
   /// |---------|---------|----------|
   /// |  index  |  begin  |  length  |
   Request(u32, u32, u32) = 6u8,

   /// A block of data corresponding to a previously sent `Request` message.
   ///
   /// # Binary Layout
   ///
   /// |   0-4   |   4-8   |   8-N   |
   /// |---------|---------|---------|
   /// |  index  |  begin  |  piece  |
   ///
   /// Unexpected pieces might arrive; refer to the [specification](https://www.bittorrent.org/beps/bep_0003.html) for details.
   Piece(u32, u32, Vec<u8>) = 7u8,
   /// Cancels a pending `Request` message.
   ///
   /// Sent to indicate that a requested block is no longer needed. Typically
   /// used towards the end of a download to optimize the acquisition of the
   /// last few pieces.
   ///
   /// # Binary Layout
   ///
   /// |  0-4  |   4-8   |  8-12  |
   /// |-------|---------|--------|
   /// | index |  begin  | length |
   Cancel(u32, u32, u32) = 8u8,

   /// This message is special, as it is not technically part of the standard [BitTorrent peer messages](https://www.bittorrent.org/beps/bep_0003.html#peer-messages),
   /// And does not have a specified Message ID, unlike the other messages that have a defined ID.
   /// This message should only be sent on the initial connection to a new peer.
   ///
   /// Refer to this paper for more information about the handshake:
   /// <https://netfuture.ch/wp-content/uploads/2015/02/zink2012efficient.pdf>
   ///
   /// # Binary layout
   ///
   /// |       0-1       |      1-20     |   20-28  |    28-48  |  48-68  |
   /// |-----------------|---------------|----------|-----------|---------|
   /// | Protocol Length | Protocol Name | Reserved | Info Hash | Peer ID |
   Handshake(Handshake),

   /// Just a ping message, used to keep the connection alive.
   KeepAlive,
}

impl PeerMessages {
   pub fn to_bytes(&self) -> Result<Vec<u8>, PeerTransportError> {
      Ok(match self {
         PeerMessages::Handshake(handshake) => handshake.to_bytes(),
         PeerMessages::Choke => Self::create_message_with_id(0, &[]),
         PeerMessages::Unchoke => Self::create_message_with_id(1, &[]),
         PeerMessages::Interested => Self::create_message_with_id(2, &[]),
         PeerMessages::NotInterested => Self::create_message_with_id(3, &[]),
         PeerMessages::Have(index) => Self::create_message_with_id(4, &index.to_be_bytes()),
         PeerMessages::Bitfield(bits) => Self::create_message_with_id(5, bits.as_raw_slice()),
         PeerMessages::Request(index, begin, length) => {
            let mut payload = Vec::with_capacity(12);
            payload.extend_from_slice(&index.to_be_bytes());
            payload.extend_from_slice(&begin.to_be_bytes());
            payload.extend_from_slice(&length.to_be_bytes());
            Self::create_message_with_id(6, &payload)
         }
         PeerMessages::Piece(index, begin, data) => {
            let mut payload = Vec::with_capacity(8 + data.len());
            payload.extend_from_slice(&index.to_be_bytes());
            payload.extend_from_slice(&begin.to_be_bytes());
            payload.extend_from_slice(data);
            Self::create_message_with_id(7, &payload)
         }
         PeerMessages::Cancel(index, begin, length) => {
            let mut payload = Vec::with_capacity(12);
            payload.extend_from_slice(&index.to_be_bytes());
            payload.extend_from_slice(&begin.to_be_bytes());
            payload.extend_from_slice(&length.to_be_bytes());
            Self::create_message_with_id(8, &payload)
         }
         _ => return Err(PeerTransportError::Other(anyhow!("Unknown message type"))),
      })
   }

   // Helper method to create a message with length prefix, ID, and payload
   fn create_message_with_id(id: u8, payload: &[u8]) -> Vec<u8> {
      let length = 1 + payload.len() as u32; // ID (1 byte) + payload length
      let mut message = Vec::with_capacity(4 + length as usize);

      // Length prefix (4 bytes)
      message.extend_from_slice(&length.to_be_bytes());
      // Message ID (1 byte)
      message.push(id);
      // Payload
      message.extend_from_slice(payload);

      message
   }

   pub fn from_bytes(bytes: Vec<u8>) -> Result<PeerMessages, PeerTransportError> {
      // Check if it's a handshake (handshakes don't have length prefix)
      if bytes.len() >= 68 && bytes[0] == 19 && &bytes[1..20] == b"BitTorrent protocol" {
         return Ok(PeerMessages::Handshake(
            Handshake::from_bytes(&bytes).map_err(|e| PeerTransportError::Other(anyhow!("{e}")))?,
         ));
      }

      // For regular messages, we need at least 4 bytes for the length prefix
      if bytes.len() < 4 {
         return Err(PeerTransportError::MessageTooShort);
      }

      let length = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;

      // Check if we have enough bytes for the full message
      if bytes.len() < 4 + length {
         return Err(PeerTransportError::MessageTooShort);
      }

      // Empty message (keep-alive)
      if length == 0 {
         return Ok(PeerMessages::KeepAlive);
      }

      // Regular message with ID
      if length < 1 {
         return Err(PeerTransportError::MessageFailed);
      }

      let id = bytes[4];
      let payload = &bytes[5..4 + length];

      match id {
         0 => Ok(PeerMessages::Choke),
         1 => Ok(PeerMessages::Unchoke),
         2 => Ok(PeerMessages::Interested),
         3 => Ok(PeerMessages::NotInterested),
         4 => {
            if payload.len() != 4 {
               return Err(PeerTransportError::MessageFailed);
            }
            let index = u32::from_be_bytes(payload[0..4].try_into().unwrap());
            Ok(PeerMessages::Have(index))
         }
         5 => Ok(PeerMessages::Bitfield(BitVec::from_slice(payload))),
         6 => {
            if payload.len() != 12 {
               return Err(PeerTransportError::MessageFailed);
            }
            let index = u32::from_be_bytes(payload[0..4].try_into().unwrap());
            let begin = u32::from_be_bytes(payload[4..8].try_into().unwrap());
            let length = u32::from_be_bytes(payload[8..12].try_into().unwrap());
            Ok(PeerMessages::Request(index, begin, length))
         }
         7 => {
            if payload.len() < 8 {
               return Err(PeerTransportError::MessageFailed);
            }
            let index = u32::from_be_bytes(payload[0..4].try_into().unwrap());
            let begin = u32::from_be_bytes(payload[4..8].try_into().unwrap());
            let data = payload[8..].to_vec();
            Ok(PeerMessages::Piece(index, begin, data))
         }
         8 => {
            if payload.len() != 12 {
               return Err(PeerTransportError::MessageFailed);
            }
            let index = u32::from_be_bytes(payload[0..4].try_into().unwrap());
            let begin = u32::from_be_bytes(payload[4..8].try_into().unwrap());
            let length = u32::from_be_bytes(payload[8..12].try_into().unwrap());
            Ok(PeerMessages::Cancel(index, begin, length))
         }
         _ => Err(PeerTransportError::Other(anyhow!("Unknown message type"))),
      }
   }
}

/// BitTorrent Handshake message structure
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
   /// Protocol identifier (typically "BitTorrent protocol")
   pub protocol: Vec<u8>,
   /// Reserved bytes for protocol extensions
   pub reserved: [u8; 8],
   /// 20-byte SHA1 hash of the info dictionary
   pub info_hash: Arc<Hash<20>>,
   /// 20-byte peer identifier
   pub peer_id: Arc<Hash<20>>,
}

impl Handshake {
   /// Create a new handshake with the given info hash and peer ID
   pub fn new(info_hash: Arc<Hash<20>>, peer_id: Arc<Hash<20>>) -> Self {
      Self {
         protocol: MAGIC_STRING.to_vec(),
         reserved: [0u8; 8],
         info_hash,
         peer_id,
      }
   }

   /// Serialize the handshake to bytes in the BitTorrent wire format
   pub fn to_bytes(&self) -> Vec<u8> {
      // Calculate total size: 1 byte for length + protocol + 8 reserved + 20 info_hash + 20 peer_id
      let mut bytes = Vec::with_capacity(1 + self.protocol.len() + 8 + 40);

      // Protocol length (as a single byte)
      bytes.push(self.protocol.len() as u8);
      // Protocol string
      bytes.extend_from_slice(&self.protocol);
      // Reserved bytes
      bytes.extend_from_slice(&self.reserved);
      // Info hash
      bytes.extend_from_slice(self.info_hash.as_bytes());
      // Peer ID
      bytes.extend_from_slice(self.peer_id.as_bytes());

      bytes
   }

   /// Deserialize a handshake from bytes
   pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
      if bytes.is_empty() {
         return Err("handshake too short");
      }

      // First byte is protocol string length
      let protocol_len = bytes[0] as usize;
      let total_expected_len = 1 + protocol_len + 8 + 40;

      if bytes.len() < total_expected_len {
         return Err("handshake too short");
      }

      // Extract protocol string
      let protocol = bytes[1..1 + protocol_len].to_vec();

      // Extract reserved bytes
      let reserved = bytes[1 + protocol_len..1 + protocol_len + 8]
         .try_into()
         .map_err(|_| "failed to extract reserved bytes")?;

      // Extract info hash
      let info_hash_bytes = bytes[1 + protocol_len + 8..1 + protocol_len + 8 + 20]
         .try_into()
         .map_err(|_| "failed to extract info hash")?;
      let info_hash = Arc::new(Hash::new(info_hash_bytes));

      // Extract peer ID
      let peer_id_bytes = bytes[1 + protocol_len + 8 + 20..1 + protocol_len + 8 + 40]
         .try_into()
         .map_err(|_| "failed to extract peer ID")?;
      let peer_id = Arc::new(Hash::new(peer_id_bytes));

      Ok(Handshake {
         protocol,
         reserved,
         info_hash,
         peer_id,
      })
   }
}
