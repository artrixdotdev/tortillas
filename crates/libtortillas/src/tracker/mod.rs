use anyhow::Result;
use async_trait::async_trait;
use http::HttpTracker;
use rand::random_range;
use serde::{
   de::{self, Visitor},
   Deserialize,
};
use std::{fmt, net::SocketAddr};
use udp::UdpTracker;

use crate::{hashes::InfoHash, peers::Peer};
pub mod http;
pub mod udp;

#[async_trait]
pub trait TrackerTrait {
   async fn stream_peers(&mut self) -> Result<Vec<Peer>>;
}

/// An Announce URI from a torrent file or magnet URI.
/// <https://www.bittorrent.org/beps/bep_0012.html>
/// Example: <udp://tracker.opentrackr.org:1337/announce>
#[derive(Debug)]
pub enum Tracker {
   /// HTTP Spec
   /// <https://www.bittorrent.org/beps/bep_0003.html>
   Http(String),
   /// UDP Spec
   /// <https://www.bittorrent.org/beps/bep_0015.html>
   Udp(String),
   Websocket(String),
}

impl Tracker {
   pub async fn get_peers(&self, info_hash: InfoHash) -> Result<Vec<Peer>> {
      match self {
         Tracker::Http(uri) => {
            let mut tracker = HttpTracker::new(uri.clone(), info_hash, None);

            Ok(tracker.stream_peers().await.unwrap())
         }
         Tracker::Udp(uri) => {
            let port: u16 = random_range(1024..65535);
            let mut tracker = UdpTracker::new(
               uri.clone(),
               None,
               info_hash,
               Some(SocketAddr::from(([0, 0, 0, 0], port))),
            )
            .await
            .unwrap();

            Ok(tracker.stream_peers().await.unwrap())
         }
         Tracker::Websocket(_) => todo!(),
      }
   }

   pub fn uri(&self) -> String {
      match self {
         Tracker::Http(uri) => uri.clone(),
         Tracker::Udp(uri) => uri.clone(),
         Tracker::Websocket(uri) => uri.clone(),
      }
   }
}

struct TrackerVisitor;

impl<'de> Deserialize<'de> for Tracker {
   fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
   where
      D: serde::Deserializer<'de>,
   {
      deserializer.deserialize_string(TrackerVisitor)
   }
}

impl Visitor<'_> for TrackerVisitor {
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
