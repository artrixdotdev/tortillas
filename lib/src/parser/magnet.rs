use std::collections::HashMap;

use super::MetaInfo;
use anyhow::Result;
use serde_qs;

pub async fn parse_magnet_uri(uri: String) -> Result<MetaInfo> {
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

#[cfg(test)]
mod tests {

   use super::*;

   #[tokio::test]
   async fn test_parse_magnet_uri() {
      let metainfo = parse_magnet_uri("magnet:?xt=urn:btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c&dn=Big+Buck+Bunny&tr=udp%3A%2F%2Fexplodie.org%3A6969&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969&tr=udp%3A%2F%2Ftracker.empire-js.us%3A1337&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=wss%3A%2F%2Ftracker.btorrent.xyz&tr=wss%3A%2F%2Ftracker.fastcast.nz&tr=wss%3A%2F%2Ftracker.openwebtorrent.com&ws=https%3A%2F%2Fwebtorrent.io%2Ftorrents%2F&xs=https%3A%2F%2Fwebtorrent.io%2Ftorrents%2Fbig-buck-bunny.torrent".into()).await.unwrap();

      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            assert_eq!(magnet.name, "Big Buck Bunny");
         }
         _ => panic!("Expected Torrent"),
      }
   }
}
