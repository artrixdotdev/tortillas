use futures::{StreamExt, stream};

use super::{Contact, DhtTransport, NodeId, Query};

/// Announces the local peer to DHT nodes that returned valid lookup tokens.
///
/// Tokens are scoped to a querying IP by [BEP 5], so a lookup must precede
/// every announcement cycle.
///
/// [BEP 5]: https://www.bittorrent.org/beps/bep_0005.html#announce-peer
pub async fn announce_peer(
   transport: DhtTransport, id: NodeId, info_hash: NodeId, port: u16,
   candidates: Vec<(Contact, Vec<u8>)>, concurrency: usize,
) -> usize {
   if port == 0 {
      return 0;
   }
   stream::iter(candidates)
      .map(|(contact, token)| {
         let transport = transport.clone();
         async move {
            transport
               .query(
                  contact.addr,
                  Query::AnnouncePeer {
                     id,
                     info_hash,
                     port,
                     token,
                     implied_port: false,
                  },
               )
               .await
               .is_ok()
         }
      })
      .buffer_unordered(concurrency.max(1))
      .filter(|announced| std::future::ready(*announced))
      .count()
      .await
}

#[cfg(test)]
mod tests {
   use std::time::Duration;

   use kameo::actor::Spawn;

   use super::*;
   use crate::{
      dht::{
         DHT_ID_LEN, DhtActor, actor::DhtActorArgs, decode_peers, messages::commands::LocalAddr,
      },
      settings::DhtSettings,
   };

   const TEST_BUFFER_SIZE: usize = 2048;
   const TEST_CONCURRENCY: usize = 1;
   const TEST_PEER_PORT: u16 = 6881;

   #[tokio::test]
   async fn announce_when_token_is_valid_then_node_stores_local_peer() {
      let server_id = NodeId::from_bytes([1; DHT_ID_LEN]);
      let client_id = NodeId::from_bytes([2; DHT_ID_LEN]);
      let info_hash = NodeId::from_bytes([3; DHT_ID_LEN]);
      let server = DhtActor::spawn(DhtActorArgs {
         id: Some(server_id),
         settings: DhtSettings {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            bootstrap_nodes: Vec::new(),
            query_timeout: Duration::from_secs(1),
            receive_buffer_size: TEST_BUFFER_SIZE,
            ..DhtSettings::default()
         },
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

      let count = announce_peer(
         client.clone(),
         client_id,
         info_hash,
         TEST_PEER_PORT,
         vec![(Contact::new(server_id, server_addr), token)],
         TEST_CONCURRENCY,
      )
      .await;
      let values = client
         .query(
            server_addr,
            Query::GetPeers {
               id: client_id,
               info_hash,
            },
         )
         .await
         .unwrap()
         .values
         .unwrap();
      let peers = decode_peers(values).unwrap();

      assert_eq!(count, 1);
      assert_eq!(peers[0].port(), TEST_PEER_PORT);
      receive_task.abort();
      server.kill();
   }
}
