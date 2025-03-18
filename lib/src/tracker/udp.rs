use std::{net::Ipv4Addr, sync::Arc};

/// UDP protocol
/// https://en.wikipedia.org/wiki/User_Datagram_Protocol
///
/// Please see the following for the UDP *tracker* protocol spec.
/// https://www.bittorrent.org/beps/bep_0015.html
/// https://xbtt.sourceforge.net/udp_tracker_protocol.html
use anyhow::{Result, anyhow};
use num_enum::TryFromPrimitive;
use rand::RngCore;
use serde_repr::{Deserialize_repr, Serialize_repr};
use tokio::net::UdpSocket;
use tracing::{debug, info, trace};

use super::{PeerAddr, TrackerTrait};

/// Types and constants
type ConnectionId = u64;

const MAGIC_CONSTANT: ConnectionId = 0x41727101980;

type TransactionId = u32;

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
   /// - [Info Hash] (20 bytes)
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
      info_hash: [u8; 20],
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
            buf.extend_from_slice(info_hash); // Info Hash
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
         return Err(anyhow!("Response too short"));
      }

      let action = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
      let action: Action = Action::try_from(action)?;

      match action {
         Action::Connect => {
            if bytes.len() < 16 {
               return Err(anyhow!("Connect response too short"));
            }
            let transaction_id = TransactionId::from_be_bytes(bytes[4..8].try_into().unwrap());
            let connection_id = ConnectionId::from_be_bytes(bytes[8..16].try_into().unwrap());
            Ok(TrackerResponse::Connect {
               action,
               connection_id,
               transaction_id,
            })
         }
         Action::Announce => {
            if bytes.len() < 20 {
               return Err(anyhow!("Announce response too short"));
            }
            let transaction_id = TransactionId::from_be_bytes(bytes[4..8].try_into().unwrap());
            let interval = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
            let leechers = u32::from_be_bytes(bytes[12..16].try_into().unwrap());
            let seeders = u32::from_be_bytes(bytes[16..20].try_into().unwrap());

            // Parse peers (each peer is 6 bytes: 4 for IP, 2 for port)
            let mut peers = Vec::new();
            const PEER_SIZE: usize = 6;
            // Subtract the size of the current bytes we've already dealt with (20) from the total length of the bytes, then divide by the size of a peer ip (6 bytes)
            let num_peers = (bytes.len() - 20) / PEER_SIZE;

            for i in 0..num_peers {
               let offset = 20 + i * PEER_SIZE;
               if offset + PEER_SIZE > bytes.len() {
                  break;
               }

               let ip_bytes: [u8; 4] = bytes[offset..offset + 4].try_into().unwrap();
               let ip = Ipv4Addr::from(ip_bytes);

               let port_bytes: [u8; 2] = bytes[offset + 4..offset + PEER_SIZE].try_into().unwrap();
               let port = u16::from_be_bytes(port_bytes);

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
            if bytes.len() < 8 {
               return Err(anyhow!("Error response too short"));
            }
            let transaction_id = TransactionId::from_be_bytes(bytes[4..8].try_into().unwrap());
            let message = String::from_utf8_lossy(&bytes[8..]).to_string();

            Ok(TrackerResponse::Error {
               action,
               transaction_id,
               message,
            })
         }
         _ => Err(anyhow!("Unsupported action: {:?}", action)),
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
   info_hash: [u8; 20],
}

impl UdpTracker {
   pub async fn new(
      uri: String,
      socket: Option<UdpSocket>,
      info_hash: [u8; 20],
   ) -> Result<UdpTracker> {
      debug!("Creating new UDP tracker for {}", uri);
      let sock = match socket {
         Some(sock) => sock,
         None => UdpSocket::bind("0.0.0.0:0").await?,
      };
      let mut peer_id = [0u8; 20];
      rand::rng().fill_bytes(&mut peer_id);
      debug!("Peer ID: {:?}", peer_id);

      Ok(UdpTracker {
         uri,
         connection_id: None,
         socket: Arc::new(sock),
         ready_state: ReadyState::Disconnected,
         peer_id,
         info_hash,
      })
   }
   async fn announce(&self) -> Result<Vec<PeerAddr>> {
      if self.ready_state != ReadyState::Ready {
         return Err(anyhow!("Tracker not ready"));
      };

      let transaction_id: TransactionId = rand::random();

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

      self.socket.send(&request.to_bytes()).await?;
      trace!("Sent announce request to {}", self.uri);
      let mut buf = Vec::new();

      self.socket.recv_buf_from(&mut buf).await?;

      let response = TrackerResponse::from_bytes(&buf)?;
      debug!("Received announce from {}", self.uri);
      match response {
         TrackerResponse::Announce { peers, .. } => {
            debug!("Found {} peers from {}", peers.len(), self.uri);
            Ok(peers)
         }
         _ => Err(anyhow!("Unexpected response")),
      }
   }
}

impl TrackerTrait for UdpTracker {
   // Makes a request using the UDP tracker protocol to connect. Returns a u64 connection ID
   async fn stream_peers(&mut self) -> Result<Vec<PeerAddr>> {
      let uri = self.uri.replace("udp://", "");
      self.socket.connect(&uri).await?;

      debug!("Connected to tracker {}", self.uri);

      self.ready_state = ReadyState::Connected;
      let transaction_id: TransactionId = rand::random();

      // Send
      let request = TrackerRequest::Connect(MAGIC_CONSTANT, Action::Connect, transaction_id);
      trace!("Sending connect request to {}", self.uri);

      // Send the request
      self.socket.send(&request.to_bytes()).await?;

      // Receive response
      let mut buffer = Vec::new();
      self.socket.recv_buf(&mut buffer).await?;
      trace!("Received connect response from {}", self.uri);

      // Parse response
      let response = TrackerResponse::from_bytes(&buffer)?;

      // Check response
      match response {
         TrackerResponse::Connect {
            connection_id,
            transaction_id: tid,
            ..
         } => {
            // Transaction ID's have to be the same per request. If I send a request to the tracker,
            // the tracker should respond with the same transaction ID. These should be unique per request though.
            if tid != transaction_id {
               return Err(anyhow!("Transaction ID mismatch"));
            }
            self.connection_id = Some(connection_id);
            info!("Tracker {} is ready", self.uri);
            self.ready_state = ReadyState::Ready;
            self.announce().await
         }
         _ => Err(anyhow!("Invalid Response")),
      }
   }
}
// UNCOMMENT WHEN TRACKER REQUEST RETRIES ARE IMPLEMENTED
// BREAKS GITHUB ACTIONS AND LEAVES A HANGING RESPONSE IF THE TRACKER'S PACKET GETS DROPPED
#[cfg(test)]
mod tests {

   use crate::parser::{MagnetUri, MetaInfo};

   use super::*;

   #[tokio::test]
   async fn test_stream_with_udp_peers() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MagnetUri::parse(contents).await.unwrap();

      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let announce_list = magnet.announce_list.unwrap();
            let announce_url = announce_list[0].uri();
            let info_hash: [u8; 20] = hex::decode(magnet.info_hash.split(':').last().unwrap())
               .unwrap()
               .try_into()
               .unwrap();

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
