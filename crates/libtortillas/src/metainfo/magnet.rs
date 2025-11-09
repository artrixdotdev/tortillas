use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_querystring;

use crate::{
   hashes::{Hash, InfoHash},
   metainfo::MetaInfo,
   tracker::Tracker,
};

/// Magnet URI Spec: <https://en.wikipedia.org/wiki/Magnet_URI_scheme> or <https://www.bittorrent.org/beps/bep_0053.html>
#[derive(Debug, Clone, Deserialize)]
pub struct MagnetUri {
   /// use `Self::info_hash` to get the info hash as a `Hash` struct.
   #[serde(rename = "xt")]
   info_hash: String,

   #[serde(rename = "dn")]
   pub name: String,

   #[serde(rename = "xl")]
   pub length: Option<usize>,

   #[serde(rename = "tr")]
   pub announce_list: Option<Vec<Tracker>>,

   #[serde(rename = "ws", default)]
   pub web_seed: Vec<String>,

   #[serde(rename = "as", default)]
   pub source: Vec<String>,

   #[serde(rename = "xs", default)]
   pub exact_source: Vec<String>,

   #[serde(rename = "kt")]
   pub keywords: Option<Vec<String>>,

   #[serde(rename = "mt")]
   pub manifest_topic: Option<String>,

   #[serde(rename = "so")]
   pub select_only: Option<Vec<String>>,

   #[serde(rename = "x.pe", default)]
   pub peer: Vec<String>,

   #[serde(skip)]
   uri: String,
}

impl Serialize for MagnetUri {
   fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
   where
      S: serde::Serializer,
   {
      serializer.serialize_str(&self.uri)
   }
}

impl TryFrom<String> for MagnetUri {
   type Error = anyhow::Error;
   fn try_from(uri: String) -> Result<Self, Self::Error> {
      let qs = uri
         .split('?')
         .nth(1)
         .ok_or_else(|| anyhow::anyhow!("Invalid magnet URI"))?;

      let mut magnet: Self =
         serde_querystring::from_str(qs, serde_querystring::ParseMode::Duplicate)?;

      magnet.uri = uri;
      Ok(magnet)
   }
}

impl MagnetUri {
   pub fn parse(uri: String) -> Result<MetaInfo> {
      Ok(MetaInfo::MagnetUri(MagnetUri::try_from(uri)?))
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

   pub fn deserialize<'de, D>(deserializer: D) -> Result<Self, D::Error>
   where
      D: Deserializer<'de>,
   {
      let uri = String::deserialize(deserializer)?;
      Self::try_from(uri).map_err(de::Error::custom)
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

   #[tokio::test]
   #[traced_test]
   async fn test_parse_magnet_uri_multi_valued_params() {
      let uri = "magnet:?xt=urn:btih:xyz&as=seed1&as=seed2&xs=exact1&xs=exact2&x.pe=peer1&x.pe=peer2&dn=name";
      let magnet = MagnetUri::try_from(uri.to_string()).unwrap();

      assert_eq!(magnet.source.len(), 2);
      assert_eq!(magnet.exact_source.len(), 2);
      assert_eq!(magnet.peer.len(), 2);
   }
}
