use std::{fmt::Display, net::Ipv4Addr};

use serde::Serialize;
use tokio::time::Instant;
pub mod utp;

#[derive(Debug, Clone, PartialEq)]
pub struct Peer {
   pub ip: Ipv4Addr,
   pub port: u16,
   chocked: bool,
   interested: bool,
   download_rate: f32,
   upload_rate: f32,
   pieces: Vec<bool>,
   last_optimistic_unchock: Option<Instant>,
}

impl Display for Peer {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(f, "{}:{}", self.ip, self.port)
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
   pub fn new(ip: Ipv4Addr, port: u16) -> Self {
      Peer {
         ip,
         port,
         chocked: true,
         interested: false,
         download_rate: 0.0,
         upload_rate: 0.0,
         pieces: vec![],
         last_optimistic_unchock: None,
      }
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
