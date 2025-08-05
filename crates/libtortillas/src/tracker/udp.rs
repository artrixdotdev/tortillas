use std::{
   collections::HashMap,
   fmt::{Debug, Display},
   net::{Ipv4Addr, SocketAddr},
   str::FromStr,
   sync::Arc,
};

use anyhow::anyhow;
use async_trait::async_trait;
use num_enum::TryFromPrimitive;
use serde_repr::{Deserialize_repr, Serialize_repr};
use tokio::{
   net::{UdpSocket, lookup_host},
   sync::{Mutex, mpsc},
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
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(3);
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
/// Centralized message receiver that broadcasts messages to all trackers
#[derive(Clone, Debug)]
pub struct UdpServer {
   /// Map of transaction IDs to sender channels for routing responses
   response_channels:
      Arc<Mutex<HashMap<TransactionId, mpsc::UnboundedSender<(usize, TrackerResponse)>>>>,
   /// Shared socket for all trackers
   socket: Arc<UdpSocket>,
}

impl UdpServer {
   pub async fn new(addr: Option<SocketAddr>) -> Self {
      let addr = addr.unwrap_or(SocketAddr::from_str("0.0.0.0:0").unwrap());
      let socket = Arc::new(UdpSocket::bind(addr).await.unwrap());

      let receiver = UdpServer {
         response_channels: Arc::new(Mutex::new(HashMap::new())),
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
      let mut response_channels = self.response_channels.lock().await;
      response_channels.insert(transaction_id, sender);
      trace!(transaction_id = transaction_id, "Registered transaction");
   }

   /// Unregister a transaction ID
   pub async fn unregister_transaction(&self, transaction_id: &TransactionId) {
      let mut response_channels = self.response_channels.lock().await;
      response_channels.remove(transaction_id);
      trace!(transaction_id = transaction_id, "Unregistered transaction");
   }

   /// Start the background task that receives messages and routes them to
   /// correct channels
   async fn start_receiver_task(&self) {
      let receiver = self.clone();
      tokio::spawn(async move {
         // I HATE THIS LINE OF CODE!!!
         //
         // We are literally forced to waste memory here (???).
         let mut buf = vec![0u8; 65536]; // Buffer for incoming messages
         loop {
            match receiver.socket.recv_from(&mut buf).await {
               Ok((size, addr)) => {
                  trace!(size = size, addr = %addr, "Received UDP message");

                  // Parse the response
                  match TrackerResponse::from_bytes(&buf[..size]) {
                     Ok(response) => {
                        // Extract transaction ID and route to correct channel
                        let transaction_id = match &response {
                           TrackerResponse::Connect { transaction_id, .. } => *transaction_id,
                           TrackerResponse::Announce { transaction_id, .. } => *transaction_id,
                           TrackerResponse::Error { transaction_id, .. } => *transaction_id,
                        };

                        trace!(
                            transaction_id = transaction_id,
                            response_type = ?response,
                            "Routing response to transaction channel"
                        );

                        // Send to the specific transaction channel
                        {
                           let response_channels = receiver.response_channels.lock().await;
                           if let Some(sender) = response_channels.get(&transaction_id) {
                              if let Err(e) = sender.send((size, response)) {
                                 warn!(
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
                              warn!(
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
   pub async fn send_message(&self, message: &[u8], addr: SocketAddr) -> Result<()> {
      self.socket.send_to(message, addr).await.map_err(|e| {
         error!(error = ?e, "Failed to send message to tracker");
         UdpTrackerError::MessageTimeout
      })?;

      trace!(size = message.len(), addr = %addr, "Sent UDP message");
      Ok(())
   }
}
#[derive(Clone)]
pub struct UdpTracker {
   /// Raw SocketAddr for the tracker
   addr: SocketAddr,
   uri: String,
   server: UdpServer,
   connection_id: Option<ConnectionId>,
   ready_state: ReadyState,
   pub peer_id: PeerId,
   info_hash: InfoHash,
   ///  The address that our TCP or uTP socket is bound to
   peer_addr: SocketAddr,
   interval: u32,
   stats: TrackerStats,
}

impl UdpTracker {
   #[instrument(skip(info_hash), fields(
        uri = %uri,
        info_hash = %info_hash,
    ))]
   pub async fn new(
      uri: String, server: Option<UdpServer>, info_hash: InfoHash, peer_info: (PeerId, SocketAddr),
   ) -> Result<UdpTracker> {
      let (peer_id, peer_addr) = peer_info;
      let addrs = lookup_host(&uri.replace("udp://", ""))
         .await
         .map_err(|e| {
            error!(error = %e, "Error looking up host for tracker");
            anyhow!(e)
         })?
         .filter(|addr| addr.is_ipv4()) // We dont support ipv6 yet
         .collect::<Vec<_>>();

      trace!("Resolved addresses: {:?}", addrs);
      let addr = *addrs.first().unwrap();

      // Create or use existing message receiver
      let server = match server {
         Some(receiver) => receiver,
         None => UdpServer::new(None).await,
      };

      debug!("UDP tracker instance created");

      Ok(UdpTracker {
         addr,
         uri,
         interval: u32::MAX,
         connection_id: None,
         server,
         ready_state: ReadyState::Disconnected,
         peer_id,
         info_hash,
         peer_addr,
         stats: TrackerStats::default(),
      })
   }

   pub fn local_addr(&self) -> SocketAddr {
      self.server.local_addr()
   }

   #[instrument(skip(self), fields(
        tracker_uri = %self.uri,
        connection_id = ?self.connection_id
   ))]
   async fn recv_retry(
      &self, transaction_id: &TransactionId,
   ) -> anyhow::Result<TrackerResponse, RetryError<UdpTrackerError>> {
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
                response_type = ?response,
                "Successfully received response"
            );
            self.stats.set_last_interaction();
            self.stats.increment_bytes_received(size);

            // Unregister the transaction ID
            self.server.unregister_transaction(transaction_id).await;
            Ok(response)
         }
         Ok(None) => {
            warn!(
               transaction_id = transaction_id,
               "Response channel closed before receiving message"
            );
            return Err(RetryError::Transient {
               err: UdpTrackerError::MessageTimeout,
               retry_after: None,
            });
         }
         Err(_) => {
            warn!(
                transaction_id = transaction_id,
                elapsed = ?MESSAGE_TIMEOUT,
                "Tracker timed out"
            );
            return Err(RetryError::Transient {
               err: UdpTrackerError::MessageTimeout,
               retry_after: None,
            });
         }
      }
   }

   #[instrument(skip(self), fields(
        tracker_uri = %self.uri,
        connection_id = ?self.connection_id
   ))]
   async fn send_and_recv_retry(
      &self, message: &TrackerRequest, transaction_id: &TransactionId,
   ) -> anyhow::Result<TrackerResponse, RetryError<UdpTrackerError>> {
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
      self.recv_retry(transaction_id).await
   }

   #[instrument(skip(self), fields(
        tracker_uri = %self.uri,
        connection_id = ?self.connection_id
    ))]
   async fn send_and_wait(&self, message: TrackerRequest) -> Result<TrackerResponse> {
      let transaction_id = match &message {
         TrackerRequest::Connect(_, _, tid) => *tid,
         TrackerRequest::Announce { transaction_id, .. } => *transaction_id,
      };

      trace!(
         transaction_id = transaction_id,
         "Transaction ID for tracker"
      );

      // UDP is an 'unreliable' protocol. This means it doesn't retransmit lost
      // packets itself. The application is responsible for this. If a response is not
      // received after 15 * 2 ^ n seconds, the client should retransmit the request,
      // where n starts at 0 and is increased up to 8 (3840 seconds) after every
      // retransmission. Note that it is necessary to rerequest a connection ID when
      // it has expired.
      //
      // From BEP 0015
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
        connection_id = ?self.connection_id
    ))]
   async fn connect(&mut self) -> Result<ConnectionId> {
      let transaction_id: TransactionId = rand::random();
      trace!(transaction_id = transaction_id, "Preparing connect request");

      let request = TrackerRequest::Connect(MAGIC_CONSTANT, Action::Connect, transaction_id);
      let response = timeout(CONNECTION_TIMEOUT, self.send_and_wait(request)).await;

      let response = response.map_err(|_| UdpTrackerError::MessageTimeout)??;

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
        tracker_uri = %self.uri,
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

      self.stats.increment_announce_attempts();

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
               return Err(UdpTrackerError::Tracker(TrackerError::TransactionMismatch));
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
}

