use anyhow::Result;
use async_trait::async_trait;
use messages::{Handshake, PeerMessages, MAGIC_STRING};
use std::{
   fmt::Display,
   net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
   sync::Arc,
   time::Duration,
};
use tokio::{
   sync::mpsc::{self, Receiver, Sender},
   time::{timeout, Instant},
};
use tracing::{error, trace};
use transport_messages::{TransportCommand, TransportResponse};

use crate::{
   errors::PeerTransportError,
   hashes::{Hash, InfoHash},
};
pub mod messages;
pub mod tcp;
pub mod transport_messages;
pub mod utp;
pub type PeerKey = SocketAddr;

/// Represents a BitTorrent peer with connection state and statistics
/// Download rate and upload rate are measured in kilobytes per second.
#[derive(Eq, PartialEq, Hash, Debug, Clone)]
pub struct Peer {
   pub ip: IpAddr,
   pub port: u16,
   pub choked: bool,
   pub interested: bool,
   pub am_choking: bool,
   pub am_interested: bool,
   pub download_rate: u64,
   pub upload_rate: u64,
   pub pieces: Vec<bool>,
   pub last_optimistic_unchoke: Option<Instant>,
   pub id: Option<Hash<20>>,
   pub last_message_sent: Option<Instant>,
   pub last_message_received: Option<Instant>,
   pub bytes_downloaded: u64,
   pub bytes_uploaded: u64,
}

impl Peer {
   /// Create a new peer with the given IP address and port
   pub fn new(ip: IpAddr, port: u16) -> Self {
      Peer {
         ip,
         port,
         choked: true,
         interested: false,
         am_choking: true,
         am_interested: false,
         download_rate: 0,
         upload_rate: 0,
         pieces: vec![],
         last_optimistic_unchoke: None,
         id: None,
         last_message_received: None,
         last_message_sent: None,
         bytes_downloaded: 0,
         bytes_uploaded: 0,
      }
   }

   /// Create a new peer from an IPv4 address and port
   pub fn from_ipv4(ip: Ipv4Addr, port: u16) -> Self {
      Self::new(IpAddr::V4(ip), port)
   }

   /// Create a new peer from an IPv6 address and port
   pub fn from_ipv6(ip: Ipv6Addr, port: u16) -> Self {
      Self::new(IpAddr::V6(ip), port)
   }

   /// Get the socket address of the peer
   pub fn socket_addr(&self) -> SocketAddr {
      SocketAddr::new(self.ip, self.port)
   }

   /// Create a new peer from a socket address
   pub fn from_socket_addr(peer_addr: SocketAddr) -> Self {
      Self::new(peer_addr.ip(), peer_addr.port())
   }
}

impl Display for Peer {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(f, "{}:{}", self.ip, self.port)
   }
}

// Generic transport protocol trait that defines behavior for a specific protocol
#[async_trait]
pub trait TransportProtocol: Send + Sync + Clone {
   /// Handshakes with a peer and returns the socket address of the peer. This socket address is
   /// also a [PeerKey](PeerKey) -- AKA the key to the hashmap of the given protocol. For further
   /// clarification on this hashmap, please see an implementation of this trait such as
   /// [UtpProtocol](utp::UtpProtocol)
   async fn connect_peer(
      &mut self,
      peer: &mut Peer,
      id: Arc<Hash<20>>,
      info_hash: Arc<InfoHash>,
   ) -> Result<PeerKey, PeerTransportError>;
   async fn send_data(&mut self, to: PeerKey, data: Vec<u8>) -> Result<(), PeerTransportError>;
   /// Receives data from a peers stream. In other words, if you wish to directly contact a peer,
   /// use this function.
   async fn receive_from_peer(&mut self, peer: PeerKey)
      -> Result<PeerMessages, PeerTransportError>;
   /// Receives data from any incoming peer. Generally used for accepting a handshake.
   async fn receive_data(
      &mut self,
      info_hash: Arc<InfoHash>,
      id: Arc<Hash<20>>,
   ) -> Result<(PeerKey, Vec<u8>), PeerTransportError>;
   fn close_connection(&mut self, peer_key: PeerKey) -> Result<()>;
   fn is_peer_connected(&self, peer_key: PeerKey) -> bool;
   async fn get_connected_peer(&self, peer_key: PeerKey) -> Option<Peer>;

