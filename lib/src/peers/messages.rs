use bitvec::prelude::*;

use crate::hashes::Hash;
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
   Have(u8) = 4u8,

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
   Request(u8, u8, u8) = 6u8,

   /// A block of data corresponding to a previously sent `Request` message.
   ///
   /// # Binary Layout
   ///
   /// |   0-4   |   4-8   |   8-N   |
   /// |---------|---------|---------|
   /// |  index  |  begin  |  piece  |
   ///
   /// Unexpected pieces might arrive; refer to the [specification](https://www.bittorrent.org/beps/bep_0003.html) for details.
   Piece(u8, u8, Vec<u8>) = 7u8,
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
   Cancel(u8, u8, u8) = 8u8,

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
}

impl PeerMessages {
   pub fn to_bytes(&self) -> Vec<u8> {
      match self {
         PeerMessages::Choke => vec![0],
         PeerMessages::Unchoke => vec![1],
         PeerMessages::Interested => vec![2],
         PeerMessages::NotInterested => vec![3],
         PeerMessages::Have(index) => vec![4, *index],
         PeerMessages::Bitfield(bits) => {
            let mut bytes = vec![5];
            bytes.extend_from_slice(&bits.len().to_be_bytes());
            bytes.extend_from_slice(bits.as_raw_slice());
            bytes
         }
         PeerMessages::Request(index, begin, length) => {
            let mut bytes = vec![6];
            bytes.extend_from_slice(&index.to_be_bytes());
            bytes.extend_from_slice(&begin.to_be_bytes());
            bytes.extend_from_slice(&length.to_be_bytes());
            bytes
         }
         PeerMessages::Piece(index, begin, data) => {
            let mut bytes = vec![6];
            bytes.extend_from_slice(&index.to_be_bytes());
            bytes.extend_from_slice(&begin.to_be_bytes());
            bytes.extend_from_slice(&data.len().to_be_bytes());
            bytes.extend_from_slice(data);
            bytes
         }
         PeerMessages::Cancel(index, begin, length) => {
            let mut bytes = vec![6];
            bytes.extend_from_slice(&index.to_be_bytes());
            bytes.extend_from_slice(&begin.to_be_bytes());
            bytes.extend_from_slice(&length.to_be_bytes());
            bytes
         }
         PeerMessages::Handshake(handshake) => handshake.to_bytes(),
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
