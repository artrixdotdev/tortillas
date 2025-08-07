use std::{
   fmt,
   net::SocketAddr,
   pin::Pin,
   sync::{
      Arc,
      atomic::{AtomicUsize, Ordering},
   },
};

use anyhow::Result;
use async_trait::async_trait;
use atomic_time::{AtomicInstant, AtomicOptionInstant};
use futures::Stream;
use http::HttpTracker;
use num_enum::TryFromPrimitive;
use serde::{
   Deserialize,
   de::{self, Visitor},
};
use serde_repr::{Deserialize_repr, Serialize_repr};
use tokio::{
   sync::{broadcast, mpsc},
   time::Instant,
};
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
///
/// let server = UdpServer::new();
/// let tracker: Box<dyn TrackerInstance> = tracker.to_instance(info_hash, peer_id, port, Some(server));
///
/// let (rx, tx) = tracker.configure();
///
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

/// Event. See <https://www.bittorrent.org/beps/bep_0003.html> @ trackers
/// Enum for UDP Tracker Protocol Events parameter. See this resource for more information: <https://xbtt.sourceforge.net/udp_tracker_protocol.html>
#[derive(
   Debug,
   Default,
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
   #[default]
   Started = 1,
   Completed = 2,
   Stopped = 3,
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

/// Broadcast sender and receiver for statistical information about trackers.
#[derive(Debug)]
struct StatsHook(
   broadcast::Sender<TrackerStats>,
   broadcast::Receiver<TrackerStats>,
);

impl Default for StatsHook {
   fn default() -> Self {
      let (tx, rx) = broadcast::channel(10);
      Self(tx, rx)
   }
}

/// We have to manually implement Clone because we can't clone the receiver
impl Clone for StatsHook {
   fn clone(&self) -> Self {
      Self(self.0.clone(), self.1.resubscribe())
   }
}

impl StatsHook {
   pub fn tx(&self) -> &broadcast::Sender<TrackerStats> {
      &self.0
   }

   pub fn rx(&self) -> &broadcast::Receiver<TrackerStats> {
      &self.1
   }
}

/// Trait for HTTP and UDP trackers.
#[async_trait]
pub trait TrackerInstance {
   /// Connects to the tracker. If this tracker is an HTTP tracker, no actual
   /// connection is made. If this tracker is a UDP tracker, a connection is
   /// established with the peer.
   async fn configure(
      &self,
   ) -> Result<(
      mpsc::Sender<TrackerUpdate>,
      broadcast::Receiver<TrackerStats>,
   )>;
   /// Returns a stream that appends every new group of peers that we receive
   /// from a tracker.
   async fn announce_stream(&self) -> Pin<Box<dyn Stream<Item = Peer> + Send>>;
}

/// Tracker statistics to be returned from
/// [announce_stream](TrackerInstance::announce_stream).
///
/// All usages of AtomicOptionInstant or AtomicInstant are a bit hacky, due to
/// the fact that they only support Instant from std, not tokio. See any of the
/// getter/setter methods as an example.
#[derive(Clone)]
pub struct TrackerStats {
   announce_attempts: Arc<AtomicUsize>,
   announce_successes: Arc<AtomicUsize>,
   total_peers_received: Arc<AtomicUsize>,
   bytes_sent: Arc<AtomicUsize>,
   bytes_received: Arc<AtomicUsize>,
   last_interaction: Arc<AtomicOptionInstant>,
   session_start: Arc<AtomicInstant>,
}

impl fmt::Debug for TrackerStats {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.debug_struct("TrackerStats")
         .field("announce_attempts", &self.get_announce_attempts())
         .field("announce_successes", &self.get_announce_successes())
         .field("total_peers_received", &self.get_total_peers_received())
         .field("bytes_sent", &self.get_bytes_sent())
         .field("bytes_received", &self.get_bytes_received())
         .field("last_interaction", &self.get_last_interaction())
         .field("session_start", &self.get_session_start())
         .finish()
   }
}

