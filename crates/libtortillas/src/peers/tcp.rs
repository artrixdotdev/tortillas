use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;
use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use tokio::{
   net::{TcpListener, TcpStream},
   sync::Mutex,
};
use tracing::trace;

use crate::errors::PeerTransportError;
use crate::hashes::{Hash, InfoHash};

use super::{Peer, PeerKey, TransportProtocol};

#[derive(Clone)]
pub struct TcpProtocol {
   pub socket: Arc<TcpListener>,
   pub peers: HashMap<PeerKey, Arc<Mutex<(Peer, TcpStream)>>>,
}

impl TcpProtocol {
   pub async fn new(socket_addr: Option<SocketAddr>) -> TcpProtocol {
      let socket_addr = socket_addr.unwrap_or(SocketAddr::from_str("0.0.0.0:6881").unwrap());
      trace!("Creating UTP socket at {}", socket_addr);
      let socket = Arc::new(TcpListener::bind(socket_addr).await.unwrap());

      TcpProtocol {
         socket,
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
      Ok(PeerKey::new(IpAddr::from_str("192.168.1.0").unwrap(), 9999))
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
