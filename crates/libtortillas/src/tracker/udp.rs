use std::{
   fmt::{Debug, Display},
   net::{Ipv4Addr, SocketAddr},
   str::FromStr,
   sync::{
      Arc,
      atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering},
   },
};

use anyhow::anyhow;
use async_trait::async_trait;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use dashmap::DashMap;
use num_enum::TryFromPrimitive;
use serde_repr::{Deserialize_repr, Serialize_repr};
use tokio::{
   net::{UdpSocket, lookup_host},
   sync::{RwLock, mpsc},
   time::{Duration, timeout},
};
use tokio_retry2::{Retry, RetryError, strategy::ExponentialBackoff};
use tracing::{debug, error, instrument, trace, warn};

use super::Peer;
use crate::{
   errors::TrackerActorError,
   hashes::InfoHash,
   peer::PeerId,
   tracker::{Event, TrackerBase, TrackerStats, TrackerUpdate},
};

/// The connection ID for a UDP connection. This is not the same as a
/// transaction ID.
type ConnectionId = u64;
/// The connection ID as an atomic.
type AtomicConnectionId = AtomicU64;
/// The transaction ID for a given UDP send & recv. This is not the same as a
/// connection ID
type TransactionId = u32;
/// A type alias for all Results in this file, for convenience.
type Result<T> = anyhow::Result<T, TrackerActorError>;
/// A magic constant for the UDP tracker protocol. needed in
/// [TrackerRequest::Connect]
const MAGIC_CONSTANT: ConnectionId = 0x41727101980;
/// Minimum connection response size, in bytes
const MIN_CONNECT_RESPONSE_SIZE: usize = 16;
/// Minimum announce response size, in bytes
const MIN_ANNOUNCE_RESPONSE_SIZE: usize = 20;
/// Minimum error response size, in bytes
const MIN_ERROR_RESPONSE_SIZE: usize = 8;
/// The maximum amount of time a connect message can take to response
/// This hard-caps the retries regardless of [MESSAGE_TIMEOUT]
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(3);
/// The size of each peer returned from a tracker, in bytes
const PEER_SIZE: usize = 6;
/// The maximum amount of time a message can take to response
const MESSAGE_TIMEOUT: Duration = Duration::from_millis(300);

/// Tracker request variants with binary layouts.
#[derive(Debug)]
enum TrackerRequest {
   /// Connect request (16 bytes total)
   Connect(ConnectionId, Action, TransactionId),

   /// Announce request (98 bytes total)
   Announce {
      /// Connection ID (8 bytes)
      connection_id: ConnectionId,
      /// Transaction ID (4 bytes)
      transaction_id: TransactionId,
      /// Info Hash (20 bytes)
      info_hash: InfoHash,
      /// Peer ID (20 bytes)
      peer_id: PeerId,
      /// Downloaded bytes (8 bytes)
      downloaded: u64,
      /// Bytes left to download (8 bytes)
      left: u64,
      /// Uploaded bytes (8 bytes)
      uploaded: u64,
      /// Event type (4 bytes)
      event: Event,
      /// IP address (4 bytes)
      ip_address: u32,
      /// Random key (4 bytes)
      key: u32,
      /// Number of peers wanted (-1 for default, 4 bytes)
      num_want: i32,
      /// Port number (2 bytes)
      port: u16,
   },
}

/// Formats the headers for a request in the UDP Tracker Protocol
impl TrackerRequest {
   pub fn transaction_id(&self) -> TransactionId {
      match self {
         Self::Connect(_, _, tid) => *tid,
         Self::Announce { transaction_id, .. } => *transaction_id,
      }
   }

