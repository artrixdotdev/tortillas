use std::net::Ipv4Addr;

use serde::Deserialize;

pub mod utp;

#[derive(Debug, Deserialize)]
pub struct Peer {
   pub ip: Ipv4Addr,
   pub port: u16,
}
