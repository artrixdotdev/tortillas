use std::{
   fmt::{Debug, Display},
   net::{Ipv4Addr, SocketAddr},
   str::FromStr,
   sync::Arc,
};

use anyhow::anyhow;
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
   net::{UdpSocket, lookup_host},
   sync::mpsc,
   time::{Duration, Instant, sleep, timeout},
};
use tokio_retry2::{Retry, RetryError, strategy::ExponentialBackoff};
use tracing::{debug, error, info, instrument, trace, warn};

use super::{Peer, TrackerTrait};
use crate::{
   errors::{TrackerError, UdpTrackerError},
   hashes::InfoHash,
   peer::PeerId,
   tracker::TrackerStats,
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
const MESSAGE_TIMEOUT: Duration = Duration::from_millis(300);

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
      peer_id: PeerId,
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
   /// - [IP (4 bytes) action + Port (2 bytes)] (6 bytes) * n
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

            trace!(serialized_size = buf.len(), "Connect request serialized");
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
            buf.extend_from_slice(peer_id.id()); // Peer ID
            buf.extend_from_slice(&downloaded.to_be_bytes()); // Downloaded
            buf.extend_from_slice(&left.to_be_bytes()); // Left
            buf.extend_from_slice(&uploaded.to_be_bytes()); // Uploaded
            buf.extend_from_slice(&(*event as u32).to_be_bytes()); // Event
            buf.extend_from_slice(&ip_address.to_be_bytes()); // IP Address
            buf.extend_from_slice(&key.to_be_bytes()); // Key
            buf.extend_from_slice(&num_want.to_be_bytes()); // Num Want
            buf.extend_from_slice(&port.to_be_bytes()); // Port

            trace!(serialized_size = buf.len(), "Announce request serialized");
         }
      }
      buf
   }
}

/// Accepts a response (in bytes) from a UDP [tracker request](TrackerRequest).
impl TrackerResponse {
   #[instrument(skip(bytes), fields(response_size = bytes.len()))]
   pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
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

      trace!(action = ?action, "Parsed response action");

      match action {
         Action::Connect => {
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

            trace!(
               transaction_id = transaction_id,
               connection_id = connection_id,
               "Parsed connect response"
            );

            Ok(TrackerResponse::Connect {
               action,
               connection_id,
               transaction_id,
            })
         }
         Action::Announce => {
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
            let num_peers = (bytes.len() - 20) / PEER_SIZE;

            trace!(
               transaction_id = transaction_id,
               interval = interval,
               leechers = leechers,
               seeders = seeders,
               num_peers = num_peers,
               "Parsing announce response"
            );

            for i in 0..num_peers {
               let offset = 20 + i * PEER_SIZE;
               if offset + PEER_SIZE > bytes.len() {
                  warn!(
                     peer_index = i,
                     offset = offset,
                     bytes_len = bytes.len(),
                     "Incomplete peer data, stopping peer parsing"
                  );
                  break;
               }

               let ip_bytes: [u8; 4] = bytes[offset..offset + 4].try_into().unwrap();
               let ip = Ipv4Addr::from(ip_bytes);

               let port_bytes: [u8; 2] = bytes[offset + 4..offset + PEER_SIZE].try_into().unwrap();
               let port = u16::from_be_bytes(port_bytes);

               peers.push(Peer::from_ipv4(ip, port));
            }

            trace!(peers_parsed = peers.len(), "Parsed announce response peers");

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
               "Unsupported action: {action:?}"
            )))
         }
      }
   }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ReadyState {
   Ready,
   Disconnected,
}

