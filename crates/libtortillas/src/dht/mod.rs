//! Mainline BitTorrent DHT support.
//!
//! This module implements the [BEP 5] Kademlia network used to discover peers
//! without a tracker. The engine owns one DHT service so its routing knowledge
//! and UDP socket can be shared across all torrents.
//!
//! [BEP 5]: https://www.bittorrent.org/beps/bep_0005.html

mod compact;
mod id;
mod message;
mod peers;
mod routing;
mod token;

pub use compact::{Contact, decode_nodes, decode_peers, encode_nodes, encode_peers};
pub use id::{DHT_ID_LEN, Distance, NodeId};
pub use message::{DhtError, Message, Query, Response};
pub use peers::PeerStore;
pub use routing::RoutingTable;
pub use token::TokenManager;