   #[instrument(skip(self), fields(%self))]
   pub fn to_bytes(&self) -> Bytes {
      let mut buf = BytesMut::new();

      match self {
         TrackerRequest::Connect(id, action, transaction_id) => {
            buf.put_u64(*id); // Magic constant
            buf.put_u32(*action as u32); // Action
            buf.put_u32(*transaction_id); // Transaction ID
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
            buf.put_u64(*connection_id); // Connection ID
            buf.put_u32(Action::Announce as u32); // Action
            buf.put_u32(*transaction_id); // Transaction ID
            buf.put_slice(info_hash.as_bytes()); // Info Hash
            buf.put_slice(peer_id.id()); // Peer ID
            buf.put_u64(*downloaded); // Downloaded
            buf.put_u64(*left); // Left
            buf.put_u64(*uploaded); // Uploaded
            buf.put_u32(*event as u32); // Event
            buf.put_u32(*ip_address); // IP Address
            buf.put_u32(*key); // Key
            buf.put_i32(*num_want); // Num Want
            buf.put_u16(*port); // Port
         }
      }
      let result = buf.freeze();
      trace!(size = result.len(), "Request serialized");
      result
   }
}

impl Display for TrackerRequest {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      match self {
         TrackerRequest::Connect(_, _, _) => write!(f, "Connect"),
         TrackerRequest::Announce { .. } => write!(f, "Announce"),
      }
   }
}

/// Tracker response variants with binary layouts.
#[derive(Debug)]
#[allow(dead_code)]
enum TrackerResponse {
   /// Connect response (16 bytes total)
   Connect {
      /// Action type (4 bytes)
      action: Action,
      /// Connection ID (8 bytes)
      connection_id: ConnectionId,
      /// Transaction ID (4 bytes)
      transaction_id: TransactionId,
   },

   /// Announce response (20 + 6n bytes total)
   Announce {
      /// Action type (4 bytes)
      action: Action,
      /// Transaction ID (4 bytes)
      transaction_id: TransactionId,
      /// Interval in seconds until next announce (4 bytes)
      interval: u32,
      /// Number of leechers (4 bytes)
      leechers: u32,
      /// Number of seeders (4 bytes)
      seeders: u32,
      /// List of [peers](super::Peer) (6 bytes per peer) unless ipv6
      peers: Vec<super::Peer>,
   },

   /// Error response (8 + message length bytes total)
   Error {
      /// Action type (4 bytes)
      action: Action,
      /// Transaction ID (4 bytes)
      transaction_id: TransactionId,
      /// Error message (variable length)
      message: String,
   },
}

/// Accepts a response (in bytes) from a UDP [tracker request](TrackerRequest).
impl TrackerResponse {
   pub fn transaction_id(&self) -> TransactionId {
      match self {
         Self::Connect { transaction_id, .. } => *transaction_id,
         Self::Announce { transaction_id, .. } => *transaction_id,
         Self::Error { transaction_id, .. } => *transaction_id,
      }
   }

   #[instrument(skip(bytes), fields(response_size = bytes.len()))]
   pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
      if bytes.len() < 4 {
         error!(
            expected_min = 4,
            actual = bytes.len(),
            "Response too short to contain action field"
         );
         return Err(TrackerActorError::ResponseTooShort {
            expected: 4,
            actual: bytes.len(),
         });
      }

      let mut buf = Bytes::copy_from_slice(bytes);
      let action = buf.get_u32();
      let action: Action = Action::try_from(action).map_err(|e| {
         error!(
            invalid_action = e.number,
            "Received invalid action in response"
         );
         TrackerActorError::InvalidAction { action: e.number }
      })?;

      trace!(action = ?action, "Parsed response action");

      let response = match action {
         Action::Connect => {
            if bytes.len() < MIN_CONNECT_RESPONSE_SIZE {
               error!(
                  expected = MIN_CONNECT_RESPONSE_SIZE,
                  actual = bytes.len(),
                  "Connect response too short"
               );
               return Err(TrackerActorError::ResponseTooShort {
                  expected: MIN_CONNECT_RESPONSE_SIZE,
                  actual: bytes.len(),
               });
            }

            let transaction_id = buf.get_u32();
            let connection_id = buf.get_u64();

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
               return Err(TrackerActorError::ResponseTooShort {
                  expected: MIN_ANNOUNCE_RESPONSE_SIZE,
                  actual: bytes.len(),
               });
            }

            let transaction_id = buf.get_u32();
            let interval = buf.get_u32();
            let leechers = buf.get_u32();
            let seeders = buf.get_u32();

            // Parse peers (each peer is 6 bytes: 4 for IP, 2 for port)
            // see [PEER_SIZE]
            let mut peers = Vec::new();
            let num_peers = buf.remaining() / PEER_SIZE;

            for i in 0..num_peers {
               if buf.remaining() < PEER_SIZE {
                  warn!(
                     peer_index = i,
                     remaining = buf.remaining(),
                     "Incomplete peer data, stopping peer parsing"
                  );
                  break;
               }

               let ip = Ipv4Addr::from(buf.get_u32());
               let port = buf.get_u16();

               peers.push(Peer::from_ipv4(ip, port));
            }

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
               return Err(TrackerActorError::ResponseTooShort {
                  expected: MIN_ERROR_RESPONSE_SIZE,
                  actual: bytes.len(),
               });
            }

