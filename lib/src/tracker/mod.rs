use anyhow::Result;
use serde::{
   Deserialize,
   de::{self, Visitor},
};
use std::{fmt, net::Ipv4Addr};
mod http;
mod udp;
// mod websocket;

use http::*;
// use udp::*;

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
   /// HTTP Spec
   /// https://www.bittorrent.org/beps/bep_0003.html
   Http(String),
   /// UDP Spec
   /// https://www.bittorrent.org/beps/bep_0015.html
   Udp(String),
   Websocket(String),
}

impl AnnounceUri {
   pub async fn get(&self, info_hash: String) -> Result<TrackerResponse> {
      match self {
         AnnounceUri::Http(_) => todo!(),
         AnnounceUri::Udp(_) => todo!(),
         AnnounceUri::Websocket(_) => todo!(),
      }
   }

   pub fn uri(&self) -> String {
      match self {
         AnnounceUri::Http(uri) => uri.clone(),
         AnnounceUri::Udp(uri) => uri.clone(),
         AnnounceUri::Websocket(uri) => uri.clone(),
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
