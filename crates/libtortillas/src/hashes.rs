use std::fmt::{self, Display};

use serde::{
   Deserialize, Deserializer, Serialize, Serializer,
   de::{self, Visitor},
};

/// A fixed-length byte array that can represent various hash values or identifiers.
///
/// `Hash<N>` is a generic wrapper around a byte array of length `N` that provides
/// convenient methods for conversion between different representations.
///
/// # Examples
///
/// ```
/// use libtortillas::hashes::Hash;
///
/// // Create a hash from a byte array
/// let hash = Hash::new([0; 5]);
/// assert_eq!(hash.to_hex(), "0000000000");
///
/// // Create a hash from a hex string
/// let from_hex = Hash::<5>::from_hex("0102030405").unwrap();
/// assert_eq!(from_hex.as_bytes(), &[1, 2, 3, 4, 5]);
///
/// // Display a hash (automatically converts to hex)
/// println!("Hash: {}", hash); // Outputs: Hash: 0000000000
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hash<const N: usize>([u8; N]);

/// A specialized Hash type for BitTorrent info hashes (20 bytes/160 bits)
pub type InfoHash = Hash<20>;

impl<const N: usize> Hash<N> {
   /// Creates a new Hash from a byte array of length N.
   ///
   /// # Examples
   ///
   /// ```
   /// use libtortillas::hashes::Hash;
   ///
   /// let hash = Hash::new([1, 2, 3, 4, 5]);
   /// ```
   pub fn new(bytes: [u8; N]) -> Self {
      Hash(bytes)
   }

   /// Creates a new Hash from a byte array of length N.
   /// This is an alias for `new()` with a more descriptive name.
   ///
   /// # Examples
   ///
   /// ```
   /// use libtortillas::hashes::Hash;
   ///
   /// let hash = Hash::from_bytes([1, 2, 3, 4, 5]);
   /// ```
   pub fn from_bytes(bytes: [u8; N]) -> Self {
      Hash(bytes)
   }

   /// Returns a reference to the underlying byte array.
   ///
   /// # Examples
   ///
   /// ```
   /// use libtortillas::hashes::Hash;
   ///
   /// let hash = Hash::new([1, 2, 3, 4, 5]);
   /// assert_eq!(hash.as_bytes(), &[1, 2, 3, 4, 5]);
   /// ```
   pub fn as_bytes(&self) -> &[u8; N] {
      &self.0
   }

   /// Converts the hash to a hexadecimal string representation.
   ///
   /// # Examples
   ///
   /// ```
   /// use libtortillas::hashes::Hash;
   ///
   /// let hash = Hash::new([0x12, 0x34, 0x56]);
   /// assert_eq!(hash.to_hex(), "123456");
   /// ```
   pub fn to_hex(&self) -> String {
      hex::encode(self.0)
   }

   /// Creates a Hash from a hexadecimal string.
   ///
   /// The input string must have exactly 2*N characters (2 hex chars per byte).
   ///
   /// # Errors
   ///
   /// Returns an error if the hex string is invalid or has incorrect length.
   ///
   /// # Examples
   ///
   /// ```
   /// use libtortillas::hashes::Hash;
   ///
   /// let hash = Hash::<3>::from_hex("123456").unwrap();
   /// assert_eq!(hash.as_bytes(), &[0x12, 0x34, 0x56]);
   ///
   /// // Invalid length
   /// assert!(Hash::<3>::from_hex("12345").is_err());
   /// assert!(Hash::<3>::from_hex("1234567").is_err());
   ///
   /// // Invalid hex characters
   /// assert!(Hash::<3>::from_hex("12345G").is_err());
   /// ```
   pub fn from_hex(hex: impl AsRef<[u8]>) -> Result<Self, hex::FromHexError> {
      let bytes = hex::decode(hex)?;
      if bytes.len() == N {
         Ok(Hash(bytes.try_into().unwrap()))
      } else {
         Err(hex::FromHexError::InvalidStringLength)
      }
   }
}

/// Implements the Display trait for Hash, converting it to a hexadecimal string.
///
/// This allows a Hash to be directly used in string formatting contexts.
///
/// # Examples
///
/// ```
/// use libtortillas::hashes::Hash;
///
/// let hash = Hash::new([0x12, 0x34, 0x56]);
/// println!("Hash: {}", hash); // Outputs: Hash: 123456
/// let formatted = format!("Hash value: {}", hash);
/// assert_eq!(formatted, "Hash value: 123456");
/// ```
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
      formatter.write_str(&format!("a byte string whose length is {}", N))
   }

   fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
   where
      E: de::Error,
   {
      if v.len() == N {
         Ok(Hash(v.try_into().unwrap()))
      } else {
         Err(E::custom(format!(
            "Expected length is {}, found {}",
            N,
            v.len(),
         )))
      }
   }
}

/// A collection of `Hash<N>` values that provides efficient storage and operations.
///
/// HashVec is optimized for working with multiple hashes of the same length,
/// providing methods to manipulate them as a collection and serialize/deserialize
/// them efficiently.
///
/// # Examples
///
/// ```
/// use libtortillas::hashes::{Hash, HashVec};
///
/// // Create a new empty collection
/// let mut hashes: HashVec<4> = HashVec::new();
///
/// // Add some hashes
/// hashes.push(Hash::new([1, 2, 3, 4]));
/// hashes.push(Hash::new([5, 6, 7, 8]));
///
/// // Check properties
/// assert_eq!(hashes.len(), 2);
/// assert!(!hashes.is_empty());
///
/// // Iterate over hashes
/// for hash in hashes {
///     println!("Hash: {}", hash);
/// }
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct HashVec<const N: usize>(Vec<Hash<N>>);

