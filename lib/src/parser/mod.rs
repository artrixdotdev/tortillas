use serde::{
   Deserialize, Serialize,
   de::{self, Visitor},
};
mod file;

pub use file::*;
/// An Announce URI from a torrent file or magnet URI.
/// https://www.bittorrent.org/beps/bep_0012.html
/// Example: udp://tracker.opentrackr.org:1337/announce
#[derive(Debug, Deserialize)]
pub struct AnnounceUri(String);

#[derive(Debug, Deserialize)]
pub struct Metainfo {
   /// The primary announce URI for the torrent.
   announce: AnnounceUri,
   /// Secondary announce URIs for different trackers, and protocols. Also can be used as a backup
   #[serde(rename(deserialize = "announce-list"))]
   announce_list: Option<Vec<Vec<AnnounceUri>>>, // Note: This is a list of lists
   comment: Option<String>,
   #[serde(rename(deserialize = "created by"))]
   created_by: Option<String>,
   #[serde(rename(deserialize = "creation date"))]
   creation_date: Option<i64>, // Typically stored as unix timestamp
   encoding: Option<String>,
   info: Info,
   url_list: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Info {
   #[serde(default)] // This will use Vec::default() if missing
   files: Vec<InfoFile>,
   name: String,
   #[serde(rename = "piece length")]
   piece_length: u64,
   /// Binary string of concatenated 20-byte SHA-1 hash values
   pieces: Hashes,
   // For single-file torrents, length may be here instead of in files
   #[serde(skip_serializing_if = "Option::is_none")]
   length: Option<u64>,
}

/// A custom type for serializing and deserializing a vector of 20-byte SHA-1 hashes.
/// Credit to [Jon Gjengset](https://github.com/jonhoo/codecrafters-bittorrent-rust/)
#[derive(Debug, Serialize)]
pub struct Hashes(pub Vec<[u8; 20]>);

// Add this implementation
impl<'de> Deserialize<'de> for Hashes {
   fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
   where
      D: de::Deserializer<'de>,
   {
      deserializer.deserialize_bytes(HashesVisitor)
   }
}

struct HashesVisitor;

impl<'de> Visitor<'de> for HashesVisitor {
   type Value = Hashes;

   fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
      formatter.write_str("a byte string whose length is a multiple of 20")
   }

   fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
   where
      E: de::Error,
   {
      if v.len() % 20 != 0 {
         return Err(E::custom(format!("length is {}", v.len())));
      }

      Ok(Hashes(
         v.chunks(20)
            .map(|slice_20| slice_20.try_into().expect("guaranteed to be length 20"))
            .collect(),
      ))
   }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct InfoFile {
   length: u64,
   path: Vec<String>,
}
