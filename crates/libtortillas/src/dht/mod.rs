//! Mainline BitTorrent DHT support.
//!
//! This module implements the BEP 5 Kademlia network used to discover peers
//! without a tracker. The engine owns one DHT service and shares it across all
//! torrents.

mod compact;
mod id;
mod message;
mod routing;

pub use compact::{Contact, decode_nodes, decode_peers, encode_nodes, encode_peers};
pub use id::{Distance, NodeId};
pub use message::{DhtError, Message, Query, Response};
pub use routing::RoutingTable;
