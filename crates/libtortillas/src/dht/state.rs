use std::net::SocketAddr;

use super::{
   Contact, DhtError, NodeId, PeerStore, Query, Response, RoutingTable, TokenManager, decode_nodes,
   encode_nodes, encode_peers,
};
use crate::settings::DhtSettings;

/// Mutable protocol state owned by the DHT actor.
pub struct DhtState {
   routing: RoutingTable,
   tokens: TokenManager,
   peers: PeerStore,
   peer_limit: usize,
   bucket_size: usize,
}

impl DhtState {
   pub fn new(id: NodeId, settings: &DhtSettings) -> Self {
      Self {
         routing: RoutingTable::new(id, settings.bucket_size),
         tokens: TokenManager::new(settings.token_rotation_interval),
         peers: PeerStore::new(settings.peer_record_ttl),
         peer_limit: settings.lookup_peer_limit,
         bucket_size: settings.bucket_size,
      }
   }

   pub const fn id(&self) -> NodeId {
      self.routing.local_id()
   }

   pub fn routing(&self) -> &RoutingTable {
      &self.routing
   }

   pub fn routing_mut(&mut self) -> &mut RoutingTable {
      &mut self.routing
   }

   pub fn learn_response(&mut self, response: &Response, source: SocketAddr) -> anyhow::Result<()> {
      self.routing.insert(Contact::new(response.id, source));
      if let Some(nodes) = &response.nodes {
         for contact in decode_nodes(nodes)? {
            self.routing.insert(contact);
         }
      }
      Ok(())
   }

   /// Applies one BEP 5 query and builds the matching response dictionary.
   pub fn handle_query(&mut self, query: Query, source: SocketAddr) -> Result<Response, DhtError> {
      self.routing.insert(Contact::new(query.id(), source));

      match query {
         Query::Ping { .. } => Ok(Response::pong(self.id())),
         Query::FindNode { target, .. } => Ok(Response {
            id: self.id(),
            nodes: Some(encode_nodes(self.routing.closest(target, self.bucket_size))),
            token: None,
            values: None,
         }),
         Query::GetPeers { info_hash, .. } => {
            let peers = self.peers.get(info_hash, self.peer_limit);
            let nodes = peers
               .is_empty()
               .then(|| encode_nodes(self.routing.closest(info_hash, self.bucket_size)));
            Ok(Response {
               id: self.id(),
               nodes,
               token: Some(self.tokens.generate(source.ip())),
               values: (!peers.is_empty()).then(|| encode_peers(peers)),
            })
         }
         Query::AnnouncePeer {
            info_hash,
            port,
            token,
            implied_port,
            ..
         } => {
            if !self.tokens.verify(&token, source.ip()) {
               return Err(DhtError::protocol("invalid announce token"));
            }
            let peer =
               SocketAddr::new(source.ip(), if implied_port { source.port() } else { port });
            if !self.peers.announce(info_hash, peer) {
               return Err(DhtError::protocol("invalid announced peer endpoint"));
            }
            Ok(Response::pong(self.id()))
         }
      }
   }
}

#[cfg(test)]
mod tests {
   use std::time::Duration;

   use super::*;
   use crate::dht::DHT_ID_LEN;

   fn state() -> DhtState {
      let settings = DhtSettings {
         token_rotation_interval: Duration::from_secs(60),
         peer_record_ttl: Duration::from_secs(60),
         ..DhtSettings::default()
      };
      DhtState::new(NodeId::from_bytes([1; DHT_ID_LEN]), &settings)
   }

   #[test]
   fn state_when_ping_arrives_then_returns_local_id_and_learns_node() {
      let mut state = state();
      let remote = NodeId::from_bytes([2; DHT_ID_LEN]);
      let source = "192.0.2.1:6881".parse().unwrap();

      let response = state
         .handle_query(Query::Ping { id: remote }, source)
         .unwrap();

      assert_eq!(response, Response::pong(state.id()));
      assert_eq!(
         state.routing().closest(remote, 1),
         vec![Contact::new(remote, source)]
      );
   }

   #[test]
   fn state_when_get_peers_arrives_then_returns_nodes_and_address_bound_token() {
      let mut state = state();
      let remote = NodeId::from_bytes([2; DHT_ID_LEN]);
      let info_hash = NodeId::from_bytes([3; DHT_ID_LEN]);
      let source = "192.0.2.1:6881".parse().unwrap();

      let response = state
         .handle_query(
            Query::GetPeers {
               id: remote,
               info_hash,
            },
            source,
         )
         .unwrap();

      assert!(response.token.is_some());
      assert!(response.nodes.is_some());
      assert!(response.values.is_none());
   }

   #[test]
   fn state_when_valid_announce_arrives_then_returns_peer_to_later_lookup() {
      let mut state = state();
      let remote = NodeId::from_bytes([2; DHT_ID_LEN]);
      let info_hash = NodeId::from_bytes([3; DHT_ID_LEN]);
      let source = "192.0.2.1:49000".parse().unwrap();
      let token = state
         .handle_query(
            Query::GetPeers {
               id: remote,
               info_hash,
            },
            source,
         )
         .unwrap()
         .token
         .unwrap();

      state
         .handle_query(
            Query::AnnouncePeer {
               id: remote,
               info_hash,
               port: 6881,
               token,
               implied_port: false,
            },
            source,
         )
         .unwrap();
      let response = state
         .handle_query(
            Query::GetPeers {
               id: remote,
               info_hash,
            },
            source,
         )
         .unwrap();

      assert_eq!(
         response.values,
         Some(encode_peers(["192.0.2.1:6881".parse().unwrap()]))
      );
      assert!(response.nodes.is_none());
   }

   #[test]
   fn state_when_announce_token_is_invalid_then_rejects_peer() {
      let mut state = state();
      let result = state.handle_query(
         Query::AnnouncePeer {
            id: NodeId::from_bytes([2; DHT_ID_LEN]),
            info_hash: NodeId::from_bytes([3; DHT_ID_LEN]),
            port: 6881,
            token: vec![0],
            implied_port: false,
         },
         "192.0.2.1:49000".parse().unwrap(),
      );

      assert_eq!(
         result.unwrap_err().code,
         DhtError::protocol("expected code").code
      );
   }
}
