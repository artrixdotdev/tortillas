use anyhow::Result;
use async_trait::async_trait;
use messages::PeerMessages;
use std::{
   fmt::Display,
   net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
   sync::Arc,
};
use tokio::time::Instant;

use crate::{
   errors::PeerTransportError,
   hashes::{Hash, InfoHash},
};
pub mod messages;
pub mod utp;

/// Represents a BitTorrent peer with connection state and statistics
#[derive(Debug, Clone)]
pub struct Peer {
   pub ip: IpAddr,
   pub port: u16,
   pub choked: bool,
   pub interested: bool,
   pub am_choking: bool,
   pub am_interested: bool,
   pub download_rate: f32,
   pub upload_rate: f32,
   pub pieces: Vec<bool>,
   pub last_optimistic_unchoke: Option<Instant>,
   pub id: Option<Hash<20>>,
   pub last_seen: Instant,
   pub bytes_downloaded: u64,
   pub bytes_uploaded: u64,
}

impl Display for Peer {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(f, "{}:{}", self.ip, self.port)
   }
}

#[async_trait] // Async in traits are typically not allowed so we need this crate to make it work
#[allow(unused_variables)]
pub trait Transport: Send + Sync {
   /// Connects to the peer using the transport's implementation and adds it to its internal list of peers.
   /// Runs the handshake and Returns the connected peer's ID.
   /// As shown in <https://wiki.theory.org/BitTorrentSpecification#Handshake>
   async fn connect(&mut self, peer: &mut Peer) -> Result<Hash<20>, PeerTransportError>;

   /// Sends a message to a specific peer with the given ID.
   async fn send_raw(&mut self, to: Hash<20>, message: Vec<u8>) -> Result<(), PeerTransportError>;

   async fn send(&mut self, to: Hash<20>, message: PeerMessages) -> Result<(), PeerTransportError> {
      Ok(())
   }

   async fn broadcast_raw(&mut self, message: Vec<u8>) -> Vec<Result<(), PeerTransportError>>;

   async fn broadcast(&mut self, message: &PeerMessages) -> Vec<Result<(), PeerTransportError>> {
      vec![]
   }

   async fn accept_incoming(&mut self) -> Result<Peer, PeerTransportError>;

   async fn close(&mut self, peer_id: Hash<20>) -> Result<()>;

   /// Our current peer ID
   fn id(&self) -> Arc<Hash<20>>;

   fn info_hash(&self) -> Arc<InfoHash>;

   fn is_connected(&self) -> bool;
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
         download_rate: 0.0,
         upload_rate: 0.0,
         pieces: vec![],
         last_optimistic_unchoke: None,
         id: None,
         last_seen: Instant::now(),
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