   /// Takes in a received handshake and returns the handshake we should respond with as well as the new peer. It preassigns the peer_id to the peer.
   fn validate_handshake(
      &mut self,
      received_handshake: Handshake,
      peer_addr: SocketAddr,
      info_hash: Arc<InfoHash>,
      id: Arc<Hash<20>>,
   ) -> Result<(Handshake, Peer), PeerTransportError> {
      let peer_id = received_handshake.peer_id;

      // Validate protocol string
      if MAGIC_STRING != received_handshake.protocol {
         error!("Invalid magic string received from peer {}", peer_addr);
         return Err(PeerTransportError::InvalidMagicString {
            received: String::from_utf8_lossy(&received_handshake.protocol).into(),
            expected: String::from_utf8_lossy(MAGIC_STRING).into(),
         });
      }

      // Validate info hash
      if info_hash.clone() != received_handshake.info_hash {
         error!("Invalid info hash received from peer {}", peer_addr);
         return Err(PeerTransportError::InvalidInfoHash {
            received: received_handshake.info_hash.to_hex(),
            expected: info_hash.clone().to_hex(),
         });
      }

      trace!("Received valid handshake from {}", peer_addr);

      let mut peer = Peer::from_socket_addr(peer_addr);
      peer.id = Some(*peer_id);

      let handshake = Handshake::new(info_hash.clone(), id.clone());
      Ok((handshake, peer))
   }
}

#[async_trait] // Async in traits are typically not allowed so we need this crate to make it work
#[allow(unused_variables)]
pub trait Transport: Send + Sync {
   fn id(&self) -> Arc<Hash<20>>;

   fn info_hash(&self) -> Arc<InfoHash>;

   /// Connects to the peer using the transport's implementation and adds it to its internal list of peers.
   /// Runs the handshake and Returns the connected peer's ID.
   /// As shown in <https://wiki.theory.org/BitTorrentSpecification#Handshake>
   async fn connect(&mut self, peer: &mut Peer) -> Result<PeerKey, PeerTransportError>;

   /// Sends a message to a specific peer with the given ID.
   async fn send_raw(&mut self, to: PeerKey, message: Vec<u8>) -> Result<(), PeerTransportError>;

   async fn send(&mut self, to: PeerKey, message: PeerMessages) -> Result<(), PeerTransportError> {
      self.send_raw(to, message.to_bytes()?).await
   }

   async fn broadcast_raw(&mut self, message: Vec<u8>) -> Vec<Result<(), PeerTransportError>>;

   async fn broadcast(&mut self, message: &PeerMessages) -> Vec<Result<(), PeerTransportError>> {
      self.broadcast_raw(message.to_bytes().unwrap()).await
   }

   async fn recv_raw(&mut self) -> Result<(PeerKey, Vec<u8>), PeerTransportError>;

   async fn recv(&mut self) -> Result<(PeerKey, PeerMessages), PeerTransportError> {
      let (key, raw) = self.recv_raw().await?;
      let message = PeerMessages::from_bytes(raw)?;

      Ok((key, message))
   }

   fn close(&mut self, peer_id: PeerKey) -> Result<()>;

   fn is_connected(&self, peer_id: PeerKey) -> bool;

