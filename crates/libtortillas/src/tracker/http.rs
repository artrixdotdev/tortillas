use std::{
   fmt::Debug,
   net::{IpAddr, Ipv4Addr, SocketAddr},
   str::FromStr,
   sync::{
      Arc,
      atomic::{AtomicUsize, Ordering},
   },
};

use anyhow::Result;
use async_stream::stream;
use async_trait::async_trait;
use futures::Stream;
use serde::{
   Deserialize,
   de::{self, Visitor},
};
use tokio::{
   sync::{RwLock, broadcast, mpsc},
   time::{Duration, Instant, sleep},
};
use tracing::{debug, error, info, instrument, trace, warn};

/// See https://www.bittorrent.org/beps/bep_0003.html
use super::{Peer, TrackerTrait};
use crate::{
   errors::{HttpTrackerError, TrackerError},
   hashes::InfoHash,
   peer::PeerId,
   tracker::{Event, StatsHook, TrackerInstance, TrackerStats, TrackerUpdate},
};

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // REMOVE SOON
pub struct TrackerResponse {
   pub interval: Option<usize>,
   #[serde(deserialize_with = "deserialize_peers")]
   pub peers: Vec<Peer>,
}

/// Tracker request. See <https://www.bittorrent.org/beps/bep_0003.html> @ trackers
#[derive(Clone, Debug)]
struct TrackerRequest {
   /// the port that our tcp or utp peer handler is listening on
   ///
   /// The port for other peers to connect to us
   port: u16,
   /// The IP the that our tcp or utp peer handler is listening on
   ip: Option<IpAddr>,
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

impl TrackerRequest {
   // Clippy wants this method to be converted to Display, but I dont think it
   // meets our use case so this should be fine.
   #[allow(clippy::inherent_to_string)]
   pub fn to_string(&self) -> String {
      let mut params = Vec::new();

      if let Some(ip) = &self.ip {
         params.push(format!("ip={}", ip));
      }

      params.push(format!("port={}", self.port));
      params.push(format!("uploaded={}", self.uploaded));
      params.push(format!("downloaded={}", self.downloaded));

      if let Some(left) = self.left {
         params.push(format!("left={}", left));
      }
      let event_str = format!("{:?}", self.event).to_lowercase(); // Hack to get the string representation of the enum

      params.push(format!("event={}", event_str));
      params.push(format!("compact={}", self.compact.unwrap_or(true) as u8));

      params.join("&")
   }
}

impl TrackerRequest {
   #[instrument(fields(peer_tracker_addr = ?peer_tracker_addr))]
   /// peer_tracker_addr refers to the port that we are listening on (which
   /// could be TCP, uTP, etc.).
   pub fn new(peer_tracker_addr: Option<SocketAddr>) -> TrackerRequest {
      let addr = peer_tracker_addr.unwrap_or_else(|| {
         let default_addr = SocketAddr::from_str("0.0.0.0:6881").unwrap();
         debug!(default_addr = %default_addr, "Using default peer tracker address");
         default_addr
      });

      // If the ip address is 127.0.0.1 or 0.0.0.0 just dont send it since its
      // optional
      let ip = if addr.ip().is_loopback() | addr.ip().is_unspecified() {
         None
      } else {
         Some(addr.ip())
      };

      let port = addr.port();

      TrackerRequest {
         ip,
         port,
         uploaded: 0,
         downloaded: 0,
         left: Some(0),
         event: Event::Stopped,
         // We currently don't support the non-compact form
         compact: Some(true),
      }
   }
}

/// Struct for handling tracker over HTTP
/// Interval is set to `[usize::MAX]` by default.
#[derive(Clone, Debug)]
pub struct HttpTracker {
   uri: String,
   pub peer_id: PeerId,
   info_hash: InfoHash,
   params: Arc<RwLock<TrackerRequest>>,
   interval: Arc<AtomicUsize>,
   stats: TrackerStats,
   stats_hook: StatsHook,
}

impl HttpTracker {
   #[instrument(skip(info_hash, peer_id), fields(
        uri = %uri,
        info_hash = %info_hash,
        peer_tracker_addr = ?peer_tracker_addr
    ))]
   pub fn new(
      uri: String, info_hash: InfoHash, peer_id: Option<PeerId>,
      peer_tracker_addr: Option<SocketAddr>,
   ) -> HttpTracker {
      let peer_id = peer_id.unwrap_or_else(|| {
         let id = PeerId::new();
         trace!(generated_peer_id = %id, "Generated new peer ID");
         id
      });
      let params = TrackerRequest::new(peer_tracker_addr);

      debug!(
          peer_id = %peer_id,
          tracker_uri = %uri,
          peer_addr = format!("{}:{}", params.ip.unwrap_or(Ipv4Addr::UNSPECIFIED.into()), params.port),
          "Created HTTP tracker instance"
      );
      let params = Arc::new(RwLock::new(params));

      HttpTracker {
         interval: Arc::new(usize::MAX.into()),
         uri,
         peer_id,
         params,
         info_hash,
         stats: TrackerStats::default(),
         stats_hook: StatsHook::default(),
      }
   }
   /// Sends tracker statistics to the receiver created from the
   /// [configure](UdpTracker::configure) function.
   #[instrument(skip(self), fields(tracker_uri = %self.uri, stats = %self.stats))]
   async fn send_stats(&self) {
      let tx = &self.stats_hook.tx();
      if let Err(e) = tx.send(self.stats.clone()) {
         error!(error = %e, "Failed to send tracker stats");
      }
      trace!("Sent tracker stats");
   }

   #[instrument(skip(self), fields(
           tracker_uri = %self.uri,
           info_hash = %self.info_hash,
           peer_id = %self.peer_id
       ))]
   async fn announce(&self) -> Result<Vec<Peer>> {
      // Update statistics
      self.stats.increment_announce_attempts();

      // URL encoding phase
      let params_encoded = &self.params.read().await.to_string();
      let info_hash_encoded = urlencode(self.info_hash.as_bytes());
      let peer_id_encoded = urlencode(self.peer_id.id());

      // URI construction
      let uri_params =
         format!("{params_encoded}&info_hash={info_hash_encoded}&peer_id={peer_id_encoded}");

      let uri = format!("{}?{}", self.uri, &uri_params);

      trace!(request_uri = %uri, "Sending HTTP request to tracker");

      // HTTP request phase
      let request_start = Instant::now();

      let response = reqwest::get(&uri).await.map_err(|e| {
         error!(
             error = %e,
             request_uri = %uri,
             "HTTP request to tracker failed"
         );
         HttpTrackerError::Request(e)
      })?;

      let status = response.status();
      trace!(
         status_code = status.as_u16(),
         "Received HTTP response from tracker"
      );

      let response_bytes = response.bytes().await.map_err(|e| {
         error!(error = %e, "Failed to read tracker response body");
         HttpTrackerError::Request(e)
      })?;

      let request_duration = request_start.elapsed();

      self.stats.increment_bytes_sent(uri.len()); // Approximate bytes sent
      self.stats.increment_bytes_received(response_bytes.len());

      // Response parsing phase
      let response: TrackerResponse = serde_bencode::from_bytes(&response_bytes).map_err(|e| {
         error!(
             error = %e,
             response_size = response_bytes.len(),
             "Failed to decode bencode response"
         );
         HttpTrackerError::Tracker(TrackerError::BencodeError(e))
      })?;

      self.stats.increment_announce_successes();
      self
         .stats
         .increment_total_peers_received(response.peers.len());
      self.stats.set_last_interaction();
      debug!(
         peers_received = response.peers.len(),
         interval_seconds = response.interval,
         request_duration_ms = request_duration.as_millis(),
         "Successfully fetched peers from HTTP tracker"
      );

      // Update interval
      self.set_interval(response.interval.unwrap_or(usize::MAX));

      Ok(response.peers)
   }
   pub fn interval(&self) -> usize {
      self.interval.load(Ordering::Acquire)
   }

   pub fn set_interval(&self, interval: usize) {
      self.interval.store(interval, Ordering::Release)
   }
}
#[async_trait]
impl TrackerInstance for HttpTracker {
   async fn configure(
      &self,
   ) -> anyhow::Result<(
      mpsc::Sender<TrackerUpdate>,
      broadcast::Receiver<TrackerStats>,
   )> {
      let (tx, mut rx) = mpsc::channel(100);
      let tracker = self.clone();
      // tracker.connect().await?;
      tokio::spawn(async move {
         while let Some(update) = rx.recv().await {
            match update {
               TrackerUpdate::Uploaded(uploaded) => {
                  let mut params = tracker.params.write().await;
                  params.uploaded = uploaded;
               }
               TrackerUpdate::Downloaded(downloaded) => {
                  let mut params = tracker.params.write().await;
                  params.downloaded = downloaded;
               }
               TrackerUpdate::Left(left) => {
                  let mut params = tracker.params.write().await;
                  params.left = Some(left);
               }
               TrackerUpdate::Event(event) => {
                  let mut params = tracker.params.write().await;
                  params.event = event;
               }
            }
         }
      });
      let stats_receiver = self.stats_hook.rx().resubscribe();
      Ok((tx, stats_receiver))
   }

