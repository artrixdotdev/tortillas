use std::sync::Arc;

use crate::hashes::Hash;

pub const MAGIC_STRING: &[u8] = b"BitTorrent protocol";

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
      if bytes.len() < 1 {
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