            let transaction_id = buf.get_u32();
            let message = String::from_utf8_lossy(&buf).to_string();

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
            Err(TrackerActorError::InvalidResponse {
               reason: format!("Unsupported action: {action:?}"),
            })
         }
      };
      response.inspect(|r| trace!(response = %r, "Successfully processed response"))
   }
}

impl Display for TrackerResponse {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      match self {
         TrackerResponse::Connect { connection_id, .. } => write!(f, "Connect ({connection_id})"),
         TrackerResponse::Announce {
            peers,
            seeders,
            leechers,
            interval,
            ..
         } => f
            .debug_struct("Announce")
            .field("peers", &peers.len())
            .field("seeders", &seeders)
            .field("leechers", &leechers)
            .field("interval", &interval)
            .finish(),
         TrackerResponse::Error { message, .. } => write!(f, "Error ({message})"),
      }
   }
}

/// The data type that we send to every [UdpTracker] from [UdpServer]
type ServerMessage = (usize, TrackerResponse);

/// Centralized message receiver that broadcasts messages to all trackers
#[derive(Clone, Debug)]
pub struct UdpServer {
   /// Map of transaction IDs to sender channels for routing responses
   response_channels: Arc<DashMap<TransactionId, mpsc::UnboundedSender<ServerMessage>>>,
   /// Shared socket for all trackers
   socket: Arc<UdpSocket>,
}

impl UdpServer {
   /// Creates a new UDP Server, intended to be used to interact with multiple
   /// trackers with a singular [UdpSocket]
   ///
   /// Only fails if the `addr` is in use
   pub async fn new(addr: Option<SocketAddr>) -> Self {
      let addr = addr.unwrap_or(SocketAddr::from_str("0.0.0.0:0").unwrap());
      let socket = Arc::new(UdpSocket::bind(addr).await.unwrap());

      let receiver = UdpServer {
         response_channels: Arc::new(DashMap::new()),
         socket,
      };

      // Start the message receiving task
      receiver.start_receiver_task().await;
      receiver
   }

   pub fn local_addr(&self) -> SocketAddr {
      self.socket.local_addr().unwrap()
   }

   /// Register a transaction ID with a response channel
   async fn register_transaction(
      &self, transaction_id: TransactionId, sender: mpsc::UnboundedSender<(usize, TrackerResponse)>,
   ) {
      self.response_channels.insert(transaction_id, sender);
      trace!(transaction_id = transaction_id, "Registered transaction");
   }

   /// Unregister a transaction ID
   pub async fn unregister_transaction(&self, transaction_id: &TransactionId) {
      self.response_channels.remove(transaction_id);
      trace!(transaction_id = transaction_id, "Unregistered transaction");
   }