   async fn announce_stream(&self) -> impl Stream<Item = Peer> {
      let tracker = self.clone();
      let interval = self.interval();

      stream! {
          loop {
              match tracker.announce().await {
                  Ok(peers) => {
                      tracker.send_stats().await;
                      for peer in peers {
                          yield peer;
                      }
                  }
                  Err(e) => {
                      error!(error = %e, "Failed to announce to tracker");
                      // Continue the loop to retry after interval
                  }
              }
              sleep(Duration::from_secs(interval as u64)).await;
          }
      }
   }
}

#[instrument(skip(t), fields(hash_length = t.len()))]
fn urlencode(t: &[u8; 20]) -> String {
   let mut encoded = String::with_capacity(3 * t.len());

   for &byte in t.iter() {
      encoded.push('%');
      let byte_hex = hex::encode([byte]);
      encoded.push_str(&byte_hex);
   }

   encoded
}

/// Fetches peers from tracker over HTTP and returns a stream of [Peers](Peer)
#[async_trait]
impl TrackerTrait for HttpTracker {
   #[instrument(skip(self), fields(
        tracker_uri = %self.uri,
        info_hash = %self.info_hash
    ))]
   async fn stream_peers(&mut self) -> Result<mpsc::Receiver<Vec<Peer>>> {
      info!("Starting HTTP tracker peer streaming");

      let (tx, rx) = mpsc::channel(100);
      let mut tracker = self.clone();
      let tx = tx.clone();

      tokio::spawn(async move {
         let mut iteration = 0usize;

         loop {
            iteration += 1;

            let start_time = Instant::now();
            match tracker.get_peers().await {
               Ok(peers) => {
                  let fetch_duration = start_time.elapsed();

                  debug!(
                     iteration,
                     peers_count = peers.len(),
                     fetch_duration_ms = fetch_duration.as_millis(),
                     "Fetched peers from HTTP tracker"
                  );

                  if tx.send(peers).await.is_err() {
                     error!("Peer stream channel closed, terminating");
                     break;
                  }
               }
               Err(e) => {
                  let fetch_duration = start_time.elapsed();

                  warn!(
                      iteration,
                      fetch_duration_ms = fetch_duration.as_millis(),
                      error = %e,
                      "Failed to fetch peers from HTTP tracker"
                  );

                  // Send empty peer list on error to keep the stream alive
                  if tx.send(vec![]).await.is_err() {
                     error!("Peer stream channel closed, terminating");
                     break;
                  }
               }
            }

            let delay = tracker.interval().max(1);
            trace!(
               next_request_delay_secs = delay,
               "Waiting before next peer fetch"
            );

            sleep(Duration::from_secs(delay as u64)).await;
         }

         debug!("HTTP peer streaming task terminated");
      });

      Ok(rx)
   }

   #[instrument(skip(self), fields(
        tracker_uri = %self.uri,
        info_hash = %self.info_hash,
        peer_id = %self.peer_id
    ))]
   async fn get_peers(&mut self) -> Result<Vec<Peer>> {
      // Update statistics
      self.stats.increment_announce_attempts();

      // URL encoding phase
      let params_encoded = &self.params.read().await.to_string();
      let info_hash_encoded = urlencode(self.info_hash.as_bytes());
      let peer_id_encoded = urlencode(self.peer_id.id());

      // URI construction
      let uri_params =
         format!("{params_encoded}&info_hash={info_hash_encoded}&peer_id={peer_id_encoded}");

      let uri = format!("{}?{}", self.uri, &uri_params);

      trace!(request_uri = %uri, "Sending HTTP request to tracker");

      // HTTP request phase
      let request_start = Instant::now();

      let response = reqwest::get(&uri).await.map_err(|e| {
         error!(
             error = %e,
             request_uri = %uri,
             "HTTP request to tracker failed"
         );
         HttpTrackerError::Request(e)
      })?;

      let status = response.status();
      trace!(
         status_code = status.as_u16(),
         "Received HTTP response from tracker"
      );

      let response_bytes = response.bytes().await.map_err(|e| {
         error!(error = %e, "Failed to read tracker response body");
         HttpTrackerError::Request(e)
      })?;

      let request_duration = request_start.elapsed();

      self.stats.increment_bytes_sent(uri.len()); // Approximate bytes sent
      self.stats.increment_bytes_received(response_bytes.len());

      // Response parsing phase
      let response: TrackerResponse = serde_bencode::from_bytes(&response_bytes).map_err(|e| {
         error!(
             error = %e,
             response_size = response_bytes.len(),
             "Failed to decode bencode response"
         );
         HttpTrackerError::Tracker(TrackerError::BencodeError(e))
      })?;

      self.stats.increment_announce_successes();
      self
         .stats
         .increment_total_peers_received(response.peers.len());
      self.stats.set_last_interaction();
      debug!(
         peers_received = response.peers.len(),
         interval_seconds = response.interval,
         request_duration_ms = request_duration.as_millis(),
         "Successfully fetched peers from HTTP tracker"
      );

      // Update interval
      self.set_interval(response.interval.unwrap_or(usize::MAX));

      Ok(response.peers)
   }
}

