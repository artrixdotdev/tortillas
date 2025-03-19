use std::{net::Ipv4Addr, sync::Arc};

/// UDP protocol
/// https://en.wikipedia.org/wiki/User_Datagram_Protocol
///
/// Please see the following for the UDP *tracker* protocol spec.
/// https://www.bittorrent.org/beps/bep_0015.html
/// https://xbtt.sourceforge.net/udp_tracker_protocol.html
use num_enum::TryFromPrimitive;
use rand::RngCore;
use serde_repr::{Deserialize_repr, Serialize_repr};
use tokio::net::UdpSocket;
use tracing::{debug, error, info, instrument, trace, warn};

use super::{PeerAddr, TrackerTrait};
use crate::{
   errors::{TrackerError, UdpTrackerError},
   hashes::InfoHash,
};

/// Types and constants
type ConnectionId = u64;
type TransactionId = u32;
type Result<T> = std::result::Result<T, UdpTrackerError>;

const MAGIC_CONSTANT: ConnectionId = 0x41727101980;
const MIN_CONNECT_RESPONSE_SIZE: usize = 16;
const MIN_ANNOUNCE_RESPONSE_SIZE: usize = 20;
const MIN_ERROR_RESPONSE_SIZE: usize = 8;
const PEER_SIZE: usize = 6;

/// Enum for UDP Tracker Protocol Action parameter. See this resource for more information: https://xbtt.sourceforge.net/udp_tracker_protocol.html
#[derive(Debug, Serialize_repr, Deserialize_repr, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u32)]
pub enum Action {
   Connect = 0u32,
   Announce = 1u32,
   Scrape = 2u32,
   Error = 3u32,
}

/// Enum for UDP Tracker Protocol Events parameter. See this resource for more information: https://xbtt.sourceforge.net/udp_tracker_protocol.html
#[derive(Debug, Serialize_repr, Deserialize_repr, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
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
      peer_id: [u8; 20],
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
   /// Note that the response headers for TrackerResponse are somewhat different in comparison to TrackerRequest:
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
      peers: Vec<super::PeerAddr>,
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

/// Formats the headers for a request in the UDP Tracker Protocol
impl TrackerRequest {
   pub fn to_bytes(&self) -> Vec<u8> {
      let mut buf = Vec::new();
      match self {
         TrackerRequest::Connect(id, action, transaction_id) => {
            buf.extend_from_slice(&id.to_be_bytes()); // Magic constant
            buf.extend_from_slice(&(*action as u32).to_be_bytes()); // Action
            buf.extend_from_slice(&transaction_id.to_be_bytes()); // Transaction ID
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
            buf.extend_from_slice(&connection_id.to_be_bytes()); // Connection ID
            buf.extend_from_slice(&(Action::Announce as u32).to_be_bytes()); // Action
            buf.extend_from_slice(&transaction_id.to_be_bytes()); // Transaction ID
            buf.extend_from_slice(info_hash.as_bytes()); // Info Hash
            buf.extend_from_slice(peer_id); // Peer ID
            buf.extend_from_slice(&downloaded.to_be_bytes()); // Downloaded
            buf.extend_from_slice(&left.to_be_bytes()); // Left
            buf.extend_from_slice(&uploaded.to_be_bytes()); // Uploaded
            buf.extend_from_slice(&(*event as u32).to_be_bytes()); // Event
            buf.extend_from_slice(&ip_address.to_be_bytes()); // IP Address
            buf.extend_from_slice(&key.to_be_bytes()); // Key
            buf.extend_from_slice(&num_want.to_be_bytes()); // Num Want
            buf.extend_from_slice(&port.to_be_bytes()); // Port
         }
      }
      buf
   }
}

/// Accepts a response (in bytes) from a UDP [tracker request](TrackerRequest).
impl TrackerResponse {
   pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
      if bytes.len() < 4 {
         return Err(UdpTrackerError::ResponseTooShort {
            expected: 4,
            actual: bytes.len(),
         });
      }

      let action = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
      let action: Action =
         Action::try_from(action).map_err(|e| TrackerError::InvalidAction(e.number))?;