   /// Start the background task that receives messages and routes them to
   /// correct channels
   async fn start_receiver_task(&self) {
      let receiver = self.clone();
      // UDP tracker messages are typically small (< 1500 bytes for MTU)
      // Largest expected response is announce with many peers
      let mut buf = vec![0u8; 8192]; // 8KB should be sufficient for tracker responses

      let response_channels = self.response_channels.clone();
      tokio::spawn(async move {
         loop {
            match receiver.socket.recv_from(&mut buf).await {
               Ok((size, addr)) => {
                  trace!(size = size, addr = %addr, "Received UDP message");

                  // Parse the response
                  match TrackerResponse::from_bytes(&buf[..size]) {
                     Ok(response) => {
                        // Extract transaction ID and route to correct channel
                        let transaction_id = response.transaction_id();

                        trace!(
                            transaction_id = transaction_id,
                            response_type = %response,
                            "Routing response to transaction channel"
                        );

                        // Send to the specific transaction channel
                        {
                           if let Some(sender) = response_channels.get(&transaction_id) {
                              if let Err(e) = sender.send((size, response)) {
                                 trace!(
                                     transaction_id = transaction_id,
                                     error = %e,
                                     "Failed to send response to transaction channel (likely closed)"
                                 );
                              } else {
                                 trace!(
                                    transaction_id = transaction_id,
                                    "Successfully routed response to transaction channel"
                                 );
                              }
                           } else {
                              trace!(
                                 transaction_id = transaction_id,
                                 "No channel registered for transaction ID"
                              );
                           }
                        }
                     }
                     Err(e) => {
                        warn!(error = %e, addr = %addr, "Failed to parse UDP response");
                     }
                  }
               }
               Err(e) => {
                  error!(error = %e, "Error receiving UDP message");
                  break;
               }
            }
         }
      });
   }

   /// Send a message through the shared socket
   pub async fn send_message(&self, message: &Bytes, addr: SocketAddr) -> Result<()> {
      self.socket.send_to(message, addr).await.map_err(|e| {
         error!(error = ?e, "Failed to send message to tracker");
         TrackerActorError::InvalidResponse {
            reason: format!("send_to failed: {e}"),
         }
      })?;

      Ok(())
   }
}

/// This is not defined in any official BitTorrent spec. It is only implemented
/// to allow us to see if a tracker is ready to use.
#[derive(Clone, Debug, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
enum ReadyState {
   Ready,
   Disconnected,
}

/// Enum for UDP Tracker Protocol Action parameter. See this resource for more
/// information: <https://xbtt.sourceforge.net/udp_tracker_protocol.html>
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

/// Parameters for announce requests.
///
/// As BEP does not always specify the units for their own protocol, please see [BitTorrent Theory](https://wiki.theory.org/BitTorrentSpecification#Tracker_Request_Parameters) for more information about the tracker request parameters.
#[derive(Debug, Default)]
struct AnnounceParams {
   /// Bytes left to download.
   left: u32,
   /// Total downloaded bytes.
   downloaded: u64,
   /// Total uploaded bytes.
   uploaded: u64,
   /// The current event of the tracker. See [Event]
   event: Event,
}

/// The core struct for Udp Trackers.
///
/// This struct contains quite a few getter and setter functions, such as
/// [UdpTracker::set_connection_id]. Consequently, these functions are not
/// documented. They are generally wrapped because the underlying field is an
/// atomic, and accessing an atomic takes a few lines of code.
#[derive(Clone)]
pub struct UdpTracker {
   /// Raw SocketAddr for the tracker
   addr: SocketAddr,
   /// The URI/URL of the tracker.
   uri: String,
   /// The field containing the UDP socket that we use to communicate with the
   /// tracker.
   server: UdpServer,
   /// Set to 0 by default
   connection_id: Arc<AtomicConnectionId>,
   /// The [ReadyState] of the tracker, stored as an atomic u8.
   ready_state: Arc<AtomicU8>,
   /// Our peer id
   peer_id: PeerId,
   /// The info hash, as specified in the documentation for the [InfoHash] type.
   info_hash: InfoHash,
   ///  The address that our TCP or uTP socket is bound to
   peer_addr: SocketAddr,
   /// The interval delay between announce requests by which the tracker
   /// specifies.
   interval: Arc<AtomicU32>,
   /// Collection of statistics. See [TrackerStats].
   stats: TrackerStats,
   /// Parameters for announce request. See [AnnounceParams].
   announce_params: Arc<RwLock<AnnounceParams>>,
}

