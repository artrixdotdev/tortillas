use super::{Peer, PeerMessages, Transport};
use crate::hashes::{Hash, InfoHash};
use anyhow::{Ok, Result};
use async_trait::async_trait;
use librqbit_utp::{UtpSocket, UtpSocketUdp, UtpStream};
use std::{collections::HashMap, net::SocketAddr, str::FromStr, sync::Arc, time::Duration};

const MAGIC_STRING: &[u8; 19] = b"BitTorrent protocol";

pub struct UtpTransport<'a> {
   pub socket: Arc<UtpSocketUdp>,
   pub id: Arc<Hash<20>>,
   pub info_hash: Arc<InfoHash>,
   pub peers: HashMap<Hash<20>, (&'a mut Peer, UtpStream)>,
}

impl<'a> UtpTransport<'a> {
   pub async fn new(id: Arc<Hash<20>>, info_hash: Arc<InfoHash>) -> UtpTransport<'a> {
      let socket = UtpSocket::new_udp(SocketAddr::from_str("0.0.0.0").unwrap())
         .await
         .unwrap();

      UtpTransport {
         socket,
         id,
         info_hash,
         peers: HashMap::new(),
      }
   }
}

#[async_trait]
impl<'a> Transport for UtpTransport<'a> {
   fn id(&self) -> Arc<Hash<20>> {
      self.id.clone()
   }

   fn info_hash(&self) -> Arc<InfoHash> {
      self.info_hash.clone()
   }

   async fn broadcast(&mut self, message: &PeerMessages) -> Result<()> {
      Ok(())
   }

   /// Handshake should start with "character nineteen (decimal) followed by the string
   /// 'BitTorrent protocol'."
   /// All integers should be encoded as four bytes big-endian.
   /// After fixed headers, reserved bytes (0).
   /// 20 byte sha1 hash of bencoded form of info value (info_hash). If both sides don't send the
   /// same value, sever the connection.
   /// 20 byte peer id. If receiving side's id doesn't match the one the initiating side expects sever the connection.
   ///
   /// <https://www.bittorrent.org/beps/bep_0003.html>
   async fn handshake(&mut self, peer_id: Hash<20>) -> Result<Hash<20>> {
      let peer = self.peers.get(&peer_id);
      // Create headers
      let mut headers = Vec::with_capacity(68);
      headers.extend_from_slice(&MAGIC_STRING.len().to_be_bytes());
      headers.extend_from_slice(MAGIC_STRING);
      headers.extend_from_slice(&[0u8; 8]);
      headers.extend_from_slice(self.info_hash.as_bytes());
      headers.extend_from_slice(peer_id.as_bytes());

      // let (mut reader, mut writer) = self.stream.split();

      // Add headers to writer
      //

      let blank = Hash::new([1; 20]);
      Ok(blank)
   }
   async fn connect(&mut self, peer: &mut Peer) -> Result<Hash<20>> {
      // let config = ConnectionConfig::default();
      // let mut stream = self.socket(self.peer.socket_addr(), config).await;
      // stream.write(...)
      let blank = Hash::new([1; 20]);
      Ok(blank)
   }

   async fn send(&mut self, to: Hash<20>, message: &PeerMessages) -> Result<()> {
      Ok(())
   }

   async fn recv(&mut self, timeout: Option<Duration>) -> Result<PeerMessages> {
      Ok(PeerMessages::Choke)
   }

   async fn close(&mut self) -> Result<()> {
      Ok(())
   }

   fn is_connected(&self) -> bool {
      false
   }
}
