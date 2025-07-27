use std::{
   fmt::{Debug, Display},
   net::{Ipv4Addr, SocketAddr},
   str::FromStr,
   sync::Arc,
};

use async_trait::async_trait;
/// UDP protocol
/// https://en.wikipedia.org/wiki/User_Datagram_Protocol
///
/// Please see the following for the UDP *tracker* protocol spec.
/// https://www.bittorrent.org/beps/bep_0015.html
/// https://xbtt.sourceforge.net/udp_tracker_protocol.html
use num_enum::TryFromPrimitive;
use serde_repr::{Deserialize_repr, Serialize_repr};
use tokio::{
   net::UdpSocket,
   sync::mpsc,
   time::{Duration, Instant, sleep},
};
use tracing::{debug, error, info, instrument, trace, warn};

use super::{Peer, TrackerTrait};
use crate::{
   errors::{TrackerError, UdpTrackerError},
   hashes::{Hash, InfoHash},
};

/// Types and constants
type ConnectionId = u64;
type TransactionId = u32;
type Result<T> = anyhow::Result<T, UdpTrackerError>;

const MAGIC_CONSTANT: ConnectionId = 0x41727101980;
const MIN_CONNECT_RESPONSE_SIZE: usize = 16;
const MIN_ANNOUNCE_RESPONSE_SIZE: usize = 20;
const MIN_ERROR_RESPONSE_SIZE: usize = 8;
const PEER_SIZE: usize = 6;

/// Enum for UDP Tracker Protocol Action parameter. See this resource for more information: <https://xbtt.sourceforge.net/udp_tracker_protocol.html>
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
pub enum Action {
   Connect = 0u32,
   Announce = 1u32,
   Scrape = 2u32,
   Error = 3u32,
}

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
pub enum Events {
   None = 0u32,
   Completed = 1u32,
   Started = 2u32,
   Stopped = 3u32,
}

/// Headers for tracker request
#[derive(Debug)]
enum TrackerRequest {
   /// Binary layout for the Connect variant:
   /// - [Magic constant](MAGIC_CONSTANT) (8 bytes)
   /// - [Action](Action::Connect) (4 bytes)
   /// - [Transaction ID](TransactionId) (4 bytes)
   ///
   /// Total: 16 bytes
   ///
   /// | Magic constant | Action | Transaction ID |
   /// |----------------|--------|----------------|
   /// |    00000000    |  0000  |     0000       |
   Connect(ConnectionId, Action, TransactionId),

   /// Binary layout for the Announce variant:
   /// - [Connection ID](ConnectionId) (8 bytes)
   /// - [Action](Action::Announce) (4 bytes)
   /// - [Transaction ID](TransactionId) (4 bytes)
   /// - [Info Hash](crate::hashes::Hash) (20 bytes)
   /// - [Peer ID] (20 bytes)
   /// - [Downloaded] (8 bytes)
   /// - [Left] (8 bytes)
   /// - [Uploaded] (8 bytes)
   /// - [Event] (4 bytes)
   /// - [IP Address] (4 bytes)
   /// - [Key] (4 bytes)
   /// - [Num Want] (4 bytes, -1 for default)
   /// - [Port] (2 bytes)
   ///
   /// Total: 98 bytes
   Announce {
      connection_id: ConnectionId,
      transaction_id: TransactionId,
      info_hash: InfoHash,
      peer_id: Hash<20>,
      downloaded: u64,
      left: u64,
      uploaded: u64,
      event: Events,
      ip_address: u32,
      key: u32,
      num_want: i32,
      port: u16,
   },
}

#[derive(Debug)]
#[allow(dead_code)]
enum TrackerResponse {
   /// Note that the response headers for TrackerResponse are somewhat different
   /// in comparison to TrackerRequest:
   ///
   /// Binary Layout for the Connect variant:
   /// - [Action](Action::Connect) (4 bytes)
   /// - [Transaction ID](TransactionId) (4 bytes)
   /// - [Connection ID](ConnectionId) (8 bytes)
   ///
   /// Total: 16 bytes
   /// | Action | Transaction ID | Connection ID |
   /// |--------|----------------|---------------|
   /// |  0000  |      0000      |   00000000    |
   Connect {
      action: Action,
      connection_id: ConnectionId,
      transaction_id: TransactionId,
   },

