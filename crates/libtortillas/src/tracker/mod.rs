use std::{fmt, net::SocketAddr};

use anyhow::Result;
use async_trait::async_trait;
use http::HttpTracker;
use rand::random_range;
use serde::{
   Deserialize,
   de::{self, Visitor},
};
use tokio::sync::mpsc;
use udp::UdpTracker;

use crate::{
   hashes::InfoHash,
   peer::{Peer, PeerId},
};
pub mod http;
pub mod udp;

#[async_trait]
pub trait TrackerTrait: Clone {
   /// Acts as a wrapper function for get_peers. Should be spawned with
   /// tokio::spawn.
   async fn stream_peers(&mut self) -> Result<mpsc::Receiver<Vec<Peer>>>;

   async fn get_peers(&mut self) -> Result<Vec<Peer>>;
}

/// An Announce URI from a torrent file or magnet URI.
/// <https://www.bittorrent.org/beps/bep_0012.html>
/// Example: <udp://tracker.opentrackr.org:1337/announce>
#[derive(Debug, Clone)]
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
   /// Gets peers based off of given tracker
   pub async fn get_peers(
      &self, info_hash: InfoHash, peer_id: Option<PeerId>,
   ) -> Result<Vec<Peer>> {
      match self {
         Tracker::Http(uri) => {
            let mut tracker = HttpTracker::new(uri.clone(), info_hash, peer_id, None);

            Ok(tracker.get_peers().await.unwrap())
         }
         Tracker::Udp(uri) => {
            let port: u16 = random_range(1024..65535);
            let mut tracker = UdpTracker::new(
               uri.clone(),
               None,
               info_hash,
               Some(SocketAddr::from(([0, 0, 0, 0], port))),
               peer_id,
            )
            .await
            .unwrap();

            Ok(tracker.get_peers().await.unwrap())
         }
         Tracker::Websocket(_) => todo!(),
      }
   }

   /// Streams peers based off of given tracker
   pub async fn stream_peers(
      &self, info_hash: InfoHash, peer_addr: Option<SocketAddr>, peer_id: PeerId,
   ) -> Result<mpsc::Receiver<Vec<Peer>>> {
      match self {
         Tracker::Http(uri) => {
            let mut tracker = HttpTracker::new(uri.clone(), info_hash, Some(peer_id), peer_addr);
            Ok(tracker.stream_peers().await.unwrap())
         }
         Tracker::Udp(uri) => {
            let mut tracker =
               UdpTracker::new(uri.clone(), None, info_hash, peer_addr, Some(peer_id))
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