#[async_trait]
impl TrackerTrait for UdpTracker {
   #[instrument(skip(self), fields(
        tracker_uri = %self.uri,
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

         loop {
            iteration += 1;

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
        tracker_uri = %self.uri,
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

   use super::*;
   use crate::metainfo::{MagnetUri, MetaInfo};

   #[tokio::test]
   // #[traced_test]
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
            let announce_url = &announce_list[0].uri();
            let port: u16 = random_range(1024..65535);
            let socket_addr = SocketAddr::from(([0, 0, 0, 0], port));

            let mut udp_tracker = UdpTracker::new(
               announce_url.clone(),
               None,
               info_hash,
               (PeerId::new(), socket_addr),
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

            // Use multiple tracker URLs from the announce list (or duplicate the same one)
            for announce_url in announce_list
               .iter()
               .filter(|t| t.uri().starts_with("udp://"))
            {
               let port: u16 = random_range(1024..65535);
               let socket_addr = SocketAddr::from(([0, 0, 0, 0], port));

               let tracker = UdpTracker::new(
                  announce_url.uri().clone(),
                  Some(udp_server.clone()),
                  info_hash,
                  (PeerId::new(), socket_addr),
               )
               .await;

               if tracker.is_err() {
                  // AFAIK, if new() fails, something most likely went wrong with the DNS lookup.
                  // We don't want to completely panic! if one tracker fails though.
                  let _ = tracker.map_err(|e| error!(error = %e, "Failed to create UDP tracker"));
               } else {
                  trackers.push(tracker.unwrap());
                  tracker_urls.push(announce_url.uri().clone());
               }
            }

            // If we don't have enough unique trackers, duplicate the first one
            while trackers.len() < 2 {
               let port: u16 = random_range(1024..65535);
               let socket_addr = SocketAddr::from(([0, 0, 0, 0], port));

               let tracker = UdpTracker::new(
                  tracker_urls[0].clone(),
                  Some(udp_server.clone()),
                  info_hash,
                  (PeerId::new(), socket_addr),
               )
               .await
               .expect("Failed to create duplicate UDP tracker");

               trackers.push(tracker);
            }

            // Test that all trackers can fetch peers concurrently using the shared socket
            let mut handles = Vec::new();
            let tracker_len = trackers.len();

            for (i, mut tracker) in trackers.into_iter().enumerate() {
               let handle = tokio::spawn(async move {
                  println!("Tracker {} starting peer fetch", i);

                  match tracker.get_peers().await {
                     Ok(peers) => {
                        println!("Tracker {} received {} peers", i, peers.len());
                        (i, Ok(peers))
                     }
                     Err(e) => {
                        println!("Tracker {} failed: {}", i, e);
                        (i, Err(e))
                     }
                  }
               });
               handles.push(handle);
            }

            // Wait for all trackers to complete
            let mut results = HashMap::new();
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