   /// Binary Layout for the Announce variant:
   /// - [Action](Action::Announce) (4 bytes)
   /// - [Transaction ID](TransactionId) (4 bytes)
   /// - [Interval] (4 bytes)
   /// - [Leechers] (4 bytes)
   /// - [Seeders] (4 bytes)
   /// - [IP (4 bytes) + Port (2 bytes)] (6 bytes) * n
   ///
   /// Total: 20 + 6n bytes
   Announce {
      action: Action,
      transaction_id: TransactionId,
      /// The interval inwhich we should send another announce request
      interval: u32,
      leechers: u32,
      seeders: u32,
      peers: Vec<super::Peer>,
   },

   /// Binary Layout for the Error variant:
   /// - [Action](Action::Error) (4 bytes)
   /// - [Transaction ID](TransactionId) (4 bytes)
   /// - [Error Message] (variable)
   ///
   /// Total: 8 + message length bytes
   Error {
      action: Action,
      transaction_id: TransactionId,
      message: String,
   },
}
impl Display for TrackerRequest {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      match self {
         TrackerRequest::Connect(_, _, _) => write!(f, "Connect"),
         TrackerRequest::Announce { .. } => write!(f, "Announce"),
      }
   }
}

/// Formats the headers for a request in the UDP Tracker Protocol
impl TrackerRequest {
   #[instrument(skip(self), fields(request_type = %self))]
   pub fn to_bytes(&self) -> Vec<u8> {
      let mut buf = Vec::new();
      match self {
         TrackerRequest::Connect(id, action, transaction_id) => {
            trace!(
                magic_constant = id,
                action = ?action,
                transaction_id = transaction_id,
                "Serializing connect request"
            );

            buf.extend_from_slice(&id.to_be_bytes()); // Magic constant
            buf.extend_from_slice(&(*action as u32).to_be_bytes()); // Action
            buf.extend_from_slice(&transaction_id.to_be_bytes()); // Transaction ID

            debug!(
               serialized_size = buf.len(),
               expected_size = 16,
               "Connect request serialized"
            );
         }

         TrackerRequest::Announce {
            connection_id,
            transaction_id,
            info_hash,
            peer_id,
            downloaded,
            left,
            uploaded,
            event,
            ip_address,
            key,
            num_want,
            port,
         } => {
            trace!(
                connection_id = connection_id,
                transaction_id = transaction_id,
                info_hash = %info_hash,
                peer_id = %peer_id,
                downloaded = downloaded,
                left = left,
                uploaded = uploaded,
                event = ?event,
                port = port,
                num_want = num_want,
                "Serializing announce request"
            );

            buf.extend_from_slice(&connection_id.to_be_bytes()); // Connection ID
            buf.extend_from_slice(&(Action::Announce as u32).to_be_bytes()); // Action
            buf.extend_from_slice(&transaction_id.to_be_bytes()); // Transaction ID
            buf.extend_from_slice(info_hash.as_bytes()); // Info Hash
            buf.extend_from_slice(peer_id.as_bytes()); // Peer ID
            buf.extend_from_slice(&downloaded.to_be_bytes()); // Downloaded
            buf.extend_from_slice(&left.to_be_bytes()); // Left
            buf.extend_from_slice(&uploaded.to_be_bytes()); // Uploaded
            buf.extend_from_slice(&(*event as u32).to_be_bytes()); // Event
            buf.extend_from_slice(&ip_address.to_be_bytes()); // IP Address
            buf.extend_from_slice(&key.to_be_bytes()); // Key
            buf.extend_from_slice(&num_want.to_be_bytes()); // Num Want
            buf.extend_from_slice(&port.to_be_bytes()); // Port

            debug!(
               serialized_size = buf.len(),
               expected_size = 98,
               "Announce request serialized"
            );
         }
      }
      buf
   }
}

/// Accepts a response (in bytes) from a UDP [tracker request](TrackerRequest).
impl TrackerResponse {
   #[instrument(skip(bytes), fields(response_size = bytes.len()))]
   pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
      debug!("Parsing tracker response");

