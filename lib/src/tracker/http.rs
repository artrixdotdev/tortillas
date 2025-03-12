use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize, de};

use super::Peer;

#[derive(Debug, Deserialize)]
pub struct TrackerResponse {
   pub interval: usize,
   #[serde(deserialize_with = "deserialize_peers")]
   pub peers: Vec<Peer>,
}

#[derive(Debug, Deserialize, Serialize)]
struct TrackerRequest {
   peer_id: String,
   port: u16,
   uploaded: u8,
   downloaded: u8,
   left: u8,
   compact: u8,
}

fn deserialize_peers<'de, D>(deserializer: D) -> Result<Vec<Peer>, D::Error>
where
   D: serde::Deserializer<'de>,
{
   let bytes = Vec::<u8>::deserialize(deserializer).expect("Invalid bytes");

   let mut peers = Vec::new();
   for chunk in bytes.chunks(6) {
      if chunk.len() != 6 {
         return Err(de::Error::custom("Invalid peer chunk length"));
      }
      let ip = Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]);

      let port = u16::from_be_bytes([chunk[4], chunk[5]]);
      peers.push(Peer { ip, port });
   }

   Ok(peers)
}