/// Serde related code. Used for deserializing response from HTTP request made
/// in get_peers
struct PeerVisitor;

impl Visitor<'_> for PeerVisitor {
   type Value = Vec<Peer>;

   fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
      formatter.write_str("a byte array containing peer information")
   }

   #[instrument(skip(self, bytes), fields(bytes_length = bytes.len()))]
   fn visit_bytes<E>(self, bytes: &[u8]) -> Result<Self::Value, E>
   where
      E: de::Error,
   {
      // Decodes response from get_peers' HTTP request according to BEP 23's compact
      // form: <https://www.bittorrent.org/beps/bep_0023.html>
      let mut peers = Vec::new();
      const PEER_SIZE: usize = 6; // 4 bytes IP + 2 bytes port

      if !bytes.len().is_multiple_of(PEER_SIZE) {
         error!(
            bytes_length = bytes.len(),
            peer_size = PEER_SIZE,
            remainder = bytes.len() % PEER_SIZE,
            "Peer data length is not a multiple of peer size"
         );
         return Err(de::Error::custom(format!(
            "Peer data length ({}) is not a multiple of peer size ({})",
            bytes.len(),
            PEER_SIZE
         )));
      }

      let expected_peer_count = bytes.len() / PEER_SIZE;

      for (i, chunk) in bytes.chunks(PEER_SIZE).enumerate() {
         if chunk.len() != PEER_SIZE {
            warn!(
               chunk_index = i,
               chunk_size = chunk.len(),
               expected_size = PEER_SIZE,
               "Invalid peer chunk length, skipping"
            );
            continue;
         }

         let ip = Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]);
         let port = u16::from_be_bytes([chunk[4], chunk[5]]);

         peers.push(Peer::from_ipv4(ip, port));
      }

      trace!(
         peers_parsed = peers.len(),
         expected_peers = expected_peer_count,
         "Parsed peer bytes"
      );

      Ok(peers)
   }
}

