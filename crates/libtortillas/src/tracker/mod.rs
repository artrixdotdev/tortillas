use std::{fmt, net::SocketAddr, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use http::HttpTracker;
use num_enum::TryFromPrimitive;
use rand::random_range;
use serde::{
   Deserialize,
   de::{self, Visitor},
};
use serde_repr::{Deserialize_repr, Serialize_repr};
use tokio::{net::UdpSocket, sync::mpsc, time::Instant};
use udp::UdpTracker;

use crate::{
   hashes::InfoHash,
   peer::{Peer, PeerId},
   tracker::udp::UdpServer,
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
/// HTTP trackers: <https://www.bittorrent.org/beps/bep_0003.html>
/// UDP trackers: <https://www.bittorrent.org/beps/bep_0015.html>
///
/// Example tracker: <udp://tracker.opentrackr.org:1337/announce>
///
/// # Example usage:
///
/// ```
/// let tracker = Tracker::Http("udp://tracker.opentrackr.org:1337/announce");
/// let udp_manager = UdpManager::new();
/// let udp_socket = UdpSocket::bind("")
/// let (tx, rx) = tracker.connect_udp(info_hash, peer_id, udp_manager, udp_socket).await; // irrelevant for HTTP trackers
/// let peers: Stream<Peer> = tracker.announce_stream();
/// tx.send({ uploaded: 20 });
/// ```
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

/// Tracker statistics to be returned from
/// [announce_stream](TrackerInstance::announce_stream).
#[derive(Clone, Debug)]
pub struct TrackerStats {
   connect_attempts: usize,
   connect_successes: usize,
   announce_attempts: usize,
   announce_successes: usize,
   total_peers_received: usize,
   bytes_sent: usize,
   bytes_received: usize,
   last_successful_announce: Option<Instant>,
   session_start: Instant,
}

impl Default for TrackerStats {
   fn default() -> Self {
      Self {
         connect_attempts: 0,
         connect_successes: 0,
         announce_attempts: 0,
         announce_successes: 0,
         total_peers_received: 0,
         bytes_sent: 0,
         bytes_received: 0,
         last_successful_announce: None,
         session_start: Instant::now(),
      }
   }
}

/// Event. See <https://www.bittorrent.org/beps/bep_0003.html> @ trackers
/// Enum for UDP Tracker Protocol Events parameter. See this resource for more information: <https://xbtt.sourceforge.net/udp_tracker_protocol.html>
#[derive(
   Debug,
   Serialize_repr,
   Deserialize_repr,
   Clone,
   Copy,
   PartialEq,
   Eq,
   TryFromPrimitive
)]
#[repr(u32)]
pub enum Event {
   Empty = 0,
   Started = 1,
   Completed = 2,
   Stopped = 3,
}

/// The format/data for a request made to either a UDP or HTTP tracker.
#[derive(Clone, Copy, Debug)]
#[allow(unused)]
pub struct TrackerRequest {
   /// The port that our TCP or uTP peer handler is listening on
   ///
   /// The port for other peers to connect to us
   port: u16,
   /// Bytes uploaded. Due to BitTorrent's godawful documentation, I am not
   /// aware if this is *actually* bytes. However, according to [BitTorrent's wiki.theory.org](https://wiki.theory.org/BitTorrentSpecification#Tracker_HTTP.2FHTTPS_Protocol), the general consensus is that the unit of this metric is bytes.
   uploaded: usize,
   /// Bytes uploaded. Due to BitTorrent's godawful documentation, I am not
   /// aware if this is *actually* bytes. However, according to [BitTorrent's wiki.theory.org](https://wiki.theory.org/BitTorrentSpecification#Tracker_HTTP.2FHTTPS_Protocol), the general consensus is that the unit of this metric is bytes.
   downloaded: usize,
   /// The number of bytes this client still has to download.
   left: Option<usize>,
   /// See documentation for [Event].
   event: Event,
   /// If we want peers in a compact format or not. Only applicable to HTTP
   /// trackers.
   compact: Option<bool>,
}

/// An enum for updating data inside a tracker with the [mpsc
/// Sender](tokio::sync::mpsc::Sender) returned from
/// [announce_stream](TrackerInstance::announce_stream).
pub enum TrackerUpdate {
   Uploaded(usize),
   Downloaded(usize),
   Left(usize),
   Event(Event),
}

/// Trait for HTTP and UDP trackers.
#[async_trait]
pub trait TrackerInstance {
   /// Connects to the tracker. If this tracker is an HTTP tracker, no actual
   /// connection is made. If this tracker is a UDP tracker, a connection is
   /// established with the peer.
   async fn connect(
      info_hash: InfoHash, peer_id: PeerId, port: u16,
   ) -> (mpsc::Sender<TrackerUpdate>, mpsc::Receiver<TrackerStats>);

   async fn connect_udp(
      info_hash: InfoHash, peer_id: PeerId, port: u16, udp_socket: Option<Arc<UdpSocket>>,
      manager: Arc<UdpServer>,
   ) -> (mpsc::Sender<TrackerUpdate>, mpsc::Receiver<TrackerStats>);

   /// Returns a stream that appends every new group of peers that we receive
   /// from a tracker.
   async fn announce_stream() -> impl Stream<Item = Peer>;
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
               (peer_id.unwrap(), SocketAddr::from(([0, 0, 0, 0], port))),
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
               UdpTracker::new(uri.clone(), None, info_hash, (peer_id, peer_addr.unwrap()))
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
