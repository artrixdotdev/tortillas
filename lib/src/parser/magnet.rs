use std::collections::HashMap;

use crate::{parser::MetaInfo, tracker::Tracker};

use anyhow::Result;
use serde::Deserialize;
use serde_qs;

/// Magnet URI Spec: https://en.wikipedia.org/wiki/Magnet_URI_scheme or https://www.bittorrent.org/beps/bep_0053.html
#[derive(Debug, Deserialize)]
pub struct MagnetUri {
   #[serde(rename(deserialize = "xt"))]
   pub info_hash: String,

   #[serde(rename(deserialize = "dn"))]
   pub name: String,

   #[serde(rename(deserialize = "xl"))]
   pub length: Option<u32>,

   #[serde(rename(deserialize = "tr"))]
   pub announce_list: Option<Vec<Tracker>>,

   #[serde(rename(deserialize = "ws"))]
   pub web_seed: Option<String>,

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
   pub async fn parse(uri: String) -> Result<MetaInfo> {
      let qs = uri.split('?').last().unwrap(); // Turns magnet:?xt=... into xt=...

      // First pass: collect all key-value pairs, grouping repeating keys
      let mut grouped_params: HashMap<String, Vec<String>> = HashMap::new();

      for param in qs.split('&') {
         if param.is_empty() {
            continue;
         }

         let mut parts = param.split('=');
         let key = parts.next().unwrap().to_string();
         let value = parts.next().map(|v| v.to_string()).unwrap_or_default();

         grouped_params.entry(key).or_default().push(value);
      }

      // Second pass: construct the new query string with array notation for repeating keys
      let mut final_params = Vec::new();

      for (key, values) in grouped_params {
         if values.len() == 1 {
            // Single values remain as normal key=value
            final_params.push(format!("{}={}", key, values[0]));
         } else {
            // Multiple values become key[0]=value1&key[1]=value2...
            for (idx, value) in values.iter().enumerate() {
               final_params.push(format!("{}[{}]={}", key, idx, value));
            }
         }
      }

      let final_qs = final_params.join("&");

      // Parse the modified query string
      Ok(MetaInfo::MagnetUri(serde_qs::from_str(&final_qs)?))
   }
}

#[cfg(test)]
mod tests {

   use super::*;

   #[tokio::test]
   async fn test_parse_magnet_uri() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MagnetUri::parse(contents).await.unwrap();

      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            assert_eq!(magnet.name, "Big Buck Bunny");
         }
         _ => panic!("Expected Torrent"),
      }
   }
}