impl UdpTracker {
   /// Creates a new tracker instance, optionally using a [UDP
   /// server](UdpServer) and runs a [DNS lookup](lookup_host) to get the IP
   /// of the tracker
   ///
   /// Will only fail if the DNS lookup fails
   #[instrument(skip(info_hash, peer_info, server), fields(
        uri = %uri,
        info_hash = %info_hash,
        peer_id = %peer_info.0,
        port = %peer_info.1.port(),
        server = ?server.clone().map(|s| s.local_addr())
    ))]
   pub async fn new(
      uri: String, server: Option<UdpServer>, info_hash: InfoHash, peer_info: (PeerId, SocketAddr),
   ) -> Result<UdpTracker> {
      let (peer_id, peer_addr) = peer_info;
      let addrs = lookup_host(&uri.replace("udp://", ""))
         .await
         .map_err(|e| {
            error!(error = %e, tracker_uri = %uri, "Error looking up host for tracker");
            anyhow!(e)
         })?
         .filter(|addr| addr.is_ipv4()) // We dont support ipv6 yet
         .collect::<Vec<_>>();

      trace!(addrs = ?addrs, "DNS lookup successful");
      let addr = addrs.first().copied().ok_or_else(|| {
         error!(tracker_uri = %uri, "No IPv4 addresses found for tracker");
         anyhow!("No IPv4 addresses found for tracker: {}", uri)
      })?;

      // Create or use existing message receiver
      let server = match server {
         Some(receiver) => receiver,
         None => UdpServer::new(None).await,
      };

      debug!("UDP tracker instance created");

      Ok(UdpTracker {
         addr,
         uri,
         interval: Arc::new(AtomicU32::new(u32::MAX)),
         connection_id: Arc::new(AtomicU64::new(0)),
         server,
         ready_state: Arc::new(AtomicU8::new(ReadyState::Disconnected as u8)),
         peer_id,
         info_hash,
         peer_addr,
         stats: TrackerStats::default(),
         announce_params: Arc::new(RwLock::new(AnnounceParams::default())),
      })
   }

   pub fn set_connection_id(&self, id: u64) {
      self.connection_id.store(id, Ordering::Release);
   }

   pub fn get_connection_id(&self) -> u64 {
      self.connection_id.load(Ordering::Acquire)
   }

   /// Returns the local address of the tracker's [UdpServer].
   pub fn local_addr(&self) -> SocketAddr {
      self.server.local_addr()
   }

   pub fn interval(&self) -> u32 {
      self.interval.load(Ordering::Acquire)
   }

   pub fn set_interval(&self, interval: u32) {
      self.interval.store(interval, Ordering::Release)
   }

   pub fn stats(&self) -> TrackerStats {
      self.stats.clone()
   }

   /// Receives a message based off of a transaction ID and handles any errors
   /// appropriately. This is a helper function.
   #[instrument(skip(self), fields(
        tracker_uri = %self.uri,
        connection_id = ?self.get_connection_id()
   ))]
   async fn to_retry_error(
      &self, transaction_id: &TransactionId,
   ) -> anyhow::Result<TrackerResponse, RetryError<TrackerActorError>> {
      // Create a channel to receive the response
      let (response_tx, mut response_rx) = mpsc::unbounded_channel();

      // Register this transaction ID with the message receiver
      self
         .server
         .register_transaction(*transaction_id, response_tx)
         .await;

      trace!(transaction_id = transaction_id, "Waiting for response");

      // Create a timeout for the response
      let timeout_result = timeout(MESSAGE_TIMEOUT, response_rx.recv()).await;

      match timeout_result {
         Ok(Some((size, response))) => {
            trace!(
                transaction_id = transaction_id,
                response_type = %response,
                "Successfully received response"
            );
            self.stats.set_last_interaction();
            self.stats.increment_bytes_received(size);

            // Unregister the transaction ID
            self.server.unregister_transaction(transaction_id).await;
            Ok(response)
         }
         Ok(None) => {
            trace!(
               transaction_id = transaction_id,
               "Response channel closed before receiving message"
            );
            return Err(RetryError::Transient {
               err: TrackerActorError::RequestTimeout { seconds: 15 },
               retry_after: None,
            });
         }
         Err(_) => {
            trace!(
                transaction_id = transaction_id,
                elapsed = ?MESSAGE_TIMEOUT,
                "Tracker timed out"
            );
            return Err(RetryError::Transient {
               err: TrackerActorError::RequestTimeout { seconds: 15 },
               retry_after: None,
            });
         }
      }
   }

   /// Uses [`UdpServer::send_message`] to send a message and
   /// [`UdpTracker::to_retry_error`] to receive the message.
   #[instrument(skip(self, message), fields(
        tracker_uri = %self.uri,
        connection_id = ?self.get_connection_id()
   ))]
   async fn send_and_recv_retry(
      &self, message: &TrackerRequest, transaction_id: &TransactionId,
   ) -> anyhow::Result<TrackerResponse, RetryError<TrackerActorError>> {
      let message_bytes = message.to_bytes();

      // Send the message through the shared message receiver
      self
         .server
         .send_message(&message_bytes, self.addr)
         .await
         .map_err(RetryError::Permanent)?;
      self.stats.increment_bytes_sent(message_bytes.len());

      trace!(
          message = %message,
          transaction_id = transaction_id,
          "Sent message to tracker"
      );

      // Try to receive response
      self.to_retry_error(transaction_id).await
   }

   /// Sends a message with exponential backoff to a tracker. The following
   /// refers to the exponential backoff settings used in this function:
   ///
   /// ```
   /// let retry_strategy = ExponentialBackoff::from_millis(15 * 1000)
   ///    .factor(2)
   ///    .max_delay_millis(3840 * 1000)
   ///    .take(8);
   /// ```
   ///
   /// The following refers to the reasoning of why exponential backoff is
   /// utilized, from BEP 0015.
   ///
   /// > UDP is an 'unreliable' protocol. This means it doesn't retransmit lost
   /// > packets itself. The application is responsible for this. If a response
   /// > is
   /// > not received after 15 * 2 ^ n seconds, the client should retransmit
   /// > the request, where n starts at 0 and is increased up to 8 (3840
   /// > seconds) after every retransmission. Note that it is necessary to
   /// > rerequest a connection ID when it has expired.
   #[instrument(skip(self, message), fields(
        tracker_uri = %self.uri,
        connection_id = ?self.get_connection_id()
    ))]
   async fn send_and_wait(&self, message: TrackerRequest) -> Result<TrackerResponse> {
      let transaction_id = message.transaction_id();

      trace!(
         transaction_id = transaction_id,
         "Transaction ID for tracker"
      );
      let retry_strategy = ExponentialBackoff::from_millis(15 * 1000)
         .factor(2)
         .max_delay_millis(3840 * 1000)
         .take(8);

      let response = Retry::spawn_notify(
         retry_strategy,
         || self.send_and_recv_retry(&message, &transaction_id),
         |e, el| tracing::warn!(error = ?e, elapsed = ?el, "Tracker message failed, retrying...", ),
      )
      .await?;

      Ok(response)
   }

   #[instrument(skip(self), fields(
        tracker_uri = %self.uri,
        connection_id = ?self.get_connection_id()
    ))]
   async fn connect(&self) -> Result<ConnectionId> {
      let transaction_id: TransactionId = rand::random();
      trace!(transaction_id = transaction_id, "Preparing connect request");

      let request = TrackerRequest::Connect(MAGIC_CONSTANT, Action::Connect, transaction_id);
      let response = timeout(CONNECTION_TIMEOUT, self.send_and_wait(request)).await;

      let response = response.map_err(|_| TrackerActorError::RequestTimeout { seconds: 15 })??;

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

            self.set_connection_id(connection_id);
            self.set_ready_state(ReadyState::Ready);
            Ok(connection_id)
         }
         TrackerResponse::Error {
            message,
            transaction_id: _,
            ..
         } => {
            trace!(
                error_message = %message,
                transaction_id = transaction_id,
                "Tracker returned error for connect request"
            );
            Err(TrackerActorError::TrackerError { message })
         }
         _ => {
            trace!(
                response_type = ?response,
                transaction_id = transaction_id,
                "Unexpected response type to connect request"
            );
            Err(TrackerActorError::InvalidResponse {
               reason: "Expected connect response".to_string(),
            })
         }
      }
   }

   /// Gets the [ReadyState] enum for the tracker from an atomic u8.
   fn get_ready_state(&self) -> ReadyState {
      self.ready_state.load(Ordering::Acquire).try_into().unwrap()
   }

   /// Sets the [ReadyState] of the tracker. Even though the input to this
   /// function is an enum, it is stored as an atomic u8.
   fn set_ready_state(&self, state: ReadyState) {
      self.ready_state.store(state as u8, Ordering::Release);
   }
}

