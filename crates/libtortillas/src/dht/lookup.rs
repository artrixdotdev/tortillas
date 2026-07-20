use std::{
   collections::{HashMap, HashSet},
   net::SocketAddr,
};

use futures::{StreamExt, stream};

use super::{Contact, DhtTransport, NodeId, Query, decode_nodes, decode_peers};

/// Peers and valid announce tokens collected by an iterative `get_peers`
/// lookup.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LookupResult {
   pub peers: Vec<SocketAddr>,
   pub announce_candidates: Vec<(Contact, Vec<u8>)>,
}

/// Performs the iterative lookup described by [BEP 5 `get_peers`].
///
/// Only the closest K contacts are retained after each round. This is the
/// Kademlia termination rule that keeps an unreachable network from turning
/// one lookup into an unbounded crawl.
///
/// [BEP 5 `get_peers`]: https://www.bittorrent.org/beps/bep_0005.html#get-peers
pub async fn lookup_peers(
   transport: DhtTransport, id: NodeId, info_hash: NodeId, seeds: Vec<Contact>, concurrency: usize,
   peer_limit: usize, bucket_size: usize,
) -> LookupResult {
   let concurrency = concurrency.max(1);
   let mut candidates = seeds;
   let mut queried = HashSet::new();
   let mut peers = HashSet::new();
   let mut tokens = HashMap::new();

   loop {
      candidates.sort_unstable_by_key(|contact| info_hash.distance(contact.id));
      let mut unique_addresses = HashSet::new();
      candidates.retain(|contact| unique_addresses.insert(contact.addr));
      candidates.truncate(bucket_size);
      let batch = candidates
         .iter()
         .filter(|contact| !queried.contains(&contact.addr))
         .take(concurrency)
         .copied()
         .collect::<Vec<_>>();
      if batch.is_empty() || peers.len() >= peer_limit {
         break;
      }
      queried.extend(batch.iter().map(|contact| contact.addr));

      let responses = stream::iter(batch)
         .map(|contact| {
            let transport = transport.clone();
            async move {
               let response = transport
                  .query(contact.addr, Query::GetPeers { id, info_hash })
                  .await;
               (contact, response)
            }
         })
         .buffer_unordered(concurrency)
         .collect::<Vec<_>>()
         .await;

      for (contact, response) in responses {
         let Ok(response) = response else {
            continue;
         };
         if let Some(token) = response.token {
            tokens.insert(contact, token);
         }
         if let Some(values) = response.values
            && let Ok(discovered) = decode_peers(values)
         {
            peers.extend(discovered);
         }
         if let Some(nodes) = response.nodes
            && let Ok(discovered) = decode_nodes(&nodes)
         {
            candidates.extend(discovered);
         }
      }
   }

   let mut peers = peers.into_iter().collect::<Vec<_>>();
   peers.truncate(peer_limit);
   let mut announce_candidates = tokens.into_iter().collect::<Vec<_>>();
   announce_candidates.sort_unstable_by_key(|(contact, _)| info_hash.distance(contact.id));
   LookupResult {
      peers,
      announce_candidates,
   }
}

#[cfg(test)]
mod tests {
   use std::time::Duration;

   use kameo::actor::Spawn;

   use super::*;
   use crate::{
      dht::{DHT_ID_LEN, DhtActor, actor::DhtActorArgs, messages::commands::LocalAddr},
      settings::DhtSettings,
   };

   const TEST_BUFFER_SIZE: usize = 2048;
   const TEST_BUCKET_SIZE: usize = 8;
   const TEST_LOOKUP_CONCURRENCY: usize = 1;
   const TEST_PEER_LIMIT: usize = 10;
   const TEST_PEER_PORT: u16 = 6881;

   #[tokio::test]
   async fn lookup_when_node_has_announced_peer_then_returns_peer_and_token() {
      let server_id = NodeId::from_bytes([1; DHT_ID_LEN]);
      let info_hash = NodeId::from_bytes([3; DHT_ID_LEN]);
      let settings = DhtSettings {
         bind_addr: "127.0.0.1:0".parse().unwrap(),
         bootstrap_nodes: Vec::new(),
         query_timeout: Duration::from_secs(1),
         receive_buffer_size: TEST_BUFFER_SIZE,
         ..DhtSettings::default()
      };
      let server = DhtActor::spawn(DhtActorArgs {
         id: Some(server_id),
         settings,
      });
      let server_addr = server.ask(LocalAddr).await.unwrap();
      let client = DhtTransport::bind(
         "127.0.0.1:0".parse().unwrap(),
         Duration::from_secs(1),
         TEST_BUFFER_SIZE,
      )
      .await
      .unwrap();
      let receiver = client.clone();
      let receive_task = tokio::spawn(async move {
         loop {
            let Ok((message, addr)) = receiver.receive().await else {
               break;
            };
            receiver.complete(&message, addr).await;
         }
      });
      let client_id = NodeId::from_bytes([2; DHT_ID_LEN]);
      let token = client
         .query(
            server_addr,
            Query::GetPeers {
               id: client_id,
               info_hash,
            },
         )
         .await
         .unwrap()
         .token
         .unwrap();
      client
         .query(
            server_addr,
            Query::AnnouncePeer {
               id: client_id,
               info_hash,
               port: TEST_PEER_PORT,
               token,
               implied_port: false,
            },
         )
         .await
         .unwrap();

      let result = lookup_peers(
         client,
         client_id,
         info_hash,
         vec![Contact::new(server_id, server_addr)],
         TEST_LOOKUP_CONCURRENCY,
         TEST_PEER_LIMIT,
         TEST_BUCKET_SIZE,
      )
      .await;

      assert_eq!(result.peers.len(), 1);
      assert_eq!(result.peers[0].port(), TEST_PEER_PORT);
      assert_eq!(result.announce_candidates.len(), 1);
      receive_task.abort();
      server.kill();
   }
}
