use anyhow::Result;
use async_trait::async_trait;
use core::str;
use hex::FromHex;
use http::HttpTracker;
use rand::random_range;
use serde::{
   de::{self, Visitor},
   Deserialize, Serialize,
};
use std::{
   fmt,
   net::{Ipv4Addr, SocketAddr},
   str::FromStr,
   time::Duration,
};
use tokio::{sync::mpsc, time::sleep};
use tracing::{trace, warn};
use udp::UdpTracker;
use wss::WssTracker;

use crate::{
   hashes::{Hash, InfoHash},
   peers::Peer,
};
pub mod http;
pub mod udp;
pub mod wss;

// This is AI generated. But it works.
fn hash_to_byte_string(hex_str: &str) -> String {
   // 1) decode hex → raw bytes
   let bytes = Vec::from_hex(hex_str).expect("invalid hex input");

   // 2) build the escaped string
   let mut out = String::new();
   for &b in &bytes {
      match b {
         // common C-style escapes
         0x00 => out.push_str(r"\0"),
         0x07 => out.push_str(r"\a"),
         0x08 => out.push_str(r"\b"),
         0x09 => out.push_str(r"\t"),
         0x0A => out.push_str(r"\n"),
         0x0B => out.push_str(r"\v"),
         0x0C => out.push_str(r"\f"),
         0x0D => out.push_str(r"\r"),

         // any other C0 control → \u00XX
         0x01..=0x06 | 0x0E..=0x1F => {
            out.push_str(&format!(r"\u{:04x}", b));
         }

         // printable ASCII
         0x20..=0x7E => {
            out.push(b as char);
         }

         // high-bit set → ISO-8859-1 codepoint
         _ => {
            let ch = char::from_u32(b as u32).expect("byte → char failed");
            out.push(ch);
         }
      }
   }

   out
}

fn urlencode(t: &[u8; 20]) -> String {
   let mut encoded = String::with_capacity(3 * t.len());

   for &byte in t {
      encoded.push('%');

      let byte = hex::encode([byte]);
      encoded.push_str(&byte);
   }

   encoded
}

#[async_trait]
pub trait TrackerTrait: Clone + 'static {
   /// Acts as a wrapper function for get_peers. Should be spawned with tokio::spawn.
   async fn stream_peers(&mut self) -> Result<mpsc::Receiver<Vec<Peer>>> {
      let (tx, rx) = mpsc::channel(100);

      // Not *super* cheap clone, but not awful
      let mut tracker = self.clone();
      // Very cheap clone
      let interval = self.get_interval();

      let tx = tx.clone();
      // no pre‑captured interval – always read the latest value
      tokio::spawn(async move {
         loop {
            let peers = tracker.get_peers().await.unwrap();
            trace!(
               "Successfully made request to get peers: {}",
               peers.last().unwrap()
            );

            // stop gracefully if the receiver was dropped
            if tx.send(peers).await.is_err() {
               warn!("Receiver dropped – stopping peer stream");
               break;
            }

            // pick up possibly updated interval (never sleep 0s)
            let delay = interval.max(1);
            sleep(Duration::from_secs(delay as u64)).await;
         }
      });
      Ok(rx)
   }

   fn get_interval(&self) -> u32;

   async fn get_peers(&mut self) -> Result<Vec<Peer>>;
}

/// Event. See <https://www.bittorrent.org/beps/bep_0003.html> @ trackers
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Event {
   Started,
   Completed,
   Stopped,
   Empty,
}

/// Tracker request. See <https://www.bittorrent.org/beps/bep_0003.html> @ trackers
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TrackerRequest {
   ip: Option<Ipv4Addr>,
   port: u16,
   uploaded: u8,
   downloaded: u8,
   left: Option<u8>,
   event: Event,
   peer_tracker_addr: SocketAddr,
}

impl TrackerRequest {
   pub fn new(peer_tracker_addr: Option<SocketAddr>) -> TrackerRequest {
      TrackerRequest {
         ip: None,
         port: 6881,
         uploaded: 0,
         downloaded: 0,
         left: None,
         event: Event::Stopped,
         peer_tracker_addr: peer_tracker_addr
            .unwrap_or(SocketAddr::from_str("0.0.0.0:6881").unwrap()),
      }
   }
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

            Ok(tracker.get_peers().await.unwrap())
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

            Ok(tracker.get_peers().await.unwrap())
         }
         Tracker::Websocket(uri) => {
            let port: u16 = random_range(1024..65535);
            let mut tracker = WssTracker::new(
               uri.clone(),
               info_hash,
               Some(SocketAddr::from(([0, 0, 0, 0], port))),
            )
            .await;
            Ok(tracker.get_peers().await.unwrap())
         }
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