      if bytes.len() < 4 {
         error!(
            expected_min = 4,
            actual = bytes.len(),
            "Response too short to contain action field"
         );
         return Err(UdpTrackerError::ResponseTooShort {
            expected: 4,
            actual: bytes.len(),
         });
      }

      let action = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
      let action: Action = Action::try_from(action).map_err(|e| {
         error!(
            invalid_action = e.number,
            "Received invalid action in response"
         );
         TrackerError::InvalidAction(e.number)
      })?;

      debug!(action = ?action, "Parsed response action");

      match action {
         Action::Connect => {
            let connect_span = tracing::debug_span!("parse_connect_response");
            let _enter = connect_span.enter();

            if bytes.len() < MIN_CONNECT_RESPONSE_SIZE {
               error!(
                  expected = MIN_CONNECT_RESPONSE_SIZE,
                  actual = bytes.len(),
                  "Connect response too short"
               );
               return Err(UdpTrackerError::ResponseTooShort {
                  expected: MIN_CONNECT_RESPONSE_SIZE,
                  actual: bytes.len(),
               });
            }

            let transaction_id = TransactionId::from_be_bytes(bytes[4..8].try_into().unwrap());
            let connection_id = ConnectionId::from_be_bytes(bytes[8..16].try_into().unwrap());

            info!(
                action = ?action,
                transaction_id = transaction_id,
                connection_id = connection_id,
                "Successfully parsed connect response"
            );

            Ok(TrackerResponse::Connect {
               action,
               connection_id,
               transaction_id,
            })
         }
         Action::Announce => {
            let announce_span = tracing::debug_span!("parse_announce_response");
            let _enter = announce_span.enter();

            if bytes.len() < MIN_ANNOUNCE_RESPONSE_SIZE {
               error!(
                  expected = MIN_ANNOUNCE_RESPONSE_SIZE,
                  actual = bytes.len(),
                  "Announce response too short"
               );
               return Err(UdpTrackerError::ResponseTooShort {
                  expected: MIN_ANNOUNCE_RESPONSE_SIZE,
                  actual: bytes.len(),
               });
            }

            let transaction_id = TransactionId::from_be_bytes(bytes[4..8].try_into().unwrap());
            let interval = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
            let leechers = u32::from_be_bytes(bytes[12..16].try_into().unwrap());
            let seeders = u32::from_be_bytes(bytes[16..20].try_into().unwrap());

            // Parse peers (each peer is 6 bytes: 4 for IP, 2 for port)
            let mut peers = Vec::new();
            // Subtract the size of the current bytes we've already dealt with (20) from the
            // total length of the bytes, then divide by the size of a peer ip (6 bytes)
            let num_peers = (bytes.len() - 20) / PEER_SIZE;
            let available_peer_bytes = bytes.len() - 20;

            debug!(
               transaction_id = transaction_id,
               interval = interval,
               leechers = leechers,
               seeders = seeders,
               num_peers = num_peers,
               available_peer_bytes = available_peer_bytes,
               "Parsing announce response metadata"
            );

            for i in 0..num_peers {
               let peer_span = tracing::trace_span!("parse_peer", peer_index = i);
               let _peer_enter = peer_span.enter();

               let offset = 20 + i * PEER_SIZE;
               if offset + PEER_SIZE > bytes.len() {
                  warn!(
                     peer_index = i,
                     offset = offset,
                     bytes_len = bytes.len(),
                     required_bytes = PEER_SIZE,
                     "Incomplete peer data, stopping peer parsing"
                  );
                  break;
               }

               let ip_bytes: [u8; 4] = bytes[offset..offset + 4].try_into().unwrap();
               let ip = Ipv4Addr::from(ip_bytes);

               let port_bytes: [u8; 2] = bytes[offset + 4..offset + PEER_SIZE].try_into().unwrap();
               let port = u16::from_be_bytes(port_bytes);

               trace!(
                   peer_index = i,
                   ip = %ip,
                   port = port,
                   offset = offset,
                   "Successfully parsed peer address"
               );

               peers.push(Peer::from_ipv4(ip, port));
            }

            info!(
               peers_parsed = peers.len(),
               expected_peers = num_peers,
               interval_seconds = interval,
               swarm_leechers = leechers,
               swarm_seeders = seeders,
               "Successfully parsed announce response"
            );

            Ok(TrackerResponse::Announce {
               action,
               transaction_id,
               interval,
               leechers,
               seeders,
               peers,
            })
         }
         Action::Error => {
            let error_span = tracing::debug_span!("parse_error_response");
            let _enter = error_span.enter();

            if bytes.len() < MIN_ERROR_RESPONSE_SIZE {
               error!(
                  expected = MIN_ERROR_RESPONSE_SIZE,
                  actual = bytes.len(),
                  "Error response too short"
               );
               return Err(UdpTrackerError::ResponseTooShort {
                  expected: MIN_ERROR_RESPONSE_SIZE,
                  actual: bytes.len(),
               });
            }

            let transaction_id = TransactionId::from_be_bytes(bytes[4..8].try_into().unwrap());
            let message = String::from_utf8_lossy(&bytes[8..]).to_string();

            error!(
                transaction_id = transaction_id,
                error_message = %message,
                message_length = message.len(),
                "Received error response from tracker"
            );

            Ok(TrackerResponse::Error {
               action,
               transaction_id,
               message,
            })
         }
         _ => {
            error!(unsupported_action = ?action, "Received unsupported action in response");
            Err(UdpTrackerError::InvalidResponse(format!(
               "Unsupported action: {:?}",
               action
            )))
         }
      }
   }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ReadyState {
   Connected,
   Ready,
   Disconnected,
}

