use std::{fmt, ops::BitXor};

use rand::random;

use crate::hashes::InfoHash;

/// The 160-bit identifier of a mainline DHT node.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct NodeId([u8; 20]);

impl NodeId {
   /// Generates an identifier suitable for a new ephemeral DHT node.
   pub fn random() -> Self {
      Self(random())
   }

   /// Creates an identifier from its wire representation.
   pub const fn from_bytes(bytes: [u8; 20]) -> Self {
      Self(bytes)
   }

   /// Returns the wire representation of this identifier.
   pub const fn as_bytes(&self) -> &[u8; 20] {
      &self.0
   }

   /// Returns the XOR distance from another DHT key.
   pub fn distance(self, other: impl Into<Self>) -> Distance {
      self ^ other.into()
   }
}

impl From<InfoHash> for NodeId {
   fn from(info_hash: InfoHash) -> Self {
      Self(*info_hash.as_bytes())
   }
}

impl TryFrom<&[u8]> for NodeId {
   type Error = anyhow::Error;

   fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
      let bytes = bytes
         .try_into()
         .map_err(|_| anyhow::anyhow!("DHT node IDs must contain exactly 20 bytes"))?;
      Ok(Self(bytes))
   }
}

impl fmt::Debug for NodeId {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      write!(f, "NodeId({})", hex::encode(self.0))
   }
}

impl fmt::Display for NodeId {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      write!(f, "{}", hex::encode(self.0))
   }
}

impl BitXor for NodeId {
   type Output = Distance;

   fn bitxor(self, rhs: Self) -> Self::Output {
      let mut bytes = [0; 20];
      for (output, (left, right)) in bytes.iter_mut().zip(self.0.iter().zip(rhs.0)) {
         *output = left ^ right;
      }
      Distance(bytes)
   }
}

/// XOR distance between two 160-bit DHT keys.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Distance([u8; 20]);

impl Distance {
   /// Returns the Kademlia bucket index, with zero representing the nearest
   /// non-identical bucket and 159 the farthest bucket.
   pub fn bucket_index(self) -> Option<usize> {
      for (byte_index, byte) in self.0.iter().enumerate() {
         if *byte != 0 {
            let significant_bit = 7 - byte.leading_zeros() as usize;
            return Some((19 - byte_index) * 8 + significant_bit);
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
      let origin = NodeId::from_bytes([0; 20]);
      let mut near = [0; 20];
      near[19] = 1;
      let mut far = [0; 20];
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
      let node = NodeId::from_bytes([42; 20]);

      assert_eq!(node.distance(node).bucket_index(), None);
   }

   #[test]
   fn node_id_when_wire_length_is_invalid_then_rejects_value() {
      assert!(NodeId::try_from([0; 19].as_slice()).is_err());
   }
}
