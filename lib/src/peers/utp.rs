use super::{Peer, PeerMessages, Transport};
use crate::hashes::{Hash, InfoHash};
use anyhow::{Ok, Result};
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};

pub struct UtpTransport {
   pub peer: Peer,
}

#[async_trait]
impl Transport for UtpTransport {
   async fn handshake(&mut self, info_hash: Arc<InfoHash>, peer_id: Hash<20>) -> Result<Hash<20>> {
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
      let x = PeerMessages::Choke;
      Ok(x)
   }

   async fn close(&mut self) -> Result<()> {
      Ok(())
   }

   fn is_connected(&self) -> bool {
      false
   }
}
