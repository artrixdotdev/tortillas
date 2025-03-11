use std::{fmt, io::Read};

use anyhow::Result;
use serde::{
   de::{self, Visitor},
   Deserialize, Serialize, Serializer,
};

mod file;
mod magnet;

pub use file::*;
pub use magnet::*;
use sha1::{Digest, Sha1};

/// An Announce URI from a torrent file or magnet URI.
/// https://www.bittorrent.org/beps/bep_0012.html
/// Example: udp://tracker.opentrackr.org:1337/announce
#[derive(Debug, Deserialize)]
pub struct AnnounceUri(String);

/// Always utilize MetaInfo instead of directly using TorrentFile or MagnetUri
#[derive(Debug, Deserialize)]
pub enum MetaInfo {
   Torrent(TorrentFile),
   MagnetUri(MagnetUri),
}

#[derive(Debug, Deserialize)]
pub struct TorrentFile {
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

/// Magnet URI Spec: https://en.wikipedia.org/wiki/Magnet_URI_scheme or https://www.bittorrent.org/beps/bep_0053.html
#[derive(Debug, Deserialize)]
pub struct MagnetUri {
   #[serde(rename(deserialize = "xt"))]
   info_hash: String,

   #[serde(rename(deserialize = "dn"))]
   name: String,

   #[serde(rename(deserialize = "xl"))]
   length: Option<u32>,

   #[serde(rename(deserialize = "tr"))]
   announce_list: Option<Vec<AnnounceUri>>,

   #[serde(rename(deserialize = "ws"))]
   web_seed: String,

   #[serde(rename(deserialize = "as"))]
   source: Option<String>,

   #[serde(rename(deserialize = "xs"))]
   exact_source: Option<String>,

   #[serde(rename(deserialize = "kt"))]
   keywords: Option<Vec<String>>,

   #[serde(rename(deserialize = "mt"))]
   manifest_topic: Option<String>,

   #[serde(rename(deserialize = "so"))]
   select_only: Option<Vec<String>>,

   #[serde(rename(deserialize = "x.pe"))]
   peer: Option<String>,
}

/// Struct for TorrentFile
#[derive(Debug, Serialize, Deserialize)]
pub struct Info {
   name: String,
   #[serde(rename = "piece length")]
   piece_length: u64,
   /// Binary string of concatenated 20-byte SHA-1 hash values
   pieces: Hashes,
   #[serde(flatten)]
   file: InfoKeys,

   source: Option<String>,
}

impl Info {
   /// Gets the file hash (xt, or exact topic) for the given Info struct
   pub fn hash(&self) -> Result<String> {
      let mut hasher = Sha1::new();
      hasher.update(serde_bencode::to_bytes(&self)?);
      let result = hasher.finalize();
      Ok(hex::encode(result))
   }
}

impl MetaInfo {
   /// Returns the info hash for the given MetaInfo enum. If the enum is a [Torrent](TorrentFile), then this
   /// function will calculate and return the hash. If the enum is a [MagnetUri](MagnetUri), then this
   /// function will grab the existing hash and return it, as the MagnetUri spec already contains
   /// the hash.
   pub fn info_hash(&self) -> String {
      match &self {
         MetaInfo::Torrent(torrent) => torrent.info.hash().unwrap(),
         MetaInfo::MagnetUri(magnet_uri) => {
            String::from(magnet_uri.info_hash.split(":").last().unwrap())
         }
      }
   }
}

#[cfg(test)]
mod tests {

   use super::*;

   #[tokio::test]
   async fn test_info_hash_with_magneturi() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = parse_magnet_uri(contents).await.unwrap();
      assert_eq!(
         metainfo.info_hash(),
         "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c"
      );
   }

   #[tokio::test]
   async fn test_info_hash_with_torrent() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/torrents/big-buck-bunny.torrent");
      let file = parse_file(path).await.unwrap();
      assert_eq!(file.info_hash(), "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c");
   }
}

/// A custom type for serializing and deserializing a vector of 20-byte SHA-1 hashes.
/// Credit to [Jon Gjengset](https://github.com/jonhoo/codecrafters-bittorrent-rust/)
#[derive(Debug)]
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

impl Serialize for Hashes {
   fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
   where
      S: Serializer,
   {
      let single_slice = self.0.concat();

      serializer.serialize_bytes(&single_slice)
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InfoKeys {
   Single {
      length: u64,
   },
   Multi {
      #[serde(default)]
      files: Vec<InfoFile>,
   },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfoFile {
   /// The length of the file, in bytes.
   length: usize,

   /// Subdirectory names for this file, the last of which is the actual file name
   /// (a zero length list is an error case).
   path: Vec<String>,
}