      match action {
         Action::Connect => {
            if bytes.len() < MIN_CONNECT_RESPONSE_SIZE {
               return Err(UdpTrackerError::ResponseTooShort {
                  expected: MIN_CONNECT_RESPONSE_SIZE,
                  actual: bytes.len(),
               });
            }
            let transaction_id = TransactionId::from_be_bytes(bytes[4..8].try_into().unwrap());
            let connection_id = ConnectionId::from_be_bytes(bytes[8..16].try_into().unwrap());

            trace!(
               action = ?action,
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
            // Subtract the size of the current bytes we've already dealt with (20) from the total length of the bytes, then divide by the size of a peer ip (6 bytes)
            let num_peers = (bytes.len() - 20) / PEER_SIZE;

            trace!(
               action = ?action,
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

               trace!(
                  peer_index = i,
                  ip = %ip,
                  port = port,
                  "Parsed peer address"
               );

               peers.push(PeerAddr { ip, port });
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
         _ => Err(UdpTrackerError::InvalidResponse(format!(
            "Unsupported action: {:?}",
            action
         ))),
      }
   }
}

#[derive(Debug, PartialEq, Eq)]
enum ReadyState {
   Connected,
   Ready,
   Disconnected,
}

pub struct UdpTracker {
   uri: String,
   connection_id: Option<ConnectionId>,
   pub socket: Arc<UdpSocket>,
   ready_state: ReadyState,
   peer_id: [u8; 20],
   info_hash: InfoHash,
}

impl UdpTracker {
   #[instrument(skip(socket, info_hash), fields(uri = %uri))]
   pub async fn new(
      uri: String,
      socket: Option<UdpSocket>,
      info_hash: InfoHash,
   ) -> Result<UdpTracker> {
      debug!("Creating new UDP tracker");
      let sock = match socket {
         Some(sock) => {
            debug!("Using provided socket");
            sock
         }
         None => {
            debug!("Creating new socket on 0.0.0.0:0");
            UdpSocket::bind("0.0.0.0:0").await.map_err(|e| {
               UdpTrackerError::ConnectionFailed(format!("Failed to bind socket: {}", e))
            })?
         }
      };

      let mut peer_id = [0u8; 20];
      rand::rng().fill_bytes(&mut peer_id);
      debug!(peer_id = ?peer_id, "Generated peer ID");

      Ok(UdpTracker {
         uri,
         connection_id: None,
         socket: Arc::new(sock),
         ready_state: ReadyState::Disconnected,
         peer_id,
         info_hash,
      })
   }

   #[instrument(skip(self))]
   async fn announce(&self) -> Result<Vec<PeerAddr>> {
      if self.ready_state != ReadyState::Ready {
         return Err(UdpTrackerError::Tracker(TrackerError::NotReady(
            "Tracker not ready for announce request".to_string(),
         )));
      };

      let transaction_id: TransactionId = rand::random();
      debug!(
         transaction_id = transaction_id,
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
         port: 6881,
      };

      trace!("Sending announce request");
      self.socket.send(&request.to_bytes()).await.map_err(|e| {
         UdpTrackerError::ConnectionFailed(format!("Failed to send announce request: {}", e))
      })?;

      let mut buf = Vec::new();
      trace!("Waiting for announce response");

      self.socket.recv_buf_from(&mut buf).await.map_err(|e| {
         UdpTrackerError::ConnectionFailed(format!("Failed to receive announce response: {}", e))
      })?;

      debug!(response_size = buf.len(), "Received announce response");
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

            info!(
               peers_count = peers.len(),
               interval = interval,
               leechers = leechers,
               seeders = seeders,
               "Announce successful"
            );

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

            Err(UdpTrackerError::TrackerMessage(message))
         }
         _ => {
            error!("Unexpected response type to announce request");
            Err(UdpTrackerError::InvalidResponse(
               "Expected announce response".to_string(),
            ))
         }
      }
   }
}

impl TrackerTrait for UdpTracker {
   // Makes a request using the UDP tracker protocol to connect. Returns a u64 connection ID
   #[instrument(skip(self))]
   async fn stream_peers(&mut self) -> std::result::Result<Vec<PeerAddr>, anyhow::Error> {
      let uri = self.uri.replace("udp://", "");
      debug!(target_uri = %uri, "Connecting to tracker");

      self.socket.connect(&uri).await.map_err(|e| {
         UdpTrackerError::ConnectionFailed(format!("Failed to connect to tracker: {}", e))
      })?;

      debug!("Socket connected");
      self.ready_state = ReadyState::Connected;

      let transaction_id: TransactionId = rand::random();
      debug!(transaction_id = transaction_id, "Preparing connect request");

      // Send
      let request = TrackerRequest::Connect(MAGIC_CONSTANT, Action::Connect, transaction_id);
      trace!("Sending connect request");

      // Send the request
      self.socket.send(&request.to_bytes()).await.map_err(|e| {
         UdpTrackerError::ConnectionFailed(format!("Failed to send connect request: {}", e))
      })?;

      // Receive response
      let mut buffer = Vec::new();
      trace!("Waiting for connect response");

      self.socket.recv_buf(&mut buffer).await.map_err(|e| {
         UdpTrackerError::ConnectionFailed(format!("Failed to receive connect response: {}", e))
      })?;

      debug!(response_size = buffer.len(), "Received connect response");

      // Parse response
      let response = TrackerResponse::from_bytes(&buffer)?;

      // Check response
      match response {
         TrackerResponse::Connect {
            connection_id,
            transaction_id: resp_tid,
            ..
         } => {
            // Transaction ID's have to be the same per request. If I send a request to the tracker,
            // the tracker should respond with the same transaction ID. These should be unique per request though.
            if resp_tid != transaction_id {
               error!(
                  expected_tid = transaction_id,
                  received_tid = resp_tid,
                  "Transaction ID mismatch in connect response"
               );
               return Err(UdpTrackerError::Tracker(TrackerError::TransactionMismatch).into());
            }

            debug!(
               connection_id = connection_id,
               "Received valid connection ID"
            );
            self.connection_id = Some(connection_id);
            info!("Tracker is ready");
            self.ready_state = ReadyState::Ready;

            Ok(self.announce().await?)
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

            error!(error_message = %message, "Tracker returned error");
            Err(UdpTrackerError::TrackerMessage(message).into())
         }
         _ => {
            error!("Unexpected response type to connect request");
            Err(UdpTrackerError::InvalidResponse("Expected connect response".to_string()).into())
         }
      }
   }
}

#[cfg(test)]
mod tests {
   use super::*;
   use crate::parser::{MagnetUri, MetaInfo};
   use tracing_test::traced_test;

   #[tokio::test]
   #[traced_test]
   async fn test_stream_with_udp_peers() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MagnetUri::parse(contents).await.unwrap();

      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let info_hash = magnet.info_hash();
            let announce_list = magnet.announce_list.unwrap();
            let announce_url = announce_list[0].uri();

            let mut udp_tracker = UdpTracker::new(announce_url, None, info_hash)
               .await
               .unwrap();
            let stream = udp_tracker.stream_peers().await.unwrap();

            let peer = &stream[0];
            assert!(!peer.ip.is_private())
         }
         _ => panic!("Expected Torrent"),
      }
   }
}
