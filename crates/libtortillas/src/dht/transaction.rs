use std::{collections::HashMap, net::SocketAddr};

use tokio::sync::oneshot;

use super::{DhtError, Response};

const TRANSACTION_ID_LEN: usize = 2;

/// Result delivered to the query that owns a KRPC transaction ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionResult {
   Response(Response),
   Error(DhtError),
}

struct PendingTransaction {
   addr: SocketAddr,
   reply: oneshot::Sender<TransactionResult>,
}

/// Correlates concurrent UDP responses with their originating queries.
///
/// The source endpoint is checked as well as the transaction ID so an
/// unrelated datagram cannot complete another node's query.
#[derive(Default)]
pub struct TransactionTable {
   next_id: u16,
   pending: HashMap<Vec<u8>, PendingTransaction>,
}

impl TransactionTable {
   pub fn new() -> Self {
      Self {
         next_id: rand::random(),
         pending: HashMap::new(),
      }
   }

   pub fn begin(&mut self, addr: SocketAddr) -> (Vec<u8>, oneshot::Receiver<TransactionResult>) {
      let (reply, receiver) = oneshot::channel();
      loop {
         let transaction_id = self.next_id.to_be_bytes().to_vec();
         self.next_id = self.next_id.wrapping_add(1);
         if self.pending.contains_key(&transaction_id) {
            continue;
         }
         debug_assert_eq!(transaction_id.len(), TRANSACTION_ID_LEN);
         self
            .pending
            .insert(transaction_id.clone(), PendingTransaction { addr, reply });
         return (transaction_id, receiver);
      }
   }

   pub fn complete(
      &mut self, transaction_id: &[u8], addr: SocketAddr, result: TransactionResult,
   ) -> bool {
      let Some(pending) = self.pending.get(transaction_id) else {
         return false;
      };
      if pending.addr != addr {
         return false;
      }
      let pending = self
         .pending
         .remove(transaction_id)
         .expect("transaction was checked before removal");
      pending.reply.send(result).is_ok()
   }

   pub fn cancel(&mut self, transaction_id: &[u8]) -> bool {
      self.pending.remove(transaction_id).is_some()
   }

   pub fn len(&self) -> usize {
      self.pending.len()
   }

   pub fn is_empty(&self) -> bool {
      self.pending.is_empty()
   }
}

#[cfg(test)]
mod tests {
   use super::*;
   use crate::dht::{DHT_ID_LEN, NodeId};

   #[tokio::test]
   async fn transaction_when_response_matches_endpoint_then_completes_query() {
      let mut table = TransactionTable::new();
      let addr = "127.0.0.1:6881".parse().unwrap();
      let (transaction_id, receiver) = table.begin(addr);
      let response = Response::pong(NodeId::from_bytes([1; DHT_ID_LEN]));

      assert!(table.complete(
         &transaction_id,
         addr,
         TransactionResult::Response(response.clone())
      ));
      assert_eq!(
         receiver.await.unwrap(),
         TransactionResult::Response(response)
      );
      assert!(table.is_empty());
   }

   #[test]
   fn transaction_when_endpoint_differs_then_ignores_response() {
      let mut table = TransactionTable::new();
      let addr = "127.0.0.1:6881".parse().unwrap();
      let other_addr = "127.0.0.1:6882".parse().unwrap();
      let (transaction_id, _receiver) = table.begin(addr);

      assert!(!table.complete(
         &transaction_id,
         other_addr,
         TransactionResult::Response(Response::pong(NodeId::from_bytes([1; DHT_ID_LEN])))
      ));
      assert_eq!(table.len(), 1);
   }

   #[test]
   fn transaction_when_cancelled_then_drops_pending_query() {
      let mut table = TransactionTable::new();
      let (transaction_id, _receiver) = table.begin("127.0.0.1:6881".parse().unwrap());

      assert!(table.cancel(&transaction_id));
      assert!(table.is_empty());
   }
}
