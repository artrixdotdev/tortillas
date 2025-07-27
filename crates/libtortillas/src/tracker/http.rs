use std::{
   net::{IpAddr, Ipv4Addr, SocketAddr},
   str::FromStr,
};

use anyhow::Result;
use async_trait::async_trait;
use serde::{
   Deserialize, Serialize,
   de::{self, Visitor},
};
use tokio::{
   sync::mpsc,
   time::{Duration, Instant, sleep},
};
use tracing::{debug, error, info, instrument, trace, warn};

/// See https://www.bittorrent.org/beps/bep_0003.html
use super::{Peer, TrackerTrait};
use crate::{
   errors::{HttpTrackerError, TrackerError},
   hashes::{Hash, InfoHash},
};

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // REMOVE SOON
pub struct TrackerResponse {
   pub interval: usize,
   #[serde(deserialize_with = "deserialize_peers")]
   pub peers: Vec<Peer>,
}

/// Event. See <https://www.bittorrent.org/beps/bep_0003.html> @ trackers
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Event {
   Started,
   Completed,
   Stopped,
   Empty,
}

/// Tracker request. See <https://www.bittorrent.org/beps/bep_0003.html> @ trackers
#[derive(Clone, Debug, Deserialize, Serialize)]
struct TrackerRequest {
   ip: Option<IpAddr>,
   port: u16,
   uploaded: u8,
   downloaded: u8,
   left: Option<u8>,
   event: Event,
}

impl TrackerRequest {
   #[instrument(fields(peer_tracker_addr = ?peer_tracker_addr))]
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

      debug!(
          peer_tracker_addr = %addr,
          event = ?Event::Stopped,
          "Creating new tracker request"
      );

      TrackerRequest {
         ip,
         port,
         uploaded: 0,
         downloaded: 0,
         left: None,
         event: Event::Stopped,
      }
   }
}

#[derive(Debug)]
struct HttpTrackerStats {
   request_attempts: u64,
   request_successes: u64,
   total_peers_received: u64,
   bytes_sent: u64,
   bytes_received: u64,
   last_successful_request: Option<Instant>,
   session_start: Instant,
   consecutive_failures: u64,
}

impl Default for HttpTrackerStats {
   fn default() -> Self {
      Self {
         request_attempts: 0,
         request_successes: 0,
         total_peers_received: 0,
         bytes_sent: 0,
         bytes_received: 0,
         last_successful_request: None,
         session_start: Instant::now(),
         consecutive_failures: 0,
      }
   }
}

/// Struct for handling tracker over HTTP
/// Interval is set to `u32::MAX` by default.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HttpTracker {
   uri: String,
   pub peer_id: Hash<20>,
   info_hash: InfoHash,
   params: TrackerRequest,
   interval: u32,
   #[serde(skip)]
   stats: std::sync::Arc<tokio::sync::Mutex<HttpTrackerStats>>,
}

impl HttpTracker {
   #[instrument(skip(info_hash, peer_id), fields(
        uri = %uri,
        info_hash = %info_hash,
        peer_tracker_addr = ?peer_tracker_addr
    ))]
   pub fn new(
      uri: String, info_hash: InfoHash, peer_id: Option<Hash<20>>,
      peer_tracker_addr: Option<SocketAddr>,
   ) -> HttpTracker {
      let creation_span = tracing::debug_span!("http_tracker_creation");
      let _enter = creation_span.enter();

      info!("Creating new HTTP tracker instance");

      let peer_id = peer_id.unwrap_or_else(|| {
         let id = Hash::new(rand::random());
         debug!(generated_peer_id = %id, "Generated new peer ID");
         id
      });

      let params = TrackerRequest::new(peer_tracker_addr);

      info!(
          peer_id = %peer_id,
          tracker_uri = %uri,
          peer_listener_addr = format!("{}:{}", params.ip.unwrap_or(Ipv4Addr::UNSPECIFIED.into()), params.port),
          "HTTP tracker instance created successfully"
      );

      HttpTracker {
         interval: u32::MAX,
         uri,
         peer_id,
         params,
         info_hash,
         stats: std::sync::Arc::new(tokio::sync::Mutex::new(HttpTrackerStats {
            session_start: Instant::now(),
            ..Default::default()
         })),
      }
   }

   #[instrument(skip(self))]
   async fn log_statistics(&self) {
      let stats = self.stats.lock().await;
      let session_duration = stats.session_start.elapsed();
      let last_request_ago = stats
         .last_successful_request
         .map(|t| t.elapsed())
         .unwrap_or(Duration::MAX);

      info!(
         session_duration_secs = session_duration.as_secs(),
         request_attempts = stats.request_attempts,
         request_successes = stats.request_successes,
         request_success_rate = if stats.request_attempts > 0 {
            (stats.request_successes as f64 / stats.request_attempts as f64) * 100.0
         } else {
            0.0
         },
         total_peers_received = stats.total_peers_received,
         bytes_sent = stats.bytes_sent,
         bytes_received = stats.bytes_received,
         consecutive_failures = stats.consecutive_failures,
         last_request_ago_secs = if last_request_ago != Duration::MAX {
            last_request_ago.as_secs()
         } else {
            0
         },
         current_interval = self.interval,
         "HTTP tracker session statistics"
      );
   }
}

