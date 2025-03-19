use std::fmt::{self, Display};

use serde::{
   Deserialize, Deserializer, Serialize, Serializer,
   de::{self, Visitor},
};

#[derive(Debug, Clone, Copy)]
pub struct Hash<const N: usize>([u8; N]);

pub type InfoHash = Hash<20>;

impl<const N: usize> Hash<N> {
   pub fn new(bytes: [u8; N]) -> Self {
      Hash(bytes)
   }

   pub fn from_bytes(bytes: [u8; N]) -> Self {
      Hash(bytes)
   }

   pub fn as_bytes(&self) -> &[u8; N] {
      &self.0
   }
   pub fn to_hex(&self) -> String {
      hex::encode(self.0)
   }
   pub fn from_hex(hex: impl AsRef<[u8]>) -> Result<Self, hex::FromHexError> {
      let bytes = hex::decode(hex)?;
      if bytes.len() == N {
         Ok(Hash(bytes.try_into().unwrap()))
      } else {
         Err(hex::FromHexError::InvalidStringLength)
      }
   }
}

/// Auto turns the hash into a hexadecimal string when displayed in a string context
impl<const N: usize> Display for Hash<N> {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      write!(f, "{}", self.to_hex())
   }
}

impl<'de, const N: usize> Deserialize<'de> for Hash<N> {
   fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
   where
      D: Deserializer<'de>,
   {
      deserializer.deserialize_bytes(HashVisitor::<N>)
   }
}

impl<const N: usize> Serialize for Hash<N> {
   fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
   where
      S: Serializer,
   {
      serializer.serialize_bytes(&self.0)
   }
}

struct HashVisitor<const N: usize>;

impl<const N: usize> Visitor<'_> for HashVisitor<N> {
   type Value = Hash<N>;

   fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
      formatter.write_str(&format!("a byte string whose length is  {}", N))
   }

   fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
   where
      E: de::Error,
   {
      if v.len() == N {
         Ok(Hash(v.try_into().unwrap()))
      } else {
         Err(E::custom(format!(
            "Expect length is {}, found {}",
            N,
            v.len(),
         )))
      }
   }
}
/// A vector of [Hashes](Hash) of the same length
#[derive(Debug)]
pub struct HashVec<const N: usize>(Vec<Hash<N>>);

impl<const N: usize> Default for HashVec<N> {
   fn default() -> Self {
      Self::new()
   }
}

impl<const N: usize> HashVec<N> {
   pub fn new() -> Self {
      HashVec(Vec::new())
   }

   pub fn push(&mut self, hash: Hash<N>) {
      self.0.push(hash);
   }

   pub fn len(&self) -> usize {
      self.0.len()
   }

   pub fn is_empty(&self) -> bool {
      self.0.is_empty()
   }

   /// Flatten the vector of hashes into a single vector of bytes.
   /// # Example
   /// ```
   /// let hashes = HashVec::new();
   /// hashes.push(Hash([0, 0, 0]));
   /// hashes.push(Hash([1, 1, 2]));
   /// let flattened = hashes.flatten();
   /// assert_eq!(flattened, vec![0, 0, 0, 1, 1, 2]);
   /// ```
   pub fn flatten(&self) -> Vec<u8> {
      let mut result = Vec::with_capacity(N * self.len());
      for hash in &self.0 {
         result.extend_from_slice(&hash.0);
      }
      result
   }
}

impl<const N: usize> IntoIterator for HashVec<N> {
   type Item = Hash<N>;
   type IntoIter = std::vec::IntoIter<Self::Item>;

   fn into_iter(self) -> Self::IntoIter {
      self.0.into_iter()
   }
}

struct HashVecVisitor<const N: usize>;
impl<const N: usize> HashVecVisitor<N> {
   pub fn new() -> Self {
      HashVecVisitor
   }
}

impl<const N: usize> Visitor<'_> for HashVecVisitor<N> {
   type Value = HashVec<N>;

   fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
      formatter.write_str(&format!(
         "a byte string whose length is a multiple of {}",
         N
      ))
   }

   fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
   where
      E: de::Error,
   {
      if v.len() % N != 0 {
         return Err(E::custom(format!("length is {}", v.len())));
      }
      let mut vec = Vec::with_capacity(v.len() / N);
      for chunk in v.chunks(N) {
         vec.push(Hash(
            chunk
               .try_into()
               .unwrap_or_else(|_| panic!("guaranteed to be length {N}")),
         ));
      }
      Ok(HashVec(vec))
   }
}

impl<const N: usize> Serialize for HashVec<N> {
   fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
   where
      S: Serializer,
   {
      serializer.serialize_bytes(&self.flatten())
   }
}

impl<'de, const N: usize> Deserialize<'de> for HashVec<N> {
   fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
   where
      D: Deserializer<'de>,
   {
      deserializer.deserialize_bytes(HashVecVisitor::new())
   }
}
