use std::net::SocketAddr;

pub mod commands;
pub mod messages;
pub mod stream;

pub type PeerKey = SocketAddr;