#[instrument(skip(t), fields(hash_length = t.len()))]
fn urlencode(t: &[u8; 20]) -> String {
   trace!("URL-encoding hash bytes");

   let mut encoded = String::with_capacity(3 * t.len());

   for (i, &byte) in t.iter().enumerate() {
      encoded.push('%');
      let byte_hex = hex::encode([byte]);
      encoded.push_str(&byte_hex);

      if i < 3 {
         // Log first few bytes for debugging
         trace!(byte_index = i, byte_value = byte, byte_hex = %byte_hex, "Encoded byte");
      }
   }

   debug!(
      original_length = t.len(),
      encoded_length = encoded.len(),
      encoded_sample = &encoded[..std::cmp::min(30, encoded.len())],
      "URL encoding completed"
   );

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
      let stream_span = tracing::info_span!("http_peer_streaming");
      let _enter = stream_span.enter();

      info!("Starting HTTP tracker peer streaming");

      let (tx, rx) = mpsc::channel(100);
      let mut tracker = self.clone();
      let tx = tx.clone();

      tokio::spawn(async move {
         let task_span = tracing::info_span!("http_peer_stream_task");
         let _task_enter = task_span.enter();

         info!("HTTP peer streaming task started");

         let mut iteration = 0u64;
         let max_consecutive_failures = 5u64;
         let mut last_stats_log = Instant::now();
         let stats_interval = Duration::from_secs(300); // Log stats every 5 minutes

         loop {
            iteration += 1;
            let iteration_span = tracing::debug_span!("stream_iteration", iteration = iteration);
            let _iter_enter = iteration_span.enter();

            debug!(iteration = iteration, "Starting peer fetch iteration");

            // Log statistics periodically
            if last_stats_log.elapsed() > stats_interval {
               tracker.log_statistics().await;
               last_stats_log = Instant::now();
            }

            let start_time = Instant::now();
            match tracker.get_peers().await {
               Ok(peers) => {
                  let fetch_duration = start_time.elapsed();

                  // Reset consecutive failures on success
                  {
                     let mut stats = tracker.stats.lock().await;
                     stats.consecutive_failures = 0;
                  }

                  info!(
                     iteration = iteration,
                     peers_count = peers.len(),
                     fetch_duration_ms = fetch_duration.as_millis(),
                     "Successfully fetched peers from HTTP tracker"
                  );

                  if tx.send(peers).await.is_err() {
                     error!("Failed to send peers to receiver - channel closed");
                     break;
                  }

                  trace!("Peers sent to receiver successfully");
               }
               Err(e) => {
                  let fetch_duration = start_time.elapsed();

                  // Update consecutive failures
                  let consecutive_failures = {
                     let mut stats = tracker.stats.lock().await;
                     stats.consecutive_failures += 1;
                     stats.consecutive_failures
                  };

                  error!(
                      iteration = iteration,
                      consecutive_failures = consecutive_failures,
                      fetch_duration_ms = fetch_duration.as_millis(),
                      error = %e,
                      "Failed to fetch peers from HTTP tracker"
                  );

                  if consecutive_failures >= max_consecutive_failures {
                     error!(
                        consecutive_failures = consecutive_failures,
                        max_failures = max_consecutive_failures,
                        "Too many consecutive failures, terminating peer stream"
                     );
                     break;
                  }

                  // Send empty peer list on error to keep the stream alive
                  if tx.send(vec![]).await.is_err() {
                     error!("Failed to send empty peer list - channel closed");
                     break;
                  }
               }
            }

            let delay = tracker.interval.max(1);
            debug!(
               next_request_delay_secs = delay,
               "Waiting before next peer fetch iteration"
            );

            sleep(Duration::from_secs(delay as u64)).await;
         }

         info!("HTTP peer streaming task terminated");
      });

      info!("HTTP peer streaming setup completed");
      Ok(rx)
   }

   #[instrument(skip(self), fields(
        tracker_uri = %self.uri,
        info_hash = %self.info_hash,
        peer_id = %self.peer_id
    ))]
   async fn get_peers(&mut self) -> Result<Vec<Peer>> {
      let get_peers_span = tracing::info_span!("http_get_peers");
      let _enter = get_peers_span.enter();

      info!("Starting HTTP tracker peer fetch process");

      // Update statistics
      {
         let mut stats = self.stats.lock().await;
         stats.request_attempts += 1;
      }

      // URL encoding phase
      let encoding_span = tracing::debug_span!("url_encoding");
      let (info_hash_encoded, peer_id_encoded, params_encoded) = {
         let _encoding_enter = encoding_span.enter();

         debug!("Starting URL encoding phase");

         let params_encoded = serde_qs::to_string(&self.params).map_err(|e| {
            error!(error = %e, "Failed to serialize tracker request parameters");
            HttpTrackerError::ParameterEncoding(e.to_string())
         })?;

         debug!(
            params_length = params_encoded.len(),
            params_sample = &params_encoded[..std::cmp::min(100, params_encoded.len())],
            "Serialized request parameters"
         );

         let info_hash_encoded = urlencode(self.info_hash.as_bytes());
         let peer_id_encoded = urlencode(self.peer_id.as_bytes());

         trace!(
            info_hash_encoded_length = info_hash_encoded.len(),
            peer_id_encoded_length = peer_id_encoded.len(),
            "URL encoding completed"
         );

         (info_hash_encoded, peer_id_encoded, params_encoded)
      };

      // URI construction
      let uri_params =
         format!("{params_encoded}&info_hash={info_hash_encoded}&peer_id={peer_id_encoded}");

      let uri = format!("{}?{}", self.uri, &uri_params);

      debug!(
          request_uri_length = uri.len(),
          base_uri = %self.uri,
          params_length = uri_params.len(),
          "Generated tracker request URI"
      );

      // HTTP request phase
      let request_span = tracing::debug_span!("http_request");
      let (response_bytes, request_duration) = {
         let _request_enter = request_span.enter();

         info!(request_uri = %uri, "Sending HTTP request to tracker");
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
         let headers = response.headers().clone();

         debug!(
             status_code = status.as_u16(),
             status_text = %status,
             headers_count = headers.len(),
             "Received HTTP response from tracker"
         );

         // Log some important headers
         if let Some(content_length) = headers.get("content-length") {
            debug!(content_length = ?content_length, "Response content length");
         }
         if let Some(content_type) = headers.get("content-type") {
            debug!(content_type = ?content_type, "Response content type");
         }

         let response_bytes = response.bytes().await.map_err(|e| {
            error!(error = %e, "Failed to read tracker response body");
            HttpTrackerError::Request(e)
         })?;

         let request_duration = request_start.elapsed();

         debug!(
            response_size = response_bytes.len(),
            request_duration_ms = request_duration.as_millis(),
            "Successfully received tracker response body"
         );

         (response_bytes, request_duration)
      };

      // Update network statistics
      {
         let mut stats = self.stats.lock().await;
         stats.bytes_sent += uri.len() as u64; // Approximate bytes sent
         stats.bytes_received += response_bytes.len() as u64;
      }

      // Response parsing phase
      let parsing_span = tracing::debug_span!("response_parsing");
      let response = {
         let _parsing_enter = parsing_span.enter();

         debug!("Starting bencode response parsing");

         // Log a sample of the response for debugging (first 100 bytes)
         let sample_size = std::cmp::min(100, response_bytes.len());
         let response_sample = &response_bytes[..sample_size];
         trace!(
             response_sample_size = sample_size,
             response_sample = ?response_sample,
             "Response body sample"
         );

         let response: TrackerResponse =
            serde_bencode::from_bytes(&response_bytes).map_err(|e| {
               error!(
                   error = %e,
                   response_size = response_bytes.len(),
                   "Failed to decode bencode response"
               );
               HttpTrackerError::Tracker(TrackerError::BencodeError(e))
            })?;

         debug!(
            peers_count = response.peers.len(),
            interval_seconds = response.interval,
            "Successfully decoded bencode response"
         );

         response
      };

      // Update success statistics
      {
         let mut stats = self.stats.lock().await;
         stats.request_successes += 1;
         stats.total_peers_received += response.peers.len() as u64;
         stats.last_successful_request = Some(Instant::now());
         stats.consecutive_failures = 0;
      }

      // Log peer samples for debugging
      if !response.peers.is_empty() {
         let sample_size = std::cmp::min(3, response.peers.len());
         for (i, peer) in response.peers.iter().take(sample_size).enumerate() {
            trace!(
                peer_index = i,
                peer_addr = %peer.socket_addr(),
                "Sample peer from HTTP tracker response"
            );
         }
      }

      info!(
         peers_received = response.peers.len(),
         interval_seconds = response.interval,
         request_duration_ms = request_duration.as_millis(),
         "HTTP tracker peer fetch completed successfully"
      );

      // Update interval
      self.interval = response.interval as u32;

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
      let parsing_span = tracing::debug_span!("peer_bytes_parsing");
      let _enter = parsing_span.enter();

      debug!("Starting peer bytes parsing");

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
      debug!(
         expected_peer_count = expected_peer_count,
         total_bytes = bytes.len(),
         "Calculated expected peer count"
      );

      for (i, chunk) in bytes.chunks(PEER_SIZE).enumerate() {
         let peer_span = tracing::trace_span!("parse_peer", peer_index = i);
         let _peer_enter = peer_span.enter();

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

         trace!(
             peer_index = i,
             ip = %ip,
             port = port,
             ip_bytes = ?&chunk[0..4],
             port_bytes = ?&chunk[4..6],
             "Successfully parsed peer address"
         );

         peers.push(Peer::from_ipv4(ip, port));
      }

      info!(
         peers_parsed = peers.len(),
         expected_peers = expected_peer_count,
         parsing_success_rate = if expected_peer_count > 0 {
            (peers.len() as f64 / expected_peer_count as f64) * 100.0
         } else {
            0.0
         },
         "Peer bytes parsing completed"
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
   trace!("Starting peer deserialization");
   let result = deserializer.deserialize_bytes(PeerVisitor);
   match &result {
      Ok(peers) => {
         debug!(
            peers_deserialized = peers.len(),
            "Peer deserialization successful"
         );
      }
      Err(e) => {
         error!(error = %e, "Peer deserialization failed");
      }
   }
   result
}

#[cfg(test)]
mod tests {
   use tracing_test::traced_test;

   use super::HttpTracker;
   use crate::{
      parser::{MagnetUri, MetaInfo},
      tracker::TrackerTrait,
   };

   #[tokio::test]
   #[traced_test]
   async fn test_get_peers_with_http_tracker() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/zenshuu.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();
      let metainfo = MagnetUri::parse(contents).unwrap();
      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let info_hash = magnet.info_hash();
            let announce_list = magnet.announce_list.unwrap();
            let announce_uri = announce_list[0].uri();
            let mut http_tracker = HttpTracker::new(announce_uri, info_hash.unwrap(), None, None);

            // Make request
            // let res = HttpTracker::get_peers(&mut http_tracker)
            //    .await
            //    .expect("Issue when unwrapping result of get_peers");

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