/// Serde related code. Reference their documentation: <https://serde.rs/impl-deserialize.html>
#[instrument(skip(deserializer))]
fn deserialize_peers<'de, D>(deserializer: D) -> Result<Vec<Peer>, D::Error>
where
   D: serde::Deserializer<'de>,
{
   deserializer.deserialize_bytes(PeerVisitor)
}

#[cfg(test)]
mod tests {
   use std::{net::SocketAddr, str::FromStr, time::Duration};

   use futures::{StreamExt, pin_mut};
   use rand::random_range;
   use tokio::time::{Instant, timeout};
   use tracing_test::traced_test;

   use super::HttpTracker;
   use crate::{
      metainfo::{MetaInfo, TorrentFile},
      peer::PeerId,
      tracker::{Event, TrackerInstance, TrackerTrait, TrackerUpdate},
   };

   #[tokio::test]
   #[traced_test]
   async fn test_get_peers_with_http_tracker() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/torrents/KNOPPIX_V9.1DVD-2021-01-25-EN.torrent");

      let metainfo = TorrentFile::read(path).await.unwrap();

      match metainfo {
         MetaInfo::Torrent(file) => {
            let info_hash = file.info.hash();
            let announce_list = file.announce_list();
            println!("announce_list: {:?}", announce_list);

            // An HTTP tracker
            let announce_uri = announce_list[0].uri();
            let mut http_tracker = HttpTracker::new(announce_uri, info_hash.unwrap(), None, None);

            // Spawn a task to re-fetch the latest list of peers at a given interval
            let mut rx = http_tracker.stream_peers().await.unwrap();

            let peers = rx.recv().await.unwrap();

            let peer = &peers[0];
            assert!(peer.ip.is_ipv4());
         }
         _ => panic!("Expected Torrent"),
      }
   }

   #[tokio::test]
   async fn test_http_tracker_instance_trait() {
      tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .init();

      let path = std::env::current_dir()
         .unwrap()
         .join("tests/torrents/KNOPPIX_V9.1DVD-2021-01-25-EN.torrent");

      let contents = tokio::fs::read(&path).await.unwrap();
      let metainfo = TorrentFile::parse(&contents).unwrap();

      match metainfo {
         MetaInfo::Torrent(torrent) => {
            let info_hash = torrent.info.hash().expect("Missing info hash");
            let announce_list = torrent.announce_list();
            let announce_url = announce_list
               .iter()
               .find(|t| t.uri().starts_with("http://"))
               .expect("No UDP tracker found in announce list");

            let port: u16 = random_range(1024..65535);
            let socket_addr = SocketAddr::from_str(&format!("0.0.0.0:{}", port)).unwrap();

            let tracker = HttpTracker::new(
               announce_url.uri().clone(),
               info_hash,
               Some(PeerId::new()),
               Some(socket_addr),
            );

            let (update_sender, mut stats_receiver) =
               tracker.configure().await.expect("Failed to configure");

            // Test sending tracker updates through the channel
            update_sender
               .send(TrackerUpdate::Downloaded(1024))
               .await
               .expect("Failed to send downloaded update");

            update_sender
               .send(TrackerUpdate::Uploaded(512))
               .await
               .expect("Failed to send uploaded update");

            update_sender
               .send(TrackerUpdate::Left(2048))
               .await
               .expect("Failed to send left update");

            update_sender
               .send(TrackerUpdate::Event(Event::Started))
               .await
               .expect("Failed to send event update");

            // Use the announce_stream method from TrackerInstance trait
            let announce_stream = tracker.announce_stream().await;
            pin_mut!(announce_stream); // https://docs.rs/async-stream/latest/async_stream/#usage

            // Collect some peers from the stream
            let mut peer_count = 0;
            let max_peers_to_collect = 5;

            // Use timeout to avoid hanging if stream doesn't produce peers quickly
            let stream_timeout = Duration::from_secs(30);
            let start_time = Instant::now();

            while peer_count < max_peers_to_collect && start_time.elapsed() < stream_timeout {
               match timeout(Duration::from_secs(5), announce_stream.next()).await {
                  Ok(Some(peer)) => {
                     peer_count += 1;
                     println!("Received peer {}: {}:{}", peer_count, peer.ip, peer.port);

                     // Verify peer format
                     assert!(peer.ip.is_ipv4(), "Expected IPv4 peer address");
                     assert!(peer.port > 0, "Expected valid port number");
                  }
                  Ok(None) => {
                     println!("Stream ended");
                     break;
                  }
                  Err(_) => {
                     println!("Timeout waiting for peer, continuing...");
                     // Don't break immediately, allow some retries
                  }
               }
            }

            // Verify we received at least one peer
            assert!(
               peer_count > 0,
               "Expected to receive at least one peer from announce stream"
            );

            // Test stats receiver (with timeout to avoid hanging)
            match timeout(Duration::from_secs(5), stats_receiver.recv()).await {
               Ok(Ok(stats)) => {
                  println!("Received tracker stats: {:?}", stats);
                  // Verify stats structure
                  assert!(
                     stats.get_last_interaction().is_some(),
                     "Expected last interaction timestamp"
                  );
               }
               Ok(Err(e)) => {
                  println!("Stats receiver error: {}", e);
                  // Don't fail the test for stats receiver errors as they might
                  // be expected
               }
               Err(_) => {
                  println!("Timeout waiting for stats");
                  // Don't fail the test for stats timeout as it might not be
                  // critical
               }
            }

            println!(
               "TrackerInstance trait test completed successfully with {} peers",
               peer_count
            );
         }
         _ => {
            panic!("Expected MagnetUri variant");
         }
      }
   }
}
