use anyhow::Result;
use serde::{
   Deserialize, Serialize,
   de::{self, Visitor},
};
use std::{fmt, net::Ipv4Addr};

#[derive(Debug, Deserialize)]
pub struct TrackerResponse {
   pub interval: usize,
   #[serde(deserialize_with = "deserialize_peers")]
   pub peers: Vec<Peer>,
}

#[derive(Debug, Deserialize)]
pub struct Peer {
   pub ip: Ipv4Addr,
   pub port: u16,
}

/// An Announce URI from a torrent file or magnet URI.
/// https://www.bittorrent.org/beps/bep_0012.html
/// Example: udp://tracker.opentrackr.org:1337/announce
#[derive(Debug)]
pub enum AnnounceUri {
   Http(String),
   Udp(String),
   Websocket(String),
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

#[derive(Debug, Deserialize, Serialize)]
struct TrackerRequest {
   peer_id: String,
   port: u16,
   uploaded: u8,
   downloaded: u8,
   left: u8,
   compact: u8,
}

impl AnnounceUri {
   pub async fn get(&self, info_hash: String) -> Result<TrackerResponse> {
      match self {
         AnnounceUri::Http(_) => todo!(),
         AnnounceUri::Udp(_) => todo!(),
         AnnounceUri::Websocket(_) => todo!(),
      }
   }
}

struct AnnounceUriVisitor;

impl<'de> Deserialize<'de> for AnnounceUri {
   fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
   where
      D: serde::Deserializer<'de>,
   {
      deserializer.deserialize_string(AnnounceUriVisitor)
   }
}

impl Visitor<'_> for AnnounceUriVisitor {
   type Value = AnnounceUri;

   fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
      formatter.write_str("a string")
   }

   // Alittle DRY code here but its fine (surely)
   fn visit_string<E>(self, s: String) -> Result<Self::Value, E>
   where
      E: de::Error,
   {
      Ok(match s.split("://").collect::<Vec<&str>>()[0] {
         "http" | "https" => AnnounceUri::Http(s),
         "udp" => AnnounceUri::Udp(s),
         "ws" | "wss" => AnnounceUri::Websocket(s),
         _ => panic!(),
      })
   }

   fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
   where
      E: de::Error,
   {
      Ok(match s.split("://").collect::<Vec<&str>>()[0] {
         "http" | "https" => AnnounceUri::Http(s.to_string()),
         "udp" => AnnounceUri::Udp(s.to_string()),
         "ws" | "wss" => AnnounceUri::Websocket(s.to_string()),
         _ => panic!(),
      })
   }
}
