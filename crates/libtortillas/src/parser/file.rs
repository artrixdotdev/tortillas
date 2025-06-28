use crate::{
   hashes::{Hash, HashVec, InfoHash},
   parser::MetaInfo,
   tracker::Tracker,
};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_bencode as bencode;
use sha1::{Digest, Sha1};
use std::path::PathBuf;

use tokio::fs;

#[derive(Debug, Deserialize)]
pub struct TorrentFile {
   /// The primary announce URI for the torrent.
   pub announce: Tracker,
   /// Secondary announce URIs for different trackers, and protocols. Also can be used as a backup
   #[serde(rename(deserialize = "announce-list"))]
   pub announce_list: Option<Vec<Vec<Tracker>>>, // Note: This is a list of lists
   pub comment: Option<String>,
   #[serde(rename(deserialize = "created by"))]
   pub created_by: Option<String>,
   #[serde(rename(deserialize = "creation date"))]
   pub creation_date: Option<i64>, // Typically stored as unix timestamp
   pub encoding: Option<String>,
   pub info: Info,
   pub url_list: Option<Vec<String>>,
}

impl TorrentFile {
   /// Parse torrent file into [`Metainfo`](super::MetaInfo).
   pub async fn read(path: PathBuf) -> Result<MetaInfo> {
      let file = fs::read(path).await?;
      Self::parse(&file)
   }
   pub fn parse(bytes: &[u8]) -> Result<MetaInfo> {
      let metainfo: MetaInfo = MetaInfo::Torrent(bencode::from_bytes(bytes)?);
      Ok(metainfo)
   }
}

/// Struct for TorrentFile
#[derive(Debug, Serialize, Deserialize)]
pub struct Info {
   name: String,
   #[serde(rename = "piece length")]
   piece_length: u64,
   /// Binary string of concatenated 20-byte SHA-1 hash values
   pieces: HashVec<20>,
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
   pub fn hash(&self) -> Result<InfoHash> {
      let mut hasher = Sha1::new();
      hasher.update(serde_bencode::to_bytes(&self)?);
      let result = hasher.finalize();
      Ok(Hash::from_hex(hex::encode(result))?)
   }
}

#[cfg(test)]
mod tests {
   use super::*;
   use tracing_test::traced_test;

   #[tokio::test]
   #[traced_test]
   async fn test_parse_file() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/torrents/big-buck-bunny.torrent");

      let metainfo = TorrentFile::read(path).await.unwrap();

      match metainfo {
         MetaInfo::Torrent(torrent) => {
            assert_eq!(torrent.info.name, "Big Buck Bunny");
         }
         _ => panic!("Expected Torrent"),
      }
   }
}
