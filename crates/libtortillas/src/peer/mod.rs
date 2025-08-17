mod actor;
mod id;
mod info;
mod state;
mod supports;

use std::{
   fmt::{self, Debug, Display},
   hash::{Hash as InternalHash, Hasher},
   net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

pub(crate) use actor::*;
use bitvec::vec::BitVec;
use bytes::BytesMut;
pub use id::*;
pub use info::*;
pub use state::*;
pub use supports::*;

/// It should be noted that the *name* PeerKey is slightly deprecated from
/// previous renditions of libtortillas. The idea of having a type for the "key"
/// of a peer is still completely relevant though.
pub type PeerKey = SocketAddr;

pub const MAGIC_STRING: &[u8] = b"BitTorrent protocol";

/// Represents a BitTorrent peer with connection state and statistics
/// Download rate and upload rate are measured in kilobytes per second.
#[derive(Clone)]
pub struct Peer {
   pub ip: IpAddr,
   pub port: u16,
   pub state: PeerState,
   pub pieces: BitVec<u8>,
   /// The reserved bytes that the peer sent us in their handshake. This
   /// indicates what extensions the peer supports.
   pub reserved: [u8; 8],
   pub peer_supports: PeerSupports,
   pub id: Option<PeerId>,
   pub info: PeerInfo,
}

impl Debug for Peer {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.debug_struct("Peer")
         .field("addr", &self.socket_addr())
         .field("choked", &self.choked())
         .field("interested", &self.interested())
         .field("am_choked", &self.am_choked())
         .field("am_interested", &self.am_interested())
         .field("id", &self.id)
         .finish()
   }
}

impl InternalHash for Peer {
   fn hash<H: Hasher>(&self, state: &mut H) {
      self.socket_addr().hash(state)
   }
}

impl Eq for Peer {}
impl PartialEq for Peer {
   fn eq(&self, other: &Self) -> bool {
      self.socket_addr() == other.socket_addr()
   }
}

impl Display for Peer {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(f, "{}:{}", self.ip, self.port)
   }
}

impl Peer {
   /// Create a new peer with the given IP address and port
   pub fn new(ip: IpAddr, port: u16) -> Self {
      Peer {
         ip,
         port,
         state: PeerState::new(),
         pieces: BitVec::EMPTY,
         reserved: [0u8; 8],
         peer_supports: PeerSupports::new(),
         id: None,
         info: PeerInfo::new(0, BytesMut::new()),
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
