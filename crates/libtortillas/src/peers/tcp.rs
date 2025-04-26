use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;
use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::{
   net::{TcpListener, TcpStream},
   sync::Mutex,
};
use tracing::{error, info, trace};

use crate::errors::PeerTransportError;
use crate::hashes::{Hash, InfoHash};
use crate::peers::messages::{Handshake, MAGIC_STRING};

use super::{Peer, PeerKey, TransportProtocol};

#[derive(Clone)]
pub struct TcpProtocol {
   pub listener: Arc<TcpListener>,
   pub peers: HashMap<PeerKey, Arc<Mutex<(Peer, TcpStream)>>>,
}

impl TcpProtocol {
   pub async fn new(socket_addr: Option<SocketAddr>) -> TcpProtocol {
      let socket_addr = socket_addr.unwrap_or(SocketAddr::from_str("0.0.0.0:6881").unwrap());
      trace!("Creating UTP socket at {}", socket_addr);
      let listener = Arc::new(TcpListener::bind(socket_addr).await.unwrap());

      TcpProtocol {
         listener,
         peers: HashMap::new(),
      }
   }
}

#[async_trait]
#[allow(unused_variables)]
impl TransportProtocol for TcpProtocol {
   async fn connect_peer(
      &mut self,
      peer: &mut Peer,
      id: Arc<Hash<20>>,
      info_hash: Arc<InfoHash>,
   ) -> Result<PeerKey, PeerTransportError> {
      trace!("Attemping connection to {}", peer.socket_addr());
      let mut stream = TcpStream::connect(peer.ip.to_string()).await.map_err(|e| {
         error!("Failed to connect to peer {e}: {}", peer.socket_addr());
            PeerTransportError::ConnectionFailed(peer.socket_addr().to_string())
      })?;

      let handshake = Handshake::new(info_hash.clone(), id.clone());
      let handshake_bytes = stream.write(&handshake.to_bytes());
      trace!("Sent handshake to peer");


      // Calculate expected size for response
      // 1 byte + protocol + reserved + hashes
      const EXPECTED_SIZE: usize = 1 + MAGIC_STRING.len() + 8 + 40;
      let mut buf = [0u8; EXPECTED_SIZE];

      // Read response handshake
      stream.read_exact(&mut buf).await.map_err(|e| {
         error!("Failed to read handshake from peer: {}", e);
         PeerTransportError::ConnectionFailed(peer.socket_addr().to_string())
      })?;

      let handshake =
         Handshake::from_bytes(&buf).map_err(|e| PeerTransportError::Other(anyhow!("{e}")))?;

      let (_, new_peer) = self
         .validate_handshake(handshake, peer.socket_addr(), info_hash, id)
         .unwrap();

      // Store peer information
      let peer_id = new_peer.id.unwrap();
      peer.id = Some(peer_id);

      self.peers.insert(
         peer.socket_addr(),
         Arc::new(Mutex::new((peer.clone(), stream))),
      );

      info!(%peer, "Peer connected");

      Ok(peer.socket_addr())
   }

   async fn send_data(&mut self, to: PeerKey, data: Vec<u8>) -> Result<(), PeerTransportError> {
      Ok(())
   }

   async fn receive_data(
      &mut self,
      info_hash: Arc<InfoHash>,
      id: Arc<Hash<20>>,
   ) -> Result<(PeerKey, Vec<u8>), PeerTransportError> {
      Ok((
         PeerKey::new(IpAddr::from_str("192.168.1.0").unwrap(), 9999),
         vec![0],
      ))
   }

   fn close_connection(&mut self, peer_key: PeerKey) -> Result<()> {
      Ok(())
   }

   fn is_peer_connected(&self, peer_key: PeerKey) -> bool {
      true
   }

   async fn get_connected_peer(&self, peer_key: PeerKey) -> Option<Peer> {
      Some(Peer::from_ipv4(
         Ipv4Addr::from_str("192.168.1.0").unwrap(),
         9999,
      ))
   }
}
