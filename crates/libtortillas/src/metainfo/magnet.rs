use anyhow::Result;
use serde::Deserialize;
use serde_querystring;

use crate::{
   hashes::{Hash, InfoHash},
   metainfo::MetaInfo,
   tracker::Tracker,
};

/// Magnet URI Spec: <https://en.wikipedia.org/wiki/Magnet_URI_scheme> or <https://www.bittorrent.org/beps/bep_0053.html>
#[derive(Debug, Deserialize)]
pub struct MagnetUri {
   /// use `Self::info_hash` to get the info hash as a `Hash` struct.
   #[serde(rename(deserialize = "xt"))]
   info_hash: String,

   #[serde(rename(deserialize = "dn"))]
   pub name: String,

   #[serde(rename(deserialize = "xl"))]
   pub length: Option<u32>,

   #[serde(rename(deserialize = "tr"))]
   pub announce_list: Option<Vec<Tracker>>,

   #[serde(rename(deserialize = "ws"), default)]
   pub web_seed: Vec<String>,

   #[serde(rename(deserialize = "as"))]
   pub source: Option<String>,

   #[serde(rename(deserialize = "xs"))]
   pub exact_source: Option<String>,

   #[serde(rename(deserialize = "kt"))]
   pub keywords: Option<Vec<String>>,

   #[serde(rename(deserialize = "mt"))]
   pub manifest_topic: Option<String>,

   #[serde(rename(deserialize = "so"))]
   pub select_only: Option<Vec<String>>,

   #[serde(rename(deserialize = "x.pe"))]
   pub peer: Option<String>,
}

impl MagnetUri {
   pub fn parse(uri: String) -> Result<MetaInfo> {
      let qs = uri.split('?').next_back().unwrap(); // Turns magnet:?xt=... into xt=...
      // Parse the modified query string
      Ok(MetaInfo::MagnetUri(serde_querystring::from_str(
         qs,
         serde_querystring::ParseMode::Duplicate,
      )?))
   }

   pub fn announce_list(&self) -> Vec<Tracker> {
      self.announce_list.clone().unwrap_or_default()
   }

   pub fn info_hash(&self) -> Result<InfoHash, anyhow::Error> {
      let hex_part = self
         .info_hash
         .split(":")
         .last()
         .ok_or_else(|| anyhow::anyhow!("Invalid info_hash format: no colon found"))?;

      Hash::from_hex(hex_part)
         .map_err(|e| anyhow::anyhow!("Failed to parse info_hash from hex: {}", e))
   }
}

#[cfg(test)]
mod tests {

   use tracing_test::traced_test;

   use super::*;

   #[tokio::test]
   #[traced_test]
   async fn test_parse_magnet_uri() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MagnetUri::parse(contents).unwrap();

      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            assert_eq!(magnet.name, "Big Buck Bunny");
         }
         _ => panic!("Expected Torrent"),
      }
   }
}