impl<const N: usize> Default for HashVec<N> {
   fn default() -> Self {
      Self::new()
   }
}

impl<const N: usize> HashVec<N> {
   /// Creates a new empty HashVec.
   ///
   /// # Examples
   ///
   /// ```
   /// use libtortillas::hashes::HashVec;
   ///
   /// let hashes: HashVec<20> = HashVec::new();
   /// assert!(hashes.is_empty());
   /// ```
   pub fn new() -> Self {
      HashVec(Vec::new())
   }

   /// Adds a hash to the collection.
   ///
   /// # Examples
   ///
   /// ```
   /// use libtortillas::hashes::{Hash, HashVec};
   ///
   /// let mut hashes = HashVec::new();
   /// hashes.push(Hash::new([1, 2, 3]));
   /// assert_eq!(hashes.len(), 1);
   /// ```
   pub fn push(&mut self, hash: Hash<N>) {
      self.0.push(hash);
   }

   /// Returns the number of hashes in the collection.
   ///
   /// # Examples
   ///
   /// ```
   /// use libtortillas::hashes::{Hash, HashVec};
   ///
   /// let mut hashes = HashVec::new();
   /// assert_eq!(hashes.len(), 0);
   ///
   /// hashes.push(Hash::new([1, 2, 3]));
   /// hashes.push(Hash::new([4, 5, 6]));
   /// assert_eq!(hashes.len(), 2);
   /// ```
   pub fn len(&self) -> usize {
      self.0.len()
   }

   /// Returns true if the collection contains no hashes.
   ///
   /// # Examples
   ///
   /// ```
   /// use libtortillas::hashes::{Hash, HashVec};
   ///
   /// let mut hashes = HashVec::new();
   /// assert!(hashes.is_empty());
   ///
   /// hashes.push(Hash::new([1, 2, 3]));
   /// assert!(!hashes.is_empty());
   /// ```
   pub fn is_empty(&self) -> bool {
      self.0.is_empty()
   }

   /// Flattens the vector of hashes into a single vector of bytes.
   ///
   /// This is useful for serialization or when you need to work with
   /// the raw bytes of all hashes concatenated together.
   ///
   /// # Examples
   ///
   /// ```
   /// use libtortillas::hashes::{Hash, HashVec};
   ///
   /// let mut hashes = HashVec::new();
   /// hashes.push(Hash::new([0, 1, 2]));
   /// hashes.push(Hash::new([3, 4, 5]));
   ///
   /// let flattened = hashes.flatten();
   /// assert_eq!(flattened, vec![0, 1, 2, 3, 4, 5]);
   /// ```
   pub fn flatten(&self) -> Vec<u8> {
      let mut result = Vec::with_capacity(N * self.len());
      for hash in &self.0 {
         result.extend_from_slice(&hash.0);
      }
      result
   }
}

/// Implements IntoIterator for HashVec to allow iteration over contained hashes.
///
/// # Examples
///
/// ```
/// use libtortillas::hashes::{Hash, HashVec};
///
/// let mut hashes = HashVec::new();
/// hashes.push(Hash::new([1, 2, 3]));
/// hashes.push(Hash::new([4, 5, 6]));
///
/// let mut sum = 0;
/// for hash in hashes {
///     // Do something with each hash
///     sum += hash.as_bytes().iter().sum::<u8>() as u32;
/// }
/// assert_eq!(sum, 21); // 1+2+3 + 4+5+6 = 21
/// ```
impl<const N: usize> IntoIterator for HashVec<N> {
   type Item = Hash<N>;
   type IntoIter = std::vec::IntoIter<Self::Item>;

   fn into_iter(self) -> Self::IntoIter {
      self.0.into_iter()
   }
}

struct HashVecVisitor<const N: usize>;

impl<const N: usize> HashVecVisitor<N> {
   /// Creates a new HashVecVisitor.
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
         return Err(E::custom(format!(
            "Expected length to be a multiple of {}, found {}",
            N,
            v.len()
         )));
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

impl<const N: usize> From<Vec<Hash<N>>> for HashVec<N> {
   fn from(vec: Vec<Hash<N>>) -> Self {
      HashVec(vec)
   }
}

impl<const N: usize> Display for HashVec<N> {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(f, "HashVec({})", self.flatten().len())
   }
}

impl<const N: usize> std::fmt::Debug for HashVec<N> {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(f, "HashVec([")?;
      for (i, hash) in self.0.iter().enumerate() {
         if i > 0 {
            write!(f, ", ")?;
         }
         write!(f, "{}", hash.to_hex())?;
      }
      write!(f, "])")
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

/// This deserializes a flat byte array into a HashVec, splitting it into
/// individual hashes of length N.
impl<'de, const N: usize> Deserialize<'de> for HashVec<N> {
   fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
   where
      D: Deserializer<'de>,
   {
      deserializer.deserialize_bytes(HashVecVisitor::new())
   }
}