#[derive(Clone, Debug)]
struct TrackerStats {
   connect_attempts: u64,
   connect_successes: u64,
   announce_attempts: u64,
   announce_successes: u64,
   total_peers_received: u64,
   bytes_sent: u64,
   bytes_received: u64,
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

#[derive(Clone)]
pub struct UdpTracker {
   uri: String,
   connection_id: Option<ConnectionId>,
   pub socket: Arc<UdpSocket>,
   ready_state: ReadyState,
   pub peer_id: Hash<20>,
   info_hash: InfoHash,
   ///  The address that our TCP or uTP socket is bound to
   peer_socket_addr: SocketAddr,
   interval: u32,
   stats: Arc<tokio::sync::Mutex<TrackerStats>>,
}

impl UdpTracker {
   #[instrument(skip(socket, info_hash, peer_id), fields(
        uri = %uri,
        info_hash = %info_hash,
        peer_socket_addr = ?peer_socket_addr
    ))]
   pub async fn new(
      uri: String, socket: Option<UdpSocket>, info_hash: InfoHash,
      peer_socket_addr: Option<SocketAddr>, peer_id: Option<Hash<20>>,
   ) -> Result<UdpTracker> {
      let creation_span = tracing::debug_span!("udp_tracker_creation");
      let _enter = creation_span.enter();

      info!("Creating new UDP tracker instance");

      let sock = match socket {
         Some(sock) => {
            let local_addr = sock.local_addr().map_err(|e| {
               error!(error = %e, "Failed to get local address from provided socket");
               UdpTrackerError::ConnectionFailed(format!("Failed to get socket address: {}", e))
            })?;
            debug!(local_addr = %local_addr, "Using provided socket");
            sock
         }
         None => {
            debug!("Creating new socket on 0.0.0.0:0");
            let sock = UdpSocket::bind("0.0.0.0:0").await.map_err(|e| {
               error!(error = %e, "Failed to bind new UDP socket");
               UdpTrackerError::ConnectionFailed(format!("Failed to bind socket: {}", e))
            })?;

            let local_addr = sock.local_addr().map_err(|e| {
               error!(error = %e, "Failed to get local address from new socket");
               UdpTrackerError::ConnectionFailed(format!("Failed to get socket address: {}", e))
            })?;
            debug!(local_addr = %local_addr, "Created new UDP socket");
            sock
         }
      };

      let peer_id = peer_id.unwrap_or_else(|| {
         let id = Hash::new(rand::random());
         debug!(generated_peer_id = %id, "Generated new peer ID");
         id
      });

      let peer_socket_addr = peer_socket_addr.unwrap_or_else(|| {
         let default_addr = SocketAddr::from_str("0.0.0.0:6881").unwrap();
         debug!(default_addr = %default_addr, "Using default peer socket address");
         default_addr
      });

      info!(
          peer_id = %peer_id,
          peer_socket_addr = %peer_socket_addr,
          tracker_uri = %uri,
          "UDP tracker instance created successfully"
      );

      Ok(UdpTracker {
         uri,
         interval: u32::MAX,
         connection_id: None,
         socket: Arc::new(sock),
         ready_state: ReadyState::Disconnected,
         peer_id,
         info_hash,
         peer_socket_addr,
         stats: Arc::new(tokio::sync::Mutex::new(TrackerStats::default())),
      })
   }

   #[instrument(skip(self), fields(
        tracker_uri = %self.uri,
        ready_state = ?self.ready_state,
        connection_id = ?self.connection_id
    ))]
   async fn announce(&mut self) -> Result<Vec<Peer>> {
      let announce_span = tracing::info_span!("udp_announce");
      let _enter = announce_span.enter();

      if self.ready_state != ReadyState::Ready {
         error!(
             current_state = ?self.ready_state,
             "Tracker not ready for announce request"
         );
         return Err(UdpTrackerError::Tracker(TrackerError::NotReady(
            "Tracker not ready for announce request".to_string(),
         )));
      };

      // Update statistics
      {
         let mut stats = self.stats.lock().await;
         stats.announce_attempts += 1;
      }

      let transaction_id: TransactionId = rand::random();
      debug!(
          transaction_id = transaction_id,
          connection_id = ?self.connection_id,
          "Preparing announce request"
      );

      // Perform announce logic here
      let request = TrackerRequest::Announce {
         connection_id: self.connection_id.unwrap(),
         transaction_id,
         info_hash: self.info_hash,
         peer_id: self.peer_id,
         downloaded: 0,
         left: 0,
         uploaded: 0,
         event: Events::Started,
         ip_address: 0,
         key: 0,
         num_want: -1,
         port: self.peer_socket_addr.port(),
      };

      let request_bytes = request.to_bytes();
      trace!(
         request_size = request_bytes.len(),
         "Sending announce request to tracker"
      );

      let send_result = self.socket.send(&request_bytes).await;
      match send_result {
         Ok(bytes_sent) => {
            debug!(
               bytes_sent = bytes_sent,
               "Announce request sent successfully"
            );
            let mut stats = self.stats.lock().await;
            stats.bytes_sent += bytes_sent as u64;
         }
         Err(e) => {
            error!(error = %e, "Failed to send announce request");
            return Err(UdpTrackerError::ConnectionFailed(format!(
               "Failed to send announce request: {}",
               e
            )));
         }
      }

      let mut buf = Vec::new();
      trace!("Waiting for announce response from tracker");

      let recv_result = self.socket.recv_buf_from(&mut buf).await;
      match recv_result {
         Ok((bytes_received, remote_addr)) => {
            debug!(
                bytes_received = bytes_received,
                remote_addr = %remote_addr,
                "Received announce response"
            );
            let mut stats = self.stats.lock().await;
            stats.bytes_received += bytes_received as u64;
         }
         Err(e) => {
            error!(error = %e, "Failed to receive announce response");
            return Err(UdpTrackerError::ConnectionFailed(format!(
               "Failed to receive announce response: {}",
               e
            )));
         }
      }

      let response = TrackerResponse::from_bytes(&buf)?;

      match response {
         TrackerResponse::Announce {
            peers,
            transaction_id: resp_tid,
            interval,
            leechers,
            seeders,
            ..
         } => {
            if resp_tid != transaction_id {
               error!(
                  expected_tid = transaction_id,
                  received_tid = resp_tid,
                  "Transaction ID mismatch in announce response"
               );
               return Err(UdpTrackerError::Tracker(TrackerError::TransactionMismatch));
            }

            // Update statistics
            {
               let mut stats = self.stats.lock().await;
               stats.announce_successes += 1;
               stats.total_peers_received += peers.len() as u64;
               stats.last_successful_announce = Some(Instant::now());
            }

            info!(
               peers_count = peers.len(),
               interval_seconds = interval,
               swarm_leechers = leechers,
               swarm_seeders = seeders,
               transaction_id = transaction_id,
               "Announce completed successfully"
            );

            // Log some peer samples for debugging
            if !peers.is_empty() {
               let sample_size = std::cmp::min(3, peers.len());
               for (i, peer) in peers.iter().take(sample_size).enumerate() {
                  trace!(
                      peer_index = i,
                      peer_addr = %peer.socket_addr(),
                      "Sample peer from announce response"
                  );
               }
            }

            self.interval = interval;
            Ok(peers)
         }
         TrackerResponse::Error {
            message,
            transaction_id: resp_tid,
            ..
         } => {
            if resp_tid != transaction_id {
               error!(
                  expected_tid = transaction_id,
                  received_tid = resp_tid,
                  "Transaction ID mismatch in error response"
               );
               return Err(UdpTrackerError::Tracker(TrackerError::TransactionMismatch));
            }

            error!(
                error_message = %message,
                transaction_id = transaction_id,
                "Tracker returned error for announce request"
            );
            Err(UdpTrackerError::TrackerMessage(message))
         }
         _ => {
            error!(
                response_type = ?response,
                "Unexpected response type to announce request"
            );
            Err(UdpTrackerError::InvalidResponse(
               "Expected announce response".to_string(),
            ))
         }
      }
   }

   #[instrument(skip(self))]
   async fn log_statistics(&self) {
      let stats = self.stats.lock().await;
      let session_duration = stats.session_start.elapsed();
      let last_announce_ago = stats
         .last_successful_announce
         .map(|t| t.elapsed())
         .unwrap_or(Duration::MAX);

      info!(
         session_duration_secs = session_duration.as_secs(),
         connect_attempts = stats.connect_attempts,
         connect_successes = stats.connect_successes,
         connect_success_rate = if stats.connect_attempts > 0 {
            (stats.connect_successes as f64 / stats.connect_attempts as f64) * 100.0
         } else {
            0.0
         },
         announce_attempts = stats.announce_attempts,
         announce_successes = stats.announce_successes,
         announce_success_rate = if stats.announce_attempts > 0 {
            (stats.announce_successes as f64 / stats.announce_attempts as f64) * 100.0
         } else {
            0.0
         },
         total_peers_received = stats.total_peers_received,
         bytes_sent = stats.bytes_sent,
         bytes_received = stats.bytes_received,
         last_announce_ago_secs = if last_announce_ago != Duration::MAX {
            last_announce_ago.as_secs()
         } else {
            0
         },
         current_interval = self.interval,
         "UDP tracker session statistics"
      );
   }
}

