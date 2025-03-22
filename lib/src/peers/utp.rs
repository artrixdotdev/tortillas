use super::Peer;
use crate::hashes::InfoHash;
use anyhow::{Ok, Result};
use std::sync::Arc;

pub struct UtpTransport(Peer);

impl UtpTransport {
   pub fn handshake(info_hash: Arc<InfoHash>) -> Result<()> {
      Ok(())
   }
}
