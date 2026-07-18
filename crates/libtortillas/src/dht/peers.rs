use std::{
   collections::HashMap,
   net::SocketAddr,
   time::{Duration, Instant},
};

use super::NodeId;

/// Time-bounded storage for peers announced to this DHT node.
#[derive(Debug)]
pub struct PeerStore {
   peers: HashMap<NodeId, HashMap<SocketAddr, Instant>>,
   ttl: Duration,
}

impl PeerStore {
   pub fn new(ttl: Duration) -> Self {
      Self {
         peers: HashMap::new(),
         ttl,
      }
   }

   pub fn announce(&mut self, info_hash: NodeId, peer: SocketAddr) -> bool {
      if peer.port() == 0 || peer.ip().is_unspecified() {
         return false;
      }
      self.prune();
      self
         .peers
         .entry(info_hash)
         .or_default()
         .insert(peer, Instant::now());
      true
   }

   pub fn get(&mut self, info_hash: NodeId, limit: usize) -> Vec<SocketAddr> {
      self.prune();
      self
         .peers
         .get(&info_hash)
         .into_iter()
         .flat_map(HashMap::keys)
         .copied()
         .take(limit)
         .collect()
   }

   pub fn prune(&mut self) {
      let cutoff = Instant::now()
         .checked_sub(self.ttl)
         .unwrap_or_else(Instant::now);
      self.peers.retain(|_, peers| {
         peers.retain(|_, announced_at| *announced_at >= cutoff);
         !peers.is_empty()
      });
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn peer_store_when_peer_is_announced_then_returns_endpoint() {
      let mut store = PeerStore::new(Duration::from_secs(60));
      let info_hash = NodeId::from_bytes([1; 20]);
      let peer = "192.0.2.1:6881".parse().unwrap();

      assert!(store.announce(info_hash, peer));
      assert_eq!(store.get(info_hash, 10), vec![peer]);
   }

   #[test]
   fn peer_store_when_record_expires_then_removes_endpoint() {
      let mut store = PeerStore::new(Duration::from_secs(60));
      let info_hash = NodeId::from_bytes([1; 20]);
      let peer = "192.0.2.1:6881".parse().unwrap();
      store.announce(info_hash, peer);
      *store
         .peers
         .get_mut(&info_hash)
         .unwrap()
         .get_mut(&peer)
         .unwrap() = Instant::now() - Duration::from_secs(61);

      assert!(store.get(info_hash, 10).is_empty());
      assert!(store.peers.is_empty());
   }

   #[test]
   fn peer_store_when_endpoint_is_unusable_then_rejects_announcement() {
      let mut store = PeerStore::new(Duration::from_secs(60));
      let info_hash = NodeId::from_bytes([1; 20]);

      assert!(!store.announce(info_hash, "0.0.0.0:6881".parse().unwrap()));
      assert!(!store.announce(info_hash, "192.0.2.1:0".parse().unwrap()));
   }
}