#[async_trait]
impl TrackerTrait for UdpTracker {
   #[instrument(skip(self), fields(
        tracker_uri = %self.uri,
        info_hash = %self.info_hash
    ))]
   async fn stream_peers(&mut self) -> anyhow::Result<mpsc::Receiver<Vec<Peer>>> {
      let stream_span = tracing::info_span!("udp_peer_streaming");
      let _enter = stream_span.enter();

      info!("Starting UDP tracker peer streaming");

      let (tx, rx) = mpsc::channel(100);

      // Clone any data needed by the spawned task
      let tx = tx.clone();
      let mut tracker = self.clone();

      tokio::spawn(async move {
         let task_span = tracing::info_span!("udp_peer_stream_task");
         let _task_enter = task_span.enter();

         info!("UDP peer streaming task started");

         let mut iteration = 0u64;
         let mut consecutive_failures = 0u64;
         let max_consecutive_failures = 5;
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
                  consecutive_failures = 0;

                  info!(
                     iteration = iteration,
                     peers_count = peers.len(),
                     fetch_duration_ms = fetch_duration.as_millis(),
                     "Successfully fetched peers from tracker"
                  );

                  if tx.send(peers).await.is_err() {
                     error!("Failed to send peers to receiver - channel closed");
                     break;
                  }

                  trace!("Peers sent to receiver successfully");
               }
               Err(e) => {
                  consecutive_failures += 1;
                  let fetch_duration = start_time.elapsed();

                  error!(
                      iteration = iteration,
                      consecutive_failures = consecutive_failures,
                      fetch_duration_ms = fetch_duration.as_millis(),
                      error = %e,
                      "Failed to fetch peers from tracker"
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

         info!("UDP peer streaming task terminated");
      });

      info!("UDP peer streaming setup completed");
      Ok(rx)
   }

   // Makes a request using the UDP tracker protocol to connect. Returns a u64
   // connection ID
   #[instrument(skip(self), fields(
        tracker_uri = %self.uri,
        ready_state = ?self.ready_state
    ))]
   async fn get_peers(&mut self) -> anyhow::Result<Vec<Peer>, anyhow::Error> {
      let get_peers_span = tracing::info_span!("udp_get_peers");
      let _enter = get_peers_span.enter();

      info!("Starting UDP tracker peer fetch process");

      let uri = self.uri.replace("udp://", "");
      let uri = uri.replace("/announce", "");

      debug!(
          target_uri = %uri,
          current_state = ?self.ready_state,
          "Preparing to connect to tracker"
      );

      // Connection phase
      if self.ready_state != ReadyState::Connected {
         let connect_span = tracing::debug_span!("udp_connect");
         let _connect_enter = connect_span.enter();

         // Update statistics
         {
            let mut stats = self.stats.lock().await;
            stats.connect_attempts += 1;
         }

         debug!("Establishing UDP connection to tracker");

         let connect_result = self.socket.connect(&uri).await;
         match connect_result {
            Ok(()) => {
               let peer_addr = self.socket.peer_addr().map_err(|e| {
                  error!(error = %e, "Failed to get peer address after connect");
                  UdpTrackerError::ConnectionFailed(format!("Failed to get peer address: {}", e))
               })?;

               info!(peer_addr = %peer_addr, "UDP socket connected to tracker");
               self.ready_state = ReadyState::Connected;

               let mut stats = self.stats.lock().await;
               stats.connect_successes += 1;
            }
            Err(e) => {
               error!(error = %e, target_uri = %uri, "Failed to connect to tracker");
               return Err(
                  UdpTrackerError::ConnectionFailed(format!("Failed to connect to tracker: {}", e))
                     .into(),
               );
            }
         }
      }

      let transaction_id: TransactionId = rand::random();
      debug!(
         transaction_id = transaction_id,
         magic_constant = MAGIC_CONSTANT,
         "Preparing connect request"
      );

      // Send connect request
      let request = TrackerRequest::Connect(MAGIC_CONSTANT, Action::Connect, transaction_id);
      let request_bytes = request.to_bytes();

      trace!(
         request_size = request_bytes.len(),
         "Sending connect request to tracker"
      );

      // Send the request
      let send_result = self.socket.send(&request_bytes).await;
      match send_result {
         Ok(bytes_sent) => {
            debug!(bytes_sent = bytes_sent, "Connect request sent successfully");
            let mut stats = self.stats.lock().await;
            stats.bytes_sent += bytes_sent as u64;
         }
         Err(e) => {
            error!(error = %e, "Failed to send connect request");
            return Err(
               UdpTrackerError::ConnectionFailed(format!("Failed to send connect request: {}", e))
                  .into(),
            );
         }
      }

      // Receive response
      let mut buffer = Vec::new();
      trace!("Waiting for connect response from tracker");

      let recv_result = self.socket.recv_buf(&mut buffer).await;
      match recv_result {
         Ok(bytes_received) => {
            debug!(
               bytes_received = bytes_received,
               "Received connect response from tracker"
            );
            let mut stats = self.stats.lock().await;
            stats.bytes_received += bytes_received as u64;
         }
         Err(e) => {
            error!(error = %e, "Failed to receive connect response");
            return Err(
               UdpTrackerError::ConnectionFailed(format!(
                  "Failed to receive connect response: {}",
                  e
               ))
               .into(),
            );
         }
      }

      // Parse response
      let response = TrackerResponse::from_bytes(&buffer)?;

      // Check response
      match response {
         TrackerResponse::Connect {
            connection_id,
            transaction_id: resp_tid,
            ..
         } => {
            // Transaction ID's have to be the same per request. If I send a request to the
            // tracker, the tracker should respond with the same transaction ID.
            // These should be unique per request though.
            if resp_tid != transaction_id {
               error!(
                  expected_tid = transaction_id,
                  received_tid = resp_tid,
                  "Transaction ID mismatch in connect response"
               );
               return Err(UdpTrackerError::Tracker(TrackerError::TransactionMismatch).into());
            }

            info!(
               connection_id = connection_id,
               transaction_id = transaction_id,
               "Received valid connection ID from tracker"
            );

            self.connection_id = Some(connection_id);
            self.ready_state = ReadyState::Ready;

            info!("Tracker connection established, proceeding to announce");

            // Proceed to announce
            let announce_result = self.announce().await;
            match &announce_result {
               Ok(peers) => {
                  info!(
                     peers_received = peers.len(),
                     "Peer fetch completed successfully"
                  );
               }
               Err(e) => {
                  error!(error = %e, "Announce phase failed");
               }
            }

            Ok(announce_result?)
         }
         TrackerResponse::Error {
            message,
            transaction_id: resp_tid,
            ..
         } => {
            if resp_tid != transaction_id {
               error!(
                  expected_tid = transaction_id,
                  received_tid = resp_tid,
                  "Transaction ID mismatch in error response"
               );
               return Err(UdpTrackerError::Tracker(TrackerError::TransactionMismatch).into());
            }

            error!(
                error_message = %message,
                transaction_id = transaction_id,
                "Tracker returned error for connect request"
            );
            Err(UdpTrackerError::TrackerMessage(message).into())
         }
         _ => {
            error!(
                response_type = ?response,
                "Unexpected response type to connect request"
            );
            Err(UdpTrackerError::InvalidResponse("Expected connect response".to_string()).into())
         }
      }
   }
}

