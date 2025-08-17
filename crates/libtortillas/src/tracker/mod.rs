use std::{
   fmt,
   net::SocketAddr,
   sync::{
      Arc,
      atomic::{AtomicUsize, Ordering},
   },
   time::Duration,
};

use anyhow::Result;
use async_trait::async_trait;
use atomic_time::{AtomicInstant, AtomicOptionInstant};
use http::HttpTracker;
use kameo::{
   Actor,
   actor::ActorRef,
   mailbox::Signal,
   prelude::{Context, Message},
};
use num_enum::TryFromPrimitive;
use serde::{
   Deserialize,
   de::{self, Visitor},
};
use serde_repr::{Deserialize_repr, Serialize_repr};
use tokio::time::{Instant, Interval, interval};
use tracing::error;
use udp::UdpTracker;

use crate::{
   hashes::InfoHash,
   peer::{Peer, PeerId},
   torrent::{Torrent, TorrentMessage, TorrentRequest, TorrentResponse},
   tracker::udp::UdpServer,
};
pub mod http;
pub mod udp;

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
/// let tracker = tracker.to_instance(info_hash, peer_id, port, Some(server));
///
/// tracker.initialize().await;
///
/// let peers: Vec<Peer> = tracker.announce();
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
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
   /// Creates a new tracker instance based on the tracker type.
   #[deprecated(note = "Use `to_instance` instead")]
   pub async fn to_base(
      &self, info_hash: InfoHash, peer_id: PeerId, port: u16, server: UdpServer,
   ) -> Result<Box<dyn TrackerBase>> {
      let socket_addr = SocketAddr::from(([0, 0, 0, 0], port));
      match self {
         Self::Http(uri) => {
            let tracker =
               HttpTracker::new(uri.clone(), info_hash, Some(peer_id), Some(socket_addr));
            Ok(Box::new(tracker))
         }
         Self::Udp(uri) => {
            let tracker =
               UdpTracker::new(uri.clone(), Some(server), info_hash, (peer_id, socket_addr))
                  .await?;
            Ok(Box::new(tracker))
         }
         Self::Websocket(_) => {
            unimplemented!("Websocket trackers not yet supported")
         }
      }
   }

   /// Creates an instance of the [`Tracker`](Tracker) struct from an info hash,
   /// a peer ID, a port,
   pub async fn to_instance(
      &self, info_hash: InfoHash, peer_id: PeerId, port: u16, server: UdpServer,
   ) -> Result<TrackerInstance> {
      let socket_addr = SocketAddr::from(([0, 0, 0, 0], port));
      match self {
         Self::Http(uri) => {
            let tracker =
               HttpTracker::new(uri.clone(), info_hash, Some(peer_id), Some(socket_addr));
            Ok(TrackerInstance::Http(tracker))
         }
         Self::Udp(uri) => {
            let tracker =
               UdpTracker::new(uri.clone(), Some(server), info_hash, (peer_id, socket_addr))
                  .await?;
            Ok(TrackerInstance::Udp(tracker))
         }
         Self::Websocket(_) => {
            unimplemented!("Websocket trackers not yet supported")
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

/// Trait for HTTP and UDP trackers.
#[async_trait]
pub trait TrackerBase: Send + Sync {
   /// Initializes the tracker instance.
   async fn initialize(&self) -> Result<()>;

   /// Announces to the tracker and returns a list of peers.
   async fn announce(&self) -> Result<Vec<Peer>>;

   /// Updates tracker stats based on current state.
   async fn update(&self, update: TrackerUpdate) -> Result<()>;

   /// Gets the current tracker stats.
   fn stats(&self) -> TrackerStats;

   /// Gets the announce interval.
   fn interval(&self) -> usize;
}

/// Enum for the different tracker variants that implement [TrackerBase] rather
/// than the URIs like [Tracker]
#[derive(Clone)]
pub enum TrackerInstance {
   Udp(UdpTracker),
   Http(HttpTracker),
}

#[async_trait]
impl TrackerBase for TrackerInstance {
   async fn initialize(&self) -> Result<()> {
      match self {
         TrackerInstance::Udp(tracker) => tracker.initialize().await,
         TrackerInstance::Http(tracker) => tracker.initialize().await,
      }
   }

   async fn announce(&self) -> Result<Vec<Peer>> {
      match self {
         TrackerInstance::Udp(tracker) => Ok(tracker.announce().await.unwrap()),
         TrackerInstance::Http(tracker) => tracker.announce().await,
      }
   }

   async fn update(&self, update: TrackerUpdate) -> Result<()> {
      match self {
         TrackerInstance::Udp(tracker) => tracker.update(update).await,
         TrackerInstance::Http(tracker) => tracker.update(update).await,
      }
   }

   fn interval(&self) -> usize {
      match self {
         TrackerInstance::Udp(tracker) => tracker.interval() as usize,
         TrackerInstance::Http(tracker) => tracker.interval(),
      }
   }
   fn stats(&self) -> TrackerStats {
      match self {
         TrackerInstance::Udp(tracker) => tracker.stats(),
         TrackerInstance::Http(tracker) => tracker.stats(),
      }
   }
}

pub(crate) struct TrackerActor {
   tracker: TrackerInstance,
   supervisor: ActorRef<Torrent>,
   /// A custom struct provided by tokio that allows a `tick` function that will
   /// wait until the next `duration` passes
   ///
   /// See <https://docs.rs/tokio/latest/tokio/time/fn.interval.html>
   interval: Interval,
}

impl Actor for TrackerActor {
   type Args = (Tracker, PeerId, UdpServer, SocketAddr, ActorRef<Torrent>);
   type Error = anyhow::Error;

   /// Unlike the [`PeerActor`](crate::protocol::PeerActor), there are no
   /// prerequisites to calling this function. In other words, the tracker is
   /// not expected to be connected when this function is called.
   async fn on_start(state: Self::Args, _: ActorRef<Self>) -> Result<Self, Self::Error> {
      let (tracker, peer_id, server, socket_addr, supervisor) = state;

      let torrent_response = supervisor.ask(TorrentRequest::InfoHash).await?;
      let info_hash = match torrent_response {
         TorrentResponse::InfoHash(info_hash) => info_hash,
         _ => unreachable!(),
      };

      let tracker = match tracker {
         Tracker::Udp(uri) => {
            let udp_tracker =
               UdpTracker::new(uri, Some(server), info_hash, (peer_id, socket_addr)).await?;
            udp_tracker.initialize().await?;
            TrackerInstance::Udp(udp_tracker)
         }
         Tracker::Http(uri) => {
            let http_tracker = HttpTracker::new(uri, info_hash, Some(peer_id), Some(socket_addr));
            http_tracker.initialize().await?;
            TrackerInstance::Http(http_tracker)
         }
         _ => unimplemented!(),
      };

      Ok(Self {
         tracker,
         supervisor,
         interval: interval(Duration::from_secs(30)),
      })
   }

   async fn next(
      &mut self, _: kameo::prelude::WeakActorRef<Self>,
      mailbox_rx: &mut kameo::prelude::MailboxReceiver<Self>,
   ) -> Option<Signal<Self>> {
      tokio::select! {
         signal = mailbox_rx.recv() => signal,
         // Waits for the next interval to tick
         _ = self.interval.tick() => {
            if let Ok(peers) = self.tracker.announce().await {
               let _ = self.supervisor.tell(TorrentMessage::Announce(peers)).await;
            }
            let duration = Duration::from_secs(self.tracker.interval() as u64);
            self.interval = interval(duration);

            None
         }
      }
   }
}

/// A message from an outside source.
#[allow(dead_code)]
pub(crate) enum TrackerMessage {
   /// Forces the tracker to make an announce request. By default, announce
   /// requests are made on an interval.
   Announce,
   /// Gets the statistics of the tracker via
   /// [`tracker.stats()`](TrackerEnum::stats)
   GetStats,
}

impl Message<TrackerMessage> for TrackerActor {
   type Reply = Option<TrackerStats>;

   async fn handle(
      &mut self, msg: TrackerMessage, _ctx: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match msg {
         TrackerMessage::Announce => {
            if let Ok(peers) = self.tracker.announce().await
               && let Err(e) = self.supervisor.tell(TorrentMessage::Announce(peers)).await
            {
               error!("Failed to send announce to supervisor: {}", e);
            }
            None
         }
         TrackerMessage::GetStats => Some(self.tracker.stats()),
      }
   }
}

/// Updates the tracker's announce fields
pub enum TrackerUpdate {
   Uploaded(usize),
   Downloaded(usize),
   Left(usize),
   Event(Event),
}

impl Message<TrackerUpdate> for TrackerActor {
   type Reply = ();

   async fn handle(
      &mut self, msg: TrackerUpdate, _ctx: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      let _ = self.tracker.update(msg).await;
   }
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
      let success_rate = if self.get_announce_attempts() > 0 {
         self.get_announce_successes() as f64 / self.get_announce_attempts() as f64
      } else {
         0.0
      };
      write!(
         f,
         "Stats (success rate: {:.2}%, peers received: {:?})",
         success_rate * 100.0,
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

/// Serde visitor used for deserializing the URI of a [tracker](Tracker).
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
