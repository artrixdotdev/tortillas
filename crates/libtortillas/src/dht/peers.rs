use std::{
   collections::HashMap,
   net::SocketAddr,
   time::{Duration, Instant},
};

#[cfg(test)]
use super::DHT_ID_LEN;
use super::NodeId;

const PRUNE_INTERVAL_DIVISOR: u32 = 4;

/// Time-bounded storage for peers announced to this DHT node.
///
/// BEP 5 requires announced peers to expire; otherwise nodes would return
/// endpoints long after clients leave a swarm. See [`announce_peer`].
///
/// [`announce_peer`]: https://www.bittorrent.org/beps/bep_0005.html#announce-peer
#[derive(Debug)]
pub struct PeerStore {
   peers: HashMap<NodeId, HashMap<SocketAddr, Instant>>,
   ttl: Duration,
   prune_interval: Duration,
   last_pruned: Instant,
}

impl PeerStore {
   pub fn new(ttl: Duration) -> Self {
      Self {
         peers: HashMap::new(),
         ttl,
         prune_interval: ttl / PRUNE_INTERVAL_DIVISOR,
         last_pruned: Instant::now(),
      }
   }

   pub fn announce(&mut self, info_hash: NodeId, peer: SocketAddr) -> bool {
      if peer.port() == 0 || peer.ip().is_unspecified() {
         return false;
      }
      self.prune_if_due();
      self
         .peers
         .entry(info_hash)
         .or_default()
         .insert(peer, Instant::now());
      true
   }

   pub fn get(&mut self, info_hash: NodeId, limit: usize) -> Vec<SocketAddr> {
      self.prune_if_due();
      let cutoff = self.expiration_cutoff();
      let Some(peers) = self.peers.get_mut(&info_hash) else {
         return Vec::new();
      };
      peers.retain(|_, announced_at| *announced_at >= cutoff);
      let result = peers.keys().copied().take(limit).collect();
      if peers.is_empty() {
         self.peers.remove(&info_hash);
      }
      result
   }

   pub fn prune(&mut self) {
      self.last_pruned = Instant::now();
      let cutoff = self.expiration_cutoff();
      self.peers.retain(|_, peers| {
         peers.retain(|_, announced_at| *announced_at >= cutoff);
         !peers.is_empty()
      });
   }

   fn prune_if_due(&mut self) {
      if self.last_pruned.elapsed() >= self.prune_interval {
         self.prune();
      }
   }

   fn expiration_cutoff(&self) -> Instant {
      Instant::now()
         .checked_sub(self.ttl)
         .unwrap_or_else(Instant::now)
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn peer_store_when_peer_is_announced_then_returns_endpoint() {
      let mut store = PeerStore::new(Duration::from_secs(60));
      let info_hash = NodeId::from_bytes([1; DHT_ID_LEN]);
      let peer = "192.0.2.1:6881".parse().unwrap();

      assert!(store.announce(info_hash, peer));
      assert_eq!(store.get(info_hash, 10), vec![peer]);
   }

   #[test]
   fn peer_store_when_record_expires_then_removes_endpoint() {
      let mut store = PeerStore::new(Duration::from_secs(60));
      let info_hash = NodeId::from_bytes([1; DHT_ID_LEN]);
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
   fn peer_store_when_prune_is_not_due_then_only_cleans_requested_swarm() {
      let mut store = PeerStore::new(Duration::from_secs(60));
      let requested_hash = NodeId::from_bytes([1; DHT_ID_LEN]);
      let other_hash = NodeId::from_bytes([2; DHT_ID_LEN]);
      let peer = "192.0.2.1:6881".parse().unwrap();
      store.announce(requested_hash, peer);
      store.announce(other_hash, peer);
      for peers in store.peers.values_mut() {
         *peers.get_mut(&peer).unwrap() = Instant::now() - Duration::from_secs(61);
      }

      assert!(store.get(requested_hash, 10).is_empty());
      assert!(!store.peers.contains_key(&requested_hash));
      assert!(store.peers.contains_key(&other_hash));
   }

   #[test]
   fn peer_store_when_endpoint_is_unusable_then_rejects_announcement() {
      let mut store = PeerStore::new(Duration::from_secs(60));
      let info_hash = NodeId::from_bytes([1; DHT_ID_LEN]);

      assert!(!store.announce(info_hash, "0.0.0.0:6881".parse().unwrap()));
      assert!(!store.announce(info_hash, "192.0.2.1:0".parse().unwrap()));
   }
}
