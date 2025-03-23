use super::{Peer, PeerMessages, Transport};
use crate::hashes::{Hash, InfoHash};
use anyhow::{Ok, Result};
use async_trait::async_trait;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use utp_rs::{cid::ConnectionPeer, socket::UtpSocket};

pub struct UtpTransport {
   pub peer: Peer,
   pub socket: UtpSocket<SocketAddr>,
}

impl UtpTransport {
   pub async fn new(peer: Peer) -> UtpTransport {
      let socket_addr = &peer.socket_addr();
      UtpTransport {
         peer,
         socket: UtpSocket::bind(*socket_addr).await.unwrap(),
      }
   }
}

#[async_trait]
impl Transport for UtpTransport {
   async fn handshake(&mut self, info_hash: Arc<InfoHash>, peer_id: Hash<20>) -> Result<Hash<20>> {
      // 1. Make request to peer IP and port
      // Handshake should start with "character nineteen (decimal) followed by the string
      // 'BitTorrent protocol'."
      // All integers should be encoded as four bytes big-endian.
      // After fixed headers, reserved bytes (0).
      // 20 byte sha1 hash of bencoded form of info value (info_hash). If both sides don't send the
      // same value, sever the connection.
      // 20 byte peer id. If receiving side's id doesn't match the one the initiating side expects,
      //    sever the connection.
      let socket_addr = self.peer.socket_addr();

      // <https://www.bittorrent.org/beps/bep_0003.html>

      // 2. Return
      let blank = Hash::new([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]);
      Ok(blank)
   }
   async fn connect(&mut self) -> Result<()> {
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
