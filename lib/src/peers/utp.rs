use super::{Peer, PeerMessages, Transport};
use crate::hashes::{Hash, InfoHash};
use anyhow::{Ok, Result};
use async_trait::async_trait;
use librqbit_utp::{UtpSocket, UtpStream};
use std::{net::SocketAddr, str::FromStr, sync::Arc, time::Duration};

pub struct UtpTransport {
   pub peer: Peer,
   pub stream: UtpStream,
}

impl UtpTransport {
   pub async fn new(peer: Peer) -> UtpTransport {
      let socket = UtpSocket::new_udp(SocketAddr::from_str("0.0.0.0").unwrap())
         .await
         .unwrap();
      UtpTransport {
         peer,
         stream: socket.accept().await.unwrap(),
      }
   }
}

#[async_trait]
impl Transport for UtpTransport {
   /// Handshake should start with "character nineteen (decimal) followed by the string
   /// 'BitTorrent protocol'."
   /// All integers should be encoded as four bytes big-endian.
   /// After fixed headers, reserved bytes (0).
   /// 20 byte sha1 hash of bencoded form of info value (info_hash). If both sides don't send the
   /// same value, sever the connection.
   /// 20 byte peer id. If receiving side's id doesn't match the one the initiating side expects sever the connection.
   ///
   /// <https://www.bittorrent.org/beps/bep_0003.html>
   async fn handshake(&mut self, info_hash: Arc<InfoHash>, peer_id: Hash<20>) -> Result<Hash<20>> {
      // Create headers
      let mut headers = Vec::with_capacity(68);
      headers.extend_from_slice(&19u8.to_be_bytes());
      headers.extend_from_slice(b"BitTorrent protocol");
      headers.extend_from_slice(&[0u8; 8]);
      headers.extend_from_slice(info_hash.as_bytes());
      headers.extend_from_slice(peer_id.as_bytes());

      // let (mut reader, mut writer) = self.stream.split();

      // Add headers to writer

      let blank = Hash::new([1; 20]);
      Ok(blank)
   }
   async fn connect(&mut self) -> Result<()> {
      // let config = ConnectionConfig::default();
      // let mut stream = self.socket(self.peer.socket_addr(), config).await;
      // stream.write(...)
      Ok(())
   }

   async fn send(&mut self, message: &PeerMessages) -> Result<()> {
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