#[derive(Clone)]
pub struct UdpTracker {
   /// Raw SocketAddr for the tracker
   addr: SocketAddr,
   connection_id: Option<ConnectionId>,
   pub socket: Arc<UdpSocket>,
   ready_state: ReadyState,
   pub peer_id: PeerId,
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
      uri: String, socket: Option<Arc<UdpSocket>>, info_hash: InfoHash,
      peer_socket_addr: Option<SocketAddr>, peer_id: Option<PeerId>,
   ) -> Result<UdpTracker> {
      let addrs = lookup_host(&uri.replace("udp://", ""))
         .await
         .unwrap()
         .filter(|addr| addr.is_ipv4()) // We dont support ipv6 yet
         .collect::<Vec<_>>();

      trace!("Resolved addresses: {:?}", addrs);
      let addr = *addrs.first().unwrap();

      let sock = match socket {
         Some(sock) => {
            let local_addr = sock.local_addr().map_err(|e| {
               error!(error = %e, "Failed to get local address from provided socket");
               UdpTrackerError::ConnectionFailed(format!("Failed to get socket address: {e}"))
            })?;

            debug!(local_addr = %local_addr, "Using provided socket");
            sock
         }
         None => {
            let sock = UdpSocket::bind("0.0.0.0:0").await.map_err(|e| {
               error!(error = %e, "Failed to bind new UDP socket");
               UdpTrackerError::ConnectionFailed(format!("Failed to bind socket: {e}"))
            })?;

            let local_addr = sock.local_addr().map_err(|e| {
               error!(error = %e, "Failed to get local address from new socket");
               UdpTrackerError::ConnectionFailed(format!("Failed to get socket address: {e}"))
            })?;
            debug!(local_addr = %local_addr, "Created new UDP socket");
            Arc::new(sock)
         }
      };

      let peer_id = peer_id.unwrap_or_else(|| {
         let id = PeerId::new();
         trace!(generated_peer_id = %id, "Generated new peer ID");
         id
      });

      let peer_socket_addr = peer_socket_addr.unwrap_or_else(|| {
         let default_addr = SocketAddr::from_str("0.0.0.0:6881").unwrap();
         trace!(default_addr = %default_addr, "Using default peer socket address");
         default_addr
      });

      debug!("UDP tracker instance created");

      Ok(UdpTracker {
         addr,
         interval: u32::MAX,
         connection_id: None,
         socket: sock,
         ready_state: ReadyState::Disconnected,
         peer_id,
         info_hash,
         peer_socket_addr,
         stats: Arc::new(tokio::sync::Mutex::new(TrackerStats::default())),
      })
   }

   fn is_our_message(
      our_transaction_id: &TransactionId, tracker_response: &TrackerResponse,
   ) -> bool {
      use TrackerResponse::*;
      match tracker_response {
         Announce { transaction_id, .. } => transaction_id == our_transaction_id,
         Connect { transaction_id, .. } => transaction_id == our_transaction_id,
         Error { transaction_id, .. } => transaction_id == our_transaction_id,
      }
   }

   async fn recv_retry(
      &self, transaction_id: &TransactionId,
   ) -> anyhow::Result<TrackerResponse, RetryError<UdpTrackerError>> {
      let mut buf = Vec::new();

      let (size, addr) = timeout(MESSAGE_TIMEOUT, self.socket.recv_buf_from(&mut buf))
         .await
         .map_err(|e| {
            warn!(elapsed = %e, "Tracker timed out");
            RetryError::Transient {
               err: UdpTrackerError::MessageTimeout,
               retry_after: None,
            }
         })?
         .map_err(|_| {
            warn!("Failed to receive message back from tracker");
            RetryError::Transient {
               err: UdpTrackerError::MessageTimeout,
               retry_after: None,
            }
         })?;

      {
         let mut stats = self.stats.lock().await;
         stats.bytes_received += size;
      }

      let response = TrackerResponse::from_bytes(&buf).map_err(RetryError::Permanent)?;

      // Verify the response is for us
      if self.addr != addr {
         warn!(
             expected_addr = %self.addr,
             received_addr = %addr,
             "Received response from unexpected address, ignoring"
         );
         return Err(RetryError::Transient {
            err: UdpTrackerError::InvalidResponse("Response from unexpected address".into()),
            retry_after: Some(Duration::ZERO), // Retry immediately
         });
      }

      if !Self::is_our_message(transaction_id, &response) {
         warn!(
            expected_tid = transaction_id,
            "Received response with wrong transaction ID, ignoring"
         );
         return Err(RetryError::Transient {
            err: UdpTrackerError::InvalidResponse("Wrong transaction ID".into()),
            retry_after: Some(Duration::ZERO), // Retry immediately
         });
      }

      Ok(response)
   }

   async fn send_and_recv_retry(
      &self, message: &TrackerRequest, transaction_id: &TransactionId,
   ) -> anyhow::Result<TrackerResponse, RetryError<UdpTrackerError>> {
      let message_bytes = message.to_bytes();

      // Send the message
      self
         .socket
         .send_to(&message_bytes, self.addr)
         .await
         .map_err(|e| {
            error!(error = ?e, "Failed to send message to tracker");
            RetryError::Permanent(UdpTrackerError::MessageTimeout)
         })?;

      {
         let mut stats = self.stats.lock().await;
         stats.bytes_sent += message_bytes.len();
      };

      trace!("Sent message to tracker");

      // Try to receive response
      self.recv_retry(transaction_id).await
   }

   #[instrument(skip(self, message), fields(self.addr))]
   async fn send_and_wait(&self, message: TrackerRequest) -> Result<TrackerResponse> {
      let transaction_id = match &message {
         TrackerRequest::Connect(_, _, tid) => *tid,
         TrackerRequest::Announce { transaction_id, .. } => *transaction_id,
      };

      // UDP is an 'unreliable' protocol. This means it doesn't retransmit lost
      // packets itself. The application is responsible for this. If a response is not
      // received after 15 * 2 ^ n seconds, the client should retransmit the request,
      // where n starts at 0 and is increased up to 8 (3840 seconds) after every
      // retransmission. Note that it is necessary to rerequest a connection ID when
      // it has expired.
      //
      // From BEP 0015
      let retry_strategy = ExponentialBackoff::from_millis(15 * 1000)
         .factor(2) // multiplication factor applied to delay
         .max_delay_millis(3840 * 1000) // set max delay between retries
         .take(8); // limit to 8 retries as per BEP 0015

      let response = Retry::spawn(retry_strategy, || {
         self.send_and_recv_retry(&message, &transaction_id)
      })
      .await?;

      Ok(response)
   }

   #[instrument(skip(self), fields(
        tracker_uri = %self.addr,
        connection_id = ?self.connection_id
    ))]
   async fn connect(&mut self) -> Result<ConnectionId> {
      // Update statistics
      {
         let mut stats = self.stats.lock().await;
         stats.connect_attempts += 1;
      }

      let transaction_id: TransactionId = rand::random();
      trace!(transaction_id = transaction_id, "Preparing connect request");

      let request = TrackerRequest::Connect(MAGIC_CONSTANT, Action::Connect, transaction_id);
      let response = self.send_and_wait(request).await?;

      match response {
         TrackerResponse::Connect {
            connection_id,
            transaction_id: _,
            ..
         } => {
            debug!(
               connection_id = connection_id,
               "Received connection ID from tracker"
            );

            {
               let mut stats = self.stats.lock().await;
               stats.connect_successes += 1;
            }

            self.connection_id = Some(connection_id);
            self.ready_state = ReadyState::Ready;
            Ok(connection_id)
         }
         TrackerResponse::Error {
            message,
            transaction_id: _,
            ..
         } => {
            error!(
                error_message = %message,
                transaction_id = transaction_id,
                "Tracker returned error for connect request"
            );
            Err(UdpTrackerError::TrackerMessage(message))
         }
         _ => {
            error!(
                response_type = ?response,
                "Unexpected response type to connect request"
            );
            Err(UdpTrackerError::InvalidResponse(
               "Expected connect response".to_string(),
            ))
         }
      }
   }

   #[instrument(skip(self), fields(
        tracker_uri = %self.addr,
        ready_state = ?self.ready_state,
        connection_id = ?self.connection_id
    ))]
   async fn announce(&mut self) -> Result<Vec<Peer>> {
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
      trace!(
         transaction_id = transaction_id,
         "Preparing announce request"
      );

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

      let response = self.send_and_wait(request).await?;

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
               stats.total_peers_received += peers.len();
               stats.last_successful_announce = Some(Instant::now());
            }

            debug!(
               peers_count = peers.len(),
               interval_seconds = interval,
               swarm_leechers = leechers,
               swarm_seeders = seeders,
               "Announce completed successfully"
            );

            self.interval = interval;
            Ok(peers)
         }
         TrackerResponse::Error {
            message,
            transaction_id,
            ..
         } => {
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
        tracker_uri = %self.addr,
        info_hash = %self.info_hash
    ))]
   async fn stream_peers(&mut self) -> anyhow::Result<mpsc::Receiver<Vec<Peer>>> {
      info!("Starting UDP tracker peer streaming");

      let (tx, rx) = mpsc::channel(100);

      // Clone any data needed by the spawned task
      let tx = tx.clone();
      let mut tracker = self.clone();

      tokio::spawn(async move {
         let mut iteration = 0usize;
         let mut consecutive_failures = 0usize;
         let max_consecutive_failures = 5;
         let mut last_stats_log = Instant::now();
         let stats_interval = Duration::from_secs(300); // Log stats every 5 minutes

         loop {
            iteration += 1;

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

                  debug!(
                     iteration = iteration,
                     peers_count = peers.len(),
                     fetch_duration_ms = fetch_duration.as_millis(),
                     "Successfully fetched peers from tracker"
                  );

                  if tx.send(peers).await.is_err() {
                     warn!("Peer stream receiver closed, terminating");
                     break;
                  }
               }
               Err(e) => {
                  consecutive_failures += 1;
                  let fetch_duration = start_time.elapsed();

                  warn!(
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
                     warn!("Peer stream receiver closed, terminating");
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

         debug!("UDP peer streaming task terminated");
      });

      Ok(rx)
   }

   #[instrument(skip(self), fields(
        tracker_uri = %self.addr,
        ready_state = ?self.ready_state
    ))]
   async fn get_peers(&mut self) -> anyhow::Result<Vec<Peer>, anyhow::Error> {
      // Connect if not already connected
      if self.ready_state != ReadyState::Ready {
         self.connect().await?;
      }

      // Attempt announce with connection ID retry logic
      match self.announce().await {
         Ok(peers) => Ok(peers),
         Err(UdpTrackerError::TrackerMessage(msg)) if msg.contains("connection") => {
            // Connection ID might have expired, try reconnecting once
            warn!(
                error_message = %msg,
                "Connection ID may have expired, attempting reconnect"
            );

            self.ready_state = ReadyState::Disconnected;
            self.connection_id = None;

            // Reconnect and try announce again
            self.connect().await?;
            self.announce().await.map_err(Into::into)
         }
         Err(e) => Err(e.into()),
      }
   }
}

