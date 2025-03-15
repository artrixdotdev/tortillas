use std::sync::Arc;

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

use super::TrackerTrait;

/// Types and constants
pub type ConnectionId = u64;

pub const MAGIC_CONSTANT: ConnectionId = 0x41727101980;

pub type TransactionId = u32;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
      }
      buf
   }
}

/// Accepts a response (in bytes) from a UDP [tracker request](TrackerRequest).
impl TrackerResponse {
   pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
      println!("Received bytes: {:?}", bytes);
      let action = u32::from_be_bytes(bytes[0..4].try_into().unwrap());

      let action: Action = Action::try_from(action)?;
      println!("{:?}", &bytes[4..8]);

      match action {
         Action::Connect => {
            let transaction_id = TransactionId::from_be_bytes(bytes[4..8].try_into().unwrap());
            let connection_id = ConnectionId::from_be_bytes(bytes[8..16].try_into().unwrap());
            Ok(TrackerResponse::Connect {
               action,
               connection_id,
               transaction_id,
            })
         }
         _ => Err(anyhow!("Invalid action")),
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
}

impl UdpTracker {
   ///
   pub async fn new(uri: String, socket: Option<UdpSocket>) -> Result<UdpTracker> {
      let sock = match socket {
         Some(sock) => sock,
         None => UdpSocket::bind("0.0.0.0:0").await?,
      };

      Ok(UdpTracker {
         uri,
         connection_id: None,
         socket: Arc::new(sock),
         ready_state: ReadyState::Disconnected,
      })
   }
}

impl TrackerTrait for UdpTracker {
   // Makes a request using the UDP tracker protocol to connect. Returns a u64 connection ID
   async fn stream_peers(&mut self, info_hash: String) -> Result<u64> {
      let uri = self.uri.replace("udp://", "");
      self.socket.connect(&uri).await?;

      self.ready_state = ReadyState::Connected;
      let transaction_id: TransactionId = rand::random();

      // Send
      let request = TrackerRequest::Connect(MAGIC_CONSTANT, Action::Connect, transaction_id);

      // Send the request
      self.socket.send_to(&request.to_bytes(), &uri).await?;

      // Receive response
      let mut buffer = Vec::new();
      self.socket.recv_buf_from(&mut buffer).await?;

      // Parse response
      let response = TrackerResponse::from_bytes(buffer.into())?;

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
            self.ready_state = ReadyState::Ready;

            Ok(connection_id)
         }
         _ => Err(anyhow!("Invalid Response")),
      }
   }
}

#[cfg(test)]
mod tests {
   use std::time::Duration;

   use tokio::time::sleep;

   use crate::parser::{MagnetUri, MetaInfo};

   use super::*;

   #[tokio::test]
   async fn test_parse_magnet_uri() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MagnetUri::parse(contents).await.unwrap();

      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let announce_list = magnet.announce_list.unwrap();
            let announce_url = announce_list[0].uri();
            let mut udp_tracker = UdpTracker::new(announce_url, None).await.unwrap();
            udp_tracker.connect().await.unwrap();

            assert_eq!(udp_tracker.ready_state, ReadyState::Ready);
         }
         _ => panic!("Expected Torrent"),
      }
   }
}
