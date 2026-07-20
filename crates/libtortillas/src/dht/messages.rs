use std::net::SocketAddr;

use kameo::messages;
use tracing::{trace, warn};

use super::{Contact, DhtActor, Message};

pub(crate) mod events {
   use super::*;

   #[messages]
   impl DhtActor {
      #[message]
      pub(crate) async fn incoming_datagram(&mut self, message: Message, addr: SocketAddr) {
         match message {
            Message::Query {
               transaction_id,
               query,
            } => {
               let reply = match self.state.handle_query(query, addr) {
                  Ok(response) => Message::Response {
                     transaction_id,
                     response,
                  },
                  Err(error) => Message::Error {
                     transaction_id,
                     error,
                  },
               };
               if let Err(err) = self.transport.send(&reply, addr).await {
                  warn!(error = %err, %addr, "Failed to reply to DHT query");
               }
            }
            Message::Response {
               transaction_id,
               response,
            } => {
               let contact_id = response.id;
               let message = Message::Response {
                  transaction_id,
                  response,
               };
               if self.transport.complete(&message, addr).await {
                  self
                     .state
                     .routing_mut()
                     .insert(Contact::new(contact_id, addr));
               } else {
                  trace!(%addr, "Ignored unmatched DHT response");
               }
            }
            Message::Error {
               transaction_id,
               error,
            } => {
               let message = Message::Error {
                  transaction_id,
                  error,
               };
               if !self.transport.complete(&message, addr).await {
                  trace!(%addr, "Ignored unmatched DHT error");
               }
            }
         }
      }
   }
}

#[cfg(test)]
pub(crate) mod commands {
   use super::*;

   #[messages]
   impl DhtActor {
      #[message]
      pub(crate) fn local_addr(&self) -> std::io::Result<SocketAddr> {
         self.transport.local_addr()
      }
   }
}

#[cfg(test)]
mod tests {
   use std::time::Duration;

   use kameo::actor::Spawn;

   use super::*;
   use crate::{
      dht::{
         DHT_ID_LEN, DhtTransport, NodeId, Query, Response, actor::DhtActorArgs,
         messages::commands::LocalAddr,
      },
      settings::DhtSettings,
   };

   const TEST_BUFFER_SIZE: usize = 2048;

   #[tokio::test]
   async fn dht_actor_when_ping_arrives_then_replies_with_local_id() {
      let settings = DhtSettings {
         bind_addr: "127.0.0.1:0".parse().unwrap(),
         query_timeout: Duration::from_secs(1),
         receive_buffer_size: TEST_BUFFER_SIZE,
         ..DhtSettings::default()
      };
      let actor_id = NodeId::from_bytes([1; DHT_ID_LEN]);
      let actor = DhtActor::spawn(DhtActorArgs {
         id: Some(actor_id),
         settings,
      });
      let actor_addr = actor.ask(LocalAddr).await.unwrap();
      let client = DhtTransport::bind(
         "127.0.0.1:0".parse().unwrap(),
         Duration::from_secs(1),
         TEST_BUFFER_SIZE,
      )
      .await
      .unwrap();
      let receiver = client.clone();
      let receive_task = tokio::spawn(async move {
         let (message, addr) = receiver.receive().await.unwrap();
         receiver.complete(&message, addr).await;
      });

      let response = client
         .query(
            actor_addr,
            Query::Ping {
               id: NodeId::from_bytes([2; DHT_ID_LEN]),
            },
         )
         .await
         .unwrap();

      assert_eq!(response, Response::pong(actor_id));
      receive_task.await.unwrap();
      actor.kill();
   }
}
