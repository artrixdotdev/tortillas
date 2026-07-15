//! Mainline BitTorrent DHT support.
//!
//! This module implements the BEP 5 Kademlia network used to discover peers
//! without a tracker. The engine owns one DHT service and shares it across all
//! torrents.