   async fn get_peer(&self, peer_key: PeerKey) -> Option<Peer>;
}
// Generic transport handler that works with any protocol implementation
#[allow(dead_code)]
pub struct TransportHandler<P: TransportProtocol> {
   pub protocol: P,
   tx: Sender<TransportCommand>,
   rx: Receiver<TransportCommand>,
   id: Arc<Hash<20>>,
   info_hash: Arc<InfoHash>,
}
#[allow(dead_code)]
impl<P: TransportProtocol + 'static> TransportHandler<P> {
   pub fn new(protocol: P, id: Arc<Hash<20>>, info_hash: Arc<InfoHash>) -> Self {
      let (tx, rx) = mpsc::channel(32);

      Self {
         protocol,
         tx,
         rx,
         id,
         info_hash,
      }
   }

   pub fn sender(&self) -> Sender<TransportCommand> {
      self.tx.clone()
   }

   pub async fn handle_commands(&mut self) -> Result<()> {
      while let Some(cmd) = self.rx.recv().await {
         let mut transport_clone = self.protocol.clone();
         match cmd {
            TransportCommand::Connect {
               mut peer,
               oneshot_tx,
            } => {
               trace!("Connecting to peer: {}", peer.ip);
               let info_hash = self.info_hash.clone();
               let id = self.id.clone();
               tokio::spawn(async move {
                  // Peers should be able to finish their handshake after two seconds
                  const TIMEOUT_DURATION: u64 = 2;
                  let connect = transport_clone.connect_peer(&mut peer, id, info_hash);
                  let res = timeout(Duration::from_secs(TIMEOUT_DURATION), connect)
                     .await
                     .map_err(|e| error!("Error connecting to peer: {e}"));

                  // Handle error from timeout
                  if res.is_err() {
                     error!(%peer, "Peer timed out.");
                     if oneshot_tx
                        .send(Err(PeerTransportError::MessageFailed))
                        .is_err()
                     {
                        error!("Error occured when sending result back");
                     };
                  } else if oneshot_tx
                     .send(Ok(TransportResponse::Connect(res.unwrap().unwrap())))
                     .is_err()
                  {
                     error!("Error occured when sending result back");
                  }
               });
            }
            TransportCommand::Receive {
               peer_key,
               oneshot_tx,
            } => {
               trace!("Receiving message from peer");

               // It is reasonable to assume that peers would send a message within 2 seconds.
               const TIMEOUT_DURATION: u64 = 2;

               let response = transport_clone.receive_from_peer(peer_key);
               let res = timeout(Duration::from_secs(TIMEOUT_DURATION), response)
                  .await
                  .map_err(|e| error!("Error receiving bytes from peer: {}", e));
               if res.is_err() {
                  if oneshot_tx
                     .send(Err(PeerTransportError::InvalidPeerResponse(
                        "Invalid peer response".into(),
                     )))
                     .is_err()
                  {
                     error!("Error occured when sending result back");
                  };
               } else if oneshot_tx
                  .send(Ok(TransportResponse::Receive {
                     message: (res.unwrap().unwrap()),
                     peer_key: (peer_key),
                  }))
                  .is_err()
               {
                  error!("Error occured when sending result back");
               }
            }
         }
      }
      Ok(())
   }
}

// Implement the base Transport trait for any transport handler with a protocol
#[async_trait]
impl<P: TransportProtocol + Send + Sync + 'static> Transport for TransportHandler<P> {
   fn id(&self) -> Arc<Hash<20>> {
      self.id.clone()
   }

   fn info_hash(&self) -> Arc<InfoHash> {
      self.info_hash.clone()
   }

   async fn connect(&mut self, peer: &mut Peer) -> Result<PeerKey, PeerTransportError> {
      self
         .protocol
         .connect_peer(peer, self.id.clone(), self.info_hash.clone())
         .await
   }

   async fn send_raw(&mut self, to: PeerKey, message: Vec<u8>) -> Result<(), PeerTransportError> {
      self.protocol.send_data(to, message).await
   }

   async fn recv_raw(&mut self) -> Result<(PeerKey, Vec<u8>), PeerTransportError> {
      self
         .protocol
         .receive_data(self.info_hash.clone(), self.id.clone())
         .await
   }

   fn close(&mut self, peer_id: PeerKey) -> Result<()> {
      self.protocol.close_connection(peer_id)
   }

   fn is_connected(&self, peer_id: PeerKey) -> bool {
      self.protocol.is_peer_connected(peer_id)
   }

   async fn get_peer(&self, peer_key: PeerKey) -> Option<Peer> {
      self.protocol.get_connected_peer(peer_key).await
   }

   // Default implementation for handshake can be kept the same
   #[allow(unreachable_code, unused_variables)]
   async fn broadcast_raw(&mut self, message: Vec<u8>) -> Vec<Result<(), PeerTransportError>> {
      // Default implementation that can be overridden by specific protocols for optimization
      let connected_peers: Vec<PeerKey> = todo!("Get list of connected peers");
      let mut results = Vec::with_capacity(connected_peers.len());

      for peer_key in connected_peers {
         results.push(self.send_raw(peer_key, message.clone()).await);
      }

      results
   }
}

#[cfg(test)]
mod tests {
   use tracing_test::traced_test;

   use super::*;

   #[tokio::test]
   #[traced_test]
   async fn test_peer_creation() {
      let peer = Peer::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 6881);
      assert_eq!(peer.ip, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
      assert_eq!(peer.port, 6881);
   }
}
