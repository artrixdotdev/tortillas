//! Mainline BitTorrent DHT support.
//!
//! This module implements the [BEP 5] Kademlia network used to discover peers
//! without a tracker. The engine owns one DHT service so its routing knowledge
//! and UDP socket can be shared across all torrents.
//!
//! [BEP 5]: https://www.bittorrent.org/beps/bep_0005.html

mod actor;
mod announce;
mod compact;
mod id;
mod lookup;
mod message;
pub(crate) mod messages;
mod peers;
mod routing;
mod state;
mod token;
mod transaction;
mod transport;

pub(crate) use actor::{DhtActor, DhtActorArgs};
pub(crate) use announce::announce_peer;
pub(crate) use compact::{Contact, decode_nodes, decode_peers, encode_nodes, encode_peers};
pub(crate) use id::{DHT_ID_LEN, NodeId};
pub(crate) use lookup::{LookupResult, lookup_peers};
pub(crate) use message::{DhtError, Message, Query, Response};
pub(crate) use peers::PeerStore;
pub(crate) use routing::RoutingTable;
pub(crate) use state::DhtState;
pub(crate) use token::TokenManager;
pub(crate) use transaction::{TransactionResult, TransactionTable};
pub(crate) use transport::DhtTransport;
