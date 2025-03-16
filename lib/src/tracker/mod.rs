use anyhow::Result;
use serde::{
   de::{self, Visitor},
   Deserialize,
};
use std::{fmt, net::Ipv4Addr};
use tokio_stream::Stream;
mod http;
mod udp;
// mod websocket;

use http::*;
// use udp::*;

#[derive(Debug, Deserialize)]
pub struct PeerAddr {
   pub ip: Ipv4Addr,
   pub port: u16,
}

trait TrackerTrait {
   async fn stream_peers(&mut self) -> Result<impl Stream<Item = PeerAddr>>;
}

/// An Announce URI from a torrent file or magnet URI.
/// https://www.bittorrent.org/beps/bep_0012.html
/// Example: udp://tracker.opentrackr.org:1337/announce
#[derive(Debug)]
pub enum Tracker {
   /// HTTP Spec
   /// https://www.bittorrent.org/beps/bep_0003.html
   Http(String),
   /// UDP Spec
   /// https://www.bittorrent.org/beps/bep_0015.html
   Udp(String),
   Websocket(String),
}

impl Tracker {
   pub async fn get(&self, info_hash: String) -> Result<()> {
      // match self {
      //    Tracker::Http(_) => todo!(),
      //    Tracker::Udp(_) => todo!(),
      //    Tracker::Websocket(_) => todo!(),
      // }
      todo!();
   }

   pub fn uri(&self) -> String {
      match self {
         Tracker::Http(uri) => uri.clone(),
         Tracker::Udp(uri) => uri.clone(),
         Tracker::Websocket(uri) => uri.clone(),
      }
   }
}

struct AnnounceUriVisitor;

impl<'de> Deserialize<'de> for Tracker {
   fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
   where
      D: serde::Deserializer<'de>,
   {
      deserializer.deserialize_string(AnnounceUriVisitor)
   }
}

impl Visitor<'_> for AnnounceUriVisitor {
   type Value = Tracker;

   fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
      formatter.write_str("a string")
   }

   // Alittle DRY code here but its fine (surely)
   fn visit_string<E>(self, s: String) -> Result<Self::Value, E>
   where
      E: de::Error,
   {
      Ok(match s.split("://").collect::<Vec<&str>>()[0] {
         "http" | "https" => Tracker::Http(s),
         "udp" => Tracker::Udp(s),
         "ws" | "wss" => Tracker::Websocket(s),
         _ => panic!(),
      })
   }

   fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
   where
      E: de::Error,
   {
      Ok(match s.split("://").collect::<Vec<&str>>()[0] {
         "http" | "https" => Tracker::Http(s.to_string()),
         "udp" => Tracker::Udp(s.to_string()),
         "ws" | "wss" => Tracker::Websocket(s.to_string()),
         _ => panic!(),
      })
   }
}