#[cfg(test)]
mod tests {
   use rand::random_range;
   use tracing_test::traced_test;

   use super::*;
   use crate::parser::{MagnetUri, MetaInfo};

   #[tokio::test]
   #[traced_test]
   async fn test_stream_with_udp_peers() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");

      info!(test_file = %path.display(), "Starting UDP tracker test");

      let contents = tokio::fs::read_to_string(&path).await.unwrap();
      debug!(magnet_uri_length = contents.len(), "Loaded magnet URI");

      let metainfo = MagnetUri::parse(contents).unwrap();

      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let info_hash = magnet.info_hash().expect("Missing info hash");
            let announce_list = magnet.announce_list.expect("Missing announce list");
            let announce_url = &announce_list[0].uri();

            info!(
                info_hash = %info_hash,
                announce_url = %announce_url,
                "Parsed magnet URI successfully"
            );

            let port: u16 = random_range(1024..65535);
            let socket_addr = SocketAddr::from(([0, 0, 0, 0], port));

            debug!(
                test_port = port,
                socket_addr = %socket_addr,
                "Generated test socket address"
            );

            let mut udp_tracker = UdpTracker::new(
               announce_url.clone(),
               None,
               info_hash,
               Some(socket_addr),
               None,
            )
            .await
            .expect("Failed to create UDP tracker");

            info!("UDP tracker created, starting peer streaming test");

            // Spawn a task to re-fetch the latest list of peers at a given interval
            let mut rx = udp_tracker
               .stream_peers()
               .await
               .expect("Failed to start peer streaming");

            debug!("Waiting for first batch of peers");
            let peers = rx.recv().await.expect("Failed to receive peers");

            info!(
               peers_received = peers.len(),
               "Received peers from UDP tracker"
            );

            assert!(!peers.is_empty(), "Expected to receive at least one peer");

            let peer = &peers[0];
            debug!(
                first_peer = %peer.socket_addr(),
                is_ipv4 = peer.ip.is_ipv4(),
                "Examining first peer"
            );

            assert!(peer.ip.is_ipv4(), "Expected IPv4 peer address");

            info!("UDP tracker test completed successfully");
         }
         _ => {
            error!("Expected MagnetUri, got different MetaInfo type");
            panic!("Expected MagnetUri variant");
         }
      }
   }
}
