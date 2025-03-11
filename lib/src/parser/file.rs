use super::{AnnounceUri, MetaInfo};
use anyhow::Result;
use serde::{
   Deserialize, Serialize, Serializer,
   de::{self, Visitor},
};
use serde_bencode as bencode;
use sha1::{Digest, Sha1};
use std::{fmt, path::PathBuf};

use tokio::fs;

#[derive(Debug, Deserialize)]
pub struct TorrentFile {
   /// The primary announce URI for the torrent.
   pub announce: AnnounceUri,
   /// Secondary announce URIs for different trackers, and protocols. Also can be used as a backup
   #[serde(rename(deserialize = "announce-list"))]
   pub announce_list: Option<Vec<Vec<AnnounceUri>>>, // Note: This is a list of lists
   pub comment: Option<String>,
   #[serde(rename(deserialize = "created by"))]
   pub created_by: Option<String>,
   #[serde(rename(deserialize = "creation date"))]
   pub creation_date: Option<i64>, // Typically stored as unix timestamp
   pub encoding: Option<String>,
   pub info: Info,
   pub url_list: Option<Vec<String>>,
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

impl Info {
   /// Gets the file hash (xt, or exact topic) for the given Info struct
   pub fn hash(&self) -> Result<String> {
      let mut hasher = Sha1::new();
      hasher.update(serde_bencode::to_bytes(&self)?);
      let result = hasher.finalize();
      Ok(hex::encode(result))
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

/// Parse torrent file into [`Metainfo`](super::MetaInfo).
pub async fn parse_file(path: PathBuf) -> Result<MetaInfo> {
   let file = fs::read(path).await?;
   let metainfo: MetaInfo = MetaInfo::Torrent(bencode::from_bytes(&file)?);
   Ok(metainfo)
}

#[cfg(test)]
mod tests {
   use super::*;

   #[tokio::test]
   async fn test_parse_file() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/torrents/big-buck-bunny.torrent");

      let metainfo = parse_file(path).await.unwrap();

      match metainfo {
         MetaInfo::Torrent(torrent) => {
            assert_eq!(torrent.info.name, "Big Buck Bunny");
         }
         _ => panic!("Expected Torrent"),
      }
   }
}
