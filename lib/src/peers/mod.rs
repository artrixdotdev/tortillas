use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use std::{
   fmt::Display,
   net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
   sync::Arc,
};
use tokio::time::{Duration, Instant};

use crate::hashes::{Hash, InfoHash};
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
   pub info_hash: Option<Arc<InfoHash>>,
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
pub trait Transport: Send + Sync {
   async fn connect(&mut self) -> Result<()>;
   /// Should return the remote peer's ID
   async fn handshake(&mut self, info_hash: Arc<InfoHash>, peer_id: Hash<20>) -> Result<Hash<20>>;

   async fn send(&mut self, message: &PeerMessages) -> Result<()>;

   async fn recv(&mut self, timeout: Option<Duration>) -> Result<PeerMessages>;

   async fn close(&mut self) -> Result<()>;

   fn is_connected(&self) -> bool;
}
/// A peer with a specific transport implementation.
/// The point of this struct is to provide a way to interact with a peer using a specific transport implementation.
pub struct ConnectedPeer<T: Transport> {
   pub peer: Peer,
   transport: T,
}

impl<T: Transport> ConnectedPeer<T> {
   pub fn new(peer: Peer, transport: T) -> Self {
      ConnectedPeer { peer, transport }
   }

   pub async fn connect(&mut self) -> Result<()> {
      self.transport.connect().await
   }

   pub async fn handshake(
      &mut self,
      info_hash: Arc<InfoHash>,
      peer_id: Hash<20>,
   ) -> Result<Hash<20>> {
      let remote_id = self.transport.handshake(info_hash.clone(), peer_id).await?;
      self.peer.info_hash = Some(info_hash);
      self.peer.id = Some(remote_id);

      Ok(remote_id)
   }

   pub async fn send(&mut self, message: &PeerMessages) -> Result<()> {
      self.transport.send(message).await
   }

   pub async fn recv(&mut self, timeout: Option<Duration>) -> Result<PeerMessages> {
      self.transport.recv(timeout).await
   }

   pub async fn close(&mut self) -> Result<()> {
      self.transport.close().await
   }
}

pub enum PeerMessages {
   Choke,
   Unchoke,
   Interested,
   NotInterested,
   Have(u32),
   Bitfield(Vec<bool>),
   Request(u32, u32, u32),
   Piece(u32, u32, Vec<u8>),
   Cancel(u32, u32, u32),
}

impl Serialize for PeerMessages {
   fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
   where
      S: serde::Serializer,
   {
      match self {
         PeerMessages::Choke => serializer.serialize_str("choke"),
         PeerMessages::Unchoke => serializer.serialize_str("unchoke"),
         PeerMessages::Interested => serializer.serialize_str("interested"),
         PeerMessages::NotInterested => serializer.serialize_str("not_interested"),
         PeerMessages::Have(index) => serializer.serialize_str(&format!("have {}", index)),
         PeerMessages::Bitfield(bits) => serializer.serialize_str(&format!("bitfield {:?}", bits)),
         PeerMessages::Request(index, begin, length) => {
            serializer.serialize_str(&format!("request {} {} {}", index, begin, length))
         }
         PeerMessages::Piece(index, begin, data) => {
            serializer.serialize_str(&format!("piece {} {} {:?}", index, begin, data))
         }
         PeerMessages::Cancel(index, begin, length) => {
            serializer.serialize_str(&format!("cancel {} {} {}", index, begin, length))
         }
      }
   }
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
         info_hash: None,
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
}

pub enum PeerTransport {
   Utp(utp::UtpTransport),
   // Tcp,
}

impl PeerTransport {
   pub async fn send(&self, _data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
      match self {
         PeerTransport::Utp(transport) => todo!(), // transport.send(data).await,
                                                   // PeerTransport::Tcp(transport) => transport.send(data).await,
      }
   }

   pub async fn receive(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
      match self {
         PeerTransport::Utp(transport) => todo!(), // transport.receive().await,
                                                   // PeerTransport::Tcp(transport) => transport.receive().await,
      }
   }
   pub async fn handshake(&self) -> Result<(), Box<dyn std::error::Error>> {
      match self {
         PeerTransport::Utp(transport) => todo!(), // transport.handshake().await,
                                                   // PeerTransport::Tcp(transport) => transport.handshake().await,
      }
   }
}
