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
pub use id::*;
pub(crate) use info::*;
pub(crate) use state::*;
pub(crate) use supports::*;

/// It should be noted that the *name* PeerKey is slightly deprecated from
/// previous renditions of libtortillas. The idea of having a type for the "key"
/// of a peer is still completely relevant though.
pub type PeerKey = SocketAddr;

pub const MAGIC_STRING: &[u8] = b"BitTorrent protocol";

/// The identity advertised for a peer on the BitTorrent wire.
///
/// Connection state belongs to the peer actor once a connection is
/// established.
#[derive(Clone)]
pub struct WirePeer {
   pub ip: IpAddr,
   pub port: u16,
   pub id: Option<PeerId>,
}

impl Debug for WirePeer {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.debug_struct("WirePeer")
         .field("addr", &self.socket_addr())
         .field("id", &self.id)
         .finish()
   }
}

impl InternalHash for WirePeer {
   fn hash<H: Hasher>(&self, state: &mut H) {
      self.socket_addr().hash(state)
   }
}

impl Eq for WirePeer {}
impl PartialEq for WirePeer {
   fn eq(&self, other: &Self) -> bool {
      self.socket_addr() == other.socket_addr()
   }
}

impl Display for WirePeer {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      let display = match &self.id {
         Some(id) => id.to_string(),
         None => format!("{}:{}", self.ip, self.port),
      };
      write!(f, "{}", display)
   }
}

impl WirePeer {
   /// Create a new peer with the given IP address and port
   pub fn new(ip: IpAddr, port: u16) -> Self {
      Self { ip, port, id: None }
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