#[cfg(test)]
mod tests {
   use rand::random_range;
   use tracing_test::traced_test;

   use super::*;
   use crate::metainfo::{MagnetUri, MetaInfo};

   #[tokio::test]
   #[traced_test]
   async fn test_stream_with_udp_peers() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");

      let contents = tokio::fs::read_to_string(&path).await.unwrap();
      let metainfo = MagnetUri::parse(contents).unwrap();

      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let info_hash = magnet.info_hash().expect("Missing info hash");
            let announce_list = magnet.announce_list.expect("Missing announce list");
            let announce_url = &announce_list[0].uri();

            let port: u16 = random_range(1024..65535);
            let socket_addr = SocketAddr::from(([0, 0, 0, 0], port));

            let mut udp_tracker = UdpTracker::new(
               announce_url.clone(),
               None,
               info_hash,
               Some(socket_addr),
               None,
            )
            .await
            .expect("Failed to create UDP tracker");

            // Spawn a task to re-fetch the latest list of peers at a given interval
            let mut rx = udp_tracker
               .stream_peers()
               .await
               .expect("Failed to start peer streaming");

            let peers = rx.recv().await.expect("Failed to receive peers");

            assert!(!peers.is_empty(), "Expected to receive at least one peer");

            let peer = &peers[0];
            assert!(peer.ip.is_ipv4(), "Expected IPv4 peer address");
         }
         _ => {
            panic!("Expected MagnetUri variant");
         }
      }
   }
}
