use std::{
   fmt::Debug,
   net::{IpAddr, Ipv4Addr, SocketAddr},
   str::FromStr,
   sync::Arc,
};

use anyhow::Result;
use async_trait::async_trait;
use serde::{
   Deserialize,
   de::{self, Visitor},
};
use tokio::{
   sync::{RwLock, mpsc},
   time::{Duration, Instant, sleep},
};
use tracing::{debug, error, info, instrument, trace, warn};

/// See https://www.bittorrent.org/beps/bep_0003.html
use super::{Peer, TrackerTrait};
use crate::{
   errors::{HttpTrackerError, TrackerError},
   hashes::InfoHash,
   peer::PeerId,
   tracker::TrackerStats,
};

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // REMOVE SOON
pub struct TrackerResponse {
   pub interval: Option<usize>,
   #[serde(deserialize_with = "deserialize_peers")]
   pub peers: Vec<Peer>,
}

/// Event. See <https://www.bittorrent.org/beps/bep_0003.html> @ trackers
#[derive(Clone, Debug)]
pub enum Event {
   Started,
   Completed,
   Stopped,
   Empty,
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
   interval: usize,
   stats: TrackerStats,
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
         interval: usize::MAX,
         uri,
         peer_id,
         params,
         info_hash,
         stats: TrackerStats::default(),
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

            let delay = tracker.interval.max(1);
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
      self.interval = response.interval.unwrap_or(usize::MAX);

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
   use tracing_test::traced_test;

   use super::HttpTracker;
   use crate::{
      metainfo::{MetaInfo, TorrentFile},
      tracker::TrackerTrait,
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
}
