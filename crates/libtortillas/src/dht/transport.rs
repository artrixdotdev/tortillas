use std::{io, net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use tokio::{net::UdpSocket, sync::Mutex, time::timeout};

use super::{Message, Query, Response, TransactionResult, TransactionTable};

/// Cloneable UDP transport shared by the DHT actor and outbound lookup tasks.
///
/// Encoding remains in [`Message`], matching the peer protocol's separation
/// between wire messages and stream I/O.
#[derive(Clone)]
pub struct DhtTransport {
   socket: Arc<UdpSocket>,
   transactions: Arc<Mutex<TransactionTable>>,
   query_timeout: Duration,
   receive_buffer_size: usize,
}

impl DhtTransport {
   pub async fn bind(
      addr: SocketAddr, query_timeout: Duration, receive_buffer_size: usize,
   ) -> io::Result<Self> {
      let socket = UdpSocket::bind(addr).await?;
      Ok(Self {
         socket: Arc::new(socket),
         transactions: Arc::new(Mutex::new(TransactionTable::new())),
         query_timeout,
         receive_buffer_size,
      })
   }

   #[cfg(test)]
   pub fn local_addr(&self) -> io::Result<SocketAddr> {
      self.socket.local_addr()
   }

   pub async fn send(&self, message: &Message, addr: SocketAddr) -> Result<()> {
      let bytes = message.encode()?;
      self
         .socket
         .send_to(&bytes, addr)
         .await
         .with_context(|| format!("failed to send DHT datagram to {addr}"))?;
      Ok(())
   }

   pub async fn query(&self, addr: SocketAddr, query: Query) -> Result<Response> {
      let (transaction_id, receiver) = self.transactions.lock().await.begin(addr);
      let message = Message::Query {
         transaction_id: transaction_id.clone(),
         query,
      };
      if let Err(error) = self.send(&message, addr).await {
         self.transactions.lock().await.cancel(&transaction_id);
         return Err(error);
      }

      let result = match timeout(self.query_timeout, receiver).await {
         Ok(Ok(result)) => result,
         Ok(Err(_)) => return Err(anyhow!("DHT response channel closed")),
         Err(_) => {
            self.transactions.lock().await.cancel(&transaction_id);
            bail!("DHT query to {addr} timed out");
         }
      };
      match result {
         TransactionResult::Response(response) => Ok(response),
         TransactionResult::Error(error) => {
            bail!("DHT node returned error {}: {}", error.code, error.message)
         }
      }
   }

   pub async fn receive(&self) -> Result<(Message, SocketAddr)> {
      let mut bytes = vec![0; self.receive_buffer_size];
      let (len, addr) = self.socket.recv_from(&mut bytes).await?;
      let message = Message::decode(&bytes[..len])?;
      Ok((message, addr))
   }

   /// Completes a pending query when the message is a response or error.
   pub async fn complete(&self, message: &Message, addr: SocketAddr) -> bool {
      let (transaction_id, result) = match message {
         Message::Response {
            transaction_id,
            response,
         } => (
            transaction_id,
            TransactionResult::Response(response.clone()),
         ),
         Message::Error {
            transaction_id,
            error,
         } => (transaction_id, TransactionResult::Error(error.clone())),
         Message::Query { .. } => return false,
      };
      self
         .transactions
         .lock()
         .await
         .complete(transaction_id, addr, result)
   }
}

#[cfg(test)]
mod tests {
   use super::*;
   use crate::dht::{DHT_ID_LEN, NodeId};

   const TEST_BUFFER_SIZE: usize = 2048;

   async fn transport() -> DhtTransport {
      DhtTransport::bind(
         "127.0.0.1:0".parse().unwrap(),
         Duration::from_secs(1),
         TEST_BUFFER_SIZE,
      )
      .await
      .unwrap()
   }

   #[tokio::test]
   async fn transport_when_local_node_replies_then_completes_query() {
      let client = transport().await;
      let server = transport().await;
      let server_addr = server.local_addr().unwrap();
      let server_id = NodeId::from_bytes([2; DHT_ID_LEN]);
      let client_receiver = client.clone();
      let receive_task = tokio::spawn(async move {
         let (message, addr) = client_receiver.receive().await.unwrap();
         client_receiver.complete(&message, addr).await
      });
      let server_task = tokio::spawn(async move {
         let (message, addr) = server.receive().await.unwrap();
         let transaction_id = message.transaction_id().to_vec();
         server
            .send(
               &Message::Response {
                  transaction_id,
                  response: Response::pong(server_id),
               },
               addr,
            )
            .await
            .unwrap();
      });

      let response = client
         .query(
            server_addr,
            Query::Ping {
               id: NodeId::from_bytes([1; DHT_ID_LEN]),
            },
         )
         .await
         .unwrap();

      assert_eq!(response, Response::pong(server_id));
      assert!(receive_task.await.unwrap());
      server_task.await.unwrap();
   }
}