#[async_trait]
impl TrackerBase for UdpTracker {
   async fn initialize(&self) -> anyhow::Result<()> {
      self.connect().await?;
      Ok(())
   }

   /// Makes an announce request to a tracker. In other words, this function
   /// handles getting and receiving peers as well as updating the statistics
   /// for the tracker.
   #[instrument(skip(self), fields(
        tracker_uri = %self.uri,
        ready_state = ?self.ready_state,
        connection_id = ?self.get_connection_id()
    ))]
   async fn announce(&self) -> anyhow::Result<Vec<Peer>> {
      if self.get_ready_state() != ReadyState::Ready {
         error!(
             current_state = ?self.ready_state,
             "Tracker not ready for announce request"
         );
         return Err(anyhow!("Tracker not ready for announce request"));
      };

      self.stats.increment_announce_attempts();

      let transaction_id: TransactionId = rand::random();
      trace!(
         transaction_id = transaction_id,
         "Preparing announce request"
      );
      let params = self.announce_params.read().await;

      let request = TrackerRequest::Announce {
         connection_id: self.get_connection_id(),
         transaction_id,
         info_hash: self.info_hash,
         peer_id: self.peer_id,
         downloaded: params.downloaded,
         left: params.left as u64,
         uploaded: params.uploaded,
         event: params.event,
         ip_address: 0,
         key: 0,
         num_want: -1,
         port: self.peer_addr.port(),
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
               return Err(anyhow!(TrackerActorError::TransactionMismatch {
                  expected: transaction_id,
                  received: resp_tid
               }));
            }

            // Update statistics
            self.stats.increment_announce_successes();
            self.stats.increment_total_peers_received(peers.len());

            debug!(
               peers_count = peers.len(),
               interval_seconds = interval,
               swarm_leechers = leechers,
               swarm_seeders = seeders,
               "Announce completed successfully"
            );

            self.set_interval(interval);
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
            Err(anyhow!(TrackerActorError::TrackerError { message }))
         }
         _ => {
            error!(
                response_type = ?response,
                "Unexpected response type to announce request"
            );
            Err(anyhow!(TrackerActorError::InvalidResponse {
               reason: "Expected announce response".to_string(),
            }))
         }
      }
   }

   async fn update(&self, update: TrackerUpdate) -> anyhow::Result<()> {
      let mut params = self.announce_params.write().await;
      match update {
         TrackerUpdate::Uploaded(bytes) => {
            params.uploaded = bytes as u64;
         }
         TrackerUpdate::Downloaded(bytes) => {
            params.downloaded = bytes as u64;
         }
         TrackerUpdate::Left(bytes) => {
            params.left = bytes as u32;
         }
         TrackerUpdate::Event(event) => {
            params.event = event;
         }
      }
      Ok(())
   }

   fn stats(&self) -> TrackerStats {
      self.stats.clone()
   }

   fn interval(&self) -> usize {
      self.interval.load(Ordering::Acquire) as usize
   }
}

