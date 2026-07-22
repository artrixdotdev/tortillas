use std::fmt;

use crate::hashes::{Hash, InfoHash};

/// Byte width of DHT node IDs and info hashes defined by BEP 5.
pub const DHT_ID_LEN: usize = 20;

/// A DHT node identifier backed by the project's canonical 20-byte hash type.
///
/// [BEP 5 terminology] distinguishes a DHT node, identified for UDP routing,
/// from a BitTorrent peer, identified on peer-wire connections. The wrapper
/// prevents accidentally sending a peer ID in a KRPC field while sharing the
/// same [`struct@Hash<20>`] storage and conversion code as info hashes.
///
/// [BEP 5 terminology]: https://www.bittorrent.org/beps/bep_0005.html
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NodeId(Hash<20>);

impl NodeId {
   /// Generates an identifier suitable for a new ephemeral DHT node.
   pub fn random() -> Self {
      Self(Hash::from_bytes(rand::random()))
   }

   /// Creates an identifier from its wire representation.
   #[cfg(test)]
   pub const fn from_bytes(bytes: [u8; DHT_ID_LEN]) -> Self {
      Self(Hash::from_bytes(bytes))
   }

   /// Returns the wire representation of this identifier.
   pub const fn as_bytes(&self) -> &[u8; DHT_ID_LEN] {
      self.0.as_bytes()
   }

   /// Returns the XOR distance from another DHT key.
   pub fn distance(self, other: Self) -> Distance {
      let mut bytes = [0; DHT_ID_LEN];
      for (output, (left, right)) in bytes
         .iter_mut()
         .zip(self.as_bytes().iter().zip(other.as_bytes()))
      {
         *output = left ^ right;
      }
      Distance(bytes)
   }
}

impl From<InfoHash> for NodeId {
   fn from(info_hash: InfoHash) -> Self {
      Self(info_hash)
   }
}

impl TryFrom<&[u8]> for NodeId {
   type Error = anyhow::Error;

   fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
      Ok(Self(Hash::try_from(bytes.to_vec())?))
   }
}

impl fmt::Display for NodeId {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      write!(f, "{}", self.0)
   }
}

/// XOR distance between two 160-bit DHT keys.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Distance([u8; DHT_ID_LEN]);

impl Distance {
   /// Returns the Kademlia bucket index, with zero representing the nearest
   /// non-identical bucket and 159 the farthest bucket.
   pub fn bucket_index(self) -> Option<usize> {
      for (byte_index, byte) in self.0.iter().enumerate() {
         if *byte != 0 {
            let significant_bit = 7 - byte.leading_zeros() as usize;
            return Some((DHT_ID_LEN - 1 - byte_index) * u8::BITS as usize + significant_bit);
         }
      }
      None
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn distance_when_keys_differ_then_orders_by_xor_value() {
      let origin = NodeId::from_bytes([0; DHT_ID_LEN]);
      let mut near = [0; DHT_ID_LEN];
      near[DHT_ID_LEN - 1] = 1;
      let mut far = [0; DHT_ID_LEN];
      far[0] = 0x80;

      assert!(origin.distance(NodeId::from_bytes(near)) < origin.distance(NodeId::from_bytes(far)));
      assert_eq!(
         origin.distance(NodeId::from_bytes(near)).bucket_index(),
         Some(0)
      );
      assert_eq!(
         origin.distance(NodeId::from_bytes(far)).bucket_index(),
         Some(159)
      );
   }

   #[test]
   fn distance_when_keys_match_then_has_no_bucket() {
      let node = NodeId::from_bytes([42; DHT_ID_LEN]);

      assert_eq!(node.distance(node).bucket_index(), None);
   }

   #[test]
   fn node_id_when_info_hash_is_supplied_then_reuses_hash_storage() {
      let hash = Hash::from_bytes([7; DHT_ID_LEN]);

      assert_eq!(NodeId::from(hash).as_bytes(), hash.as_bytes());
   }
}