impl fmt::Display for TrackerStats {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      let succes_rate = self.get_announce_successes() as f64 / self.get_announce_attempts() as f64;
      write!(
         f,
         "Stats (success rate: {:.2}%, peers received: {:?})",
         succes_rate * 100.0,
         self.get_total_peers_received()
      )
   }
}

impl Default for TrackerStats {
   fn default() -> Self {
      Self {
         announce_attempts: Arc::new(AtomicUsize::new(0)),
         announce_successes: Arc::new(AtomicUsize::new(0)),
         total_peers_received: Arc::new(AtomicUsize::new(0)),
         bytes_sent: Arc::new(AtomicUsize::new(0)),
         bytes_received: Arc::new(AtomicUsize::new(0)),
         last_interaction: Arc::new(AtomicOptionInstant::new(Some(Instant::now().into_std()))),
         session_start: Arc::new(AtomicInstant::new(Instant::now().into_std())),
      }
   }
}

impl TrackerStats {
   pub fn get_announce_attempts(&self) -> usize {
      self.announce_attempts.load(Ordering::Acquire)
   }

   pub fn increment_announce_attempts(&self) {
      let cur = self.get_announce_attempts();
      self.announce_attempts.store(cur + 1, Ordering::Release)
   }

   pub fn get_announce_successes(&self) -> usize {
      self.announce_successes.load(Ordering::Acquire)
   }

   pub fn increment_announce_successes(&self) {
      let cur = self.get_announce_successes();
      self.announce_successes.store(cur + 1, Ordering::Release)
   }

   pub fn get_total_peers_received(&self) -> usize {
      self.total_peers_received.load(Ordering::Acquire)
   }

   pub fn increment_total_peers_received(&self, value: usize) {
      let cur = self.get_total_peers_received();
      self
         .total_peers_received
         .store(cur + value, Ordering::Release)
   }

   pub fn get_bytes_sent(&self) -> usize {
      self.bytes_sent.load(Ordering::Acquire)
   }

   pub fn increment_bytes_sent(&self, value: usize) {
      let cur = self.get_bytes_sent();
      self.bytes_sent.store(cur + value, Ordering::Release)
   }

   pub fn get_bytes_received(&self) -> usize {
      self.bytes_received.load(Ordering::Acquire)
   }

   pub fn increment_bytes_received(&self, value: usize) {
      let cur = self.get_bytes_received();
      self.bytes_received.store(cur + value, Ordering::Release)
   }

   pub fn get_last_interaction(&self) -> Option<Instant> {
      Some(
         self
            .last_interaction
            .load(Ordering::Acquire)
            .unwrap()
            .into(),
      )
   }

   pub fn set_last_interaction(&self) {
      self
         .last_interaction
         .store(Some(Instant::now().into_std()), Ordering::Release)
   }

   pub fn get_session_start(&self) -> Instant {
      self.session_start.load(Ordering::Acquire).into()
   }

   pub fn set_session_start(&self) {
      self
         .session_start
         .store(Instant::now().into_std(), Ordering::Release)
   }
}

impl Tracker {
   /// Creates a new tracker instance based on the tracker type.
   ///
   /// Forced to use some really hacky code to coerce the tracker into a
   /// [TrackerInstance].
   pub async fn to_instance(
      &self, info_hash: InfoHash, peer_id: PeerId, port: u16, server: UdpServer,
   ) -> Box<dyn TrackerInstance> {
      // We can't use a traditional "match" statement here because match statements
      // don't allow arms with varying types
      let mut instance: Option<Box<dyn TrackerInstance>> = None;
      let socket_addr = SocketAddr::from(([0, 0, 0, 0], port));
      if let Self::Http(uri) = &self {
         let tracker = HttpTracker::new(uri.clone(), info_hash, Some(peer_id), Some(socket_addr));
         instance = Some(Box::new(tracker));
      } else if let Self::Udp(uri) = &self {
         let tracker =
            UdpTracker::new(uri.clone(), Some(server), info_hash, (peer_id, socket_addr))
               .await
               .unwrap();
         instance = Some(Box::new(tracker));
      };
      instance.unwrap()
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