#[cfg(test)]
mod tests {
   use rand::random_range;

   use super::*;
   use crate::metainfo::{MagnetUri, MetaInfo};

   #[tokio::test]
   async fn test_stream_with_udp_peers() {
      tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .init();

      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");

      let contents = tokio::fs::read_to_string(&path).await.unwrap();
      let metainfo = MagnetUri::parse(contents).unwrap();

      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let info_hash = magnet.info_hash().expect("Missing info hash");
            let announce_list = magnet.announce_list.expect("Missing announce list");
            let uri = announce_list[0].uri();
            let port: u16 = random_range(1024..65535);
            let socket_addr = SocketAddr::from_str(&format!("0.0.0.0:{}", port)).unwrap();

            let tracker = UdpTracker::new(uri, None, info_hash, (PeerId::new(), socket_addr))
               .await
               .unwrap();

            tracker
               .initialize()
               .await
               .expect("Failed to connect to UDP tracker");

            // Spawn a task to re-fetch the latest list of peers at a given interval
            let peers = tracker.announce().await.expect("Failed to announce");

            let peer = &peers[0];
            assert!(peer.ip.is_ipv4());
         }
         _ => {
            panic!("Expected MagnetUri variant");
         }
      }
   }

   #[tokio::test]
   //#[traced_test]
   async fn test_multiple_trackers_single_udp_server() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");

      tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .init();

      let contents = tokio::fs::read_to_string(&path).await.unwrap();
      let metainfo = MagnetUri::parse(contents).unwrap();

      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let info_hash = magnet.info_hash().expect("Missing info hash");
            let announce_list = magnet.announce_list.expect("Missing announce list");

            // Create multiple tracker instances using the same socket
            let mut trackers = Vec::new();
            let mut tracker_urls = Vec::new();

            let udp_server = UdpServer::new(None).await;
            let peer_id = PeerId::new();

            // Use multiple tracker URLs from the announce list (or duplicate the same one)
            for announce_url in announce_list
               .iter()
               .filter(|t| t.uri().starts_with("udp://"))
            {
               let port: u16 = random_range(1024..65535);

               let tracker = announce_url
                  .to_instance(info_hash, peer_id, port, udp_server.clone())
                  .await;

               if let Ok(tracker) = tracker {
                  if tracker.initialize().await.is_ok() {
                     trackers.push(tracker);
                     tracker_urls.push(announce_url.uri().clone());
                  } else {
                     error!("There was an error creating the tracker");
                  }
               }
            }

            // Test that all trackers can fetch peers concurrently using the shared socket
            let mut handles = Vec::new();
            let tracker_len = trackers.len();

            for (i, tracker) in trackers.into_iter().enumerate() {
               let handle = tokio::spawn(async move {
                  println!("Tracker {} starting peer fetch", i);

                  let mut peers = vec![];

                  if let Ok(res) = timeout(Duration::from_secs(1), tracker.announce()).await
                     && let Ok(wrapped_peers) = res
                  {
                     peers.extend(wrapped_peers);
                  }

                  if !peers.is_empty() {
                     println!("Tracker {} received {} peers", i, peers.len());
                     (i, Ok(peers))
                  } else {
                     println!("Tracker {} failed", i);
                     (
                        i,
                        Err("Tracker failed (no peers, or something else failed)"),
                     )
                  }
               });
               handles.push(handle);
            }

            // Wait for all trackers to complete
            let results = DashMap::new();
            for handle in handles {
               let (tracker_id, result) = handle.await.expect("Task panicked");
               results.insert(tracker_id, result);
            }

            // Verify results
            let mut successful_trackers = 0;
            let mut total_peers = 0;

            for (tracker_id, result) in results {
               match result {
                  Ok(peers) => {
                     successful_trackers += 1;
                     total_peers += peers.len();
                     println!(
                        "Tracker {} succeeded with {} peers",
                        tracker_id,
                        peers.len()
                     );

                     // Verify peer format
                     for peer in &peers {
                        assert!(peer.ip.is_ipv4(), "Expected IPv4 peer address");
                        assert!(peer.port > 0, "Expected valid port number");
                     }
                  }
                  Err(e) => {
                     println!("Tracker {} failed: {}", tracker_id, e);
                  }
               }
            }

            // At least one tracker should succeed
            assert!(
               successful_trackers > 0,
               "Expected at least one tracker to succeed, but all failed"
            );

            println!(
               "Test completed: {}/{} trackers succeeded, {} total peers received",
               successful_trackers, tracker_len, total_peers
            );
         }
         _ => {
            panic!("Expected MagnetUri variant");
         }
      }
   }
}
