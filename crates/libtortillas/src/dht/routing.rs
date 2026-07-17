use std::{collections::VecDeque, time::Instant};

use super::{Contact, NodeId};

const FAILURE_LIMIT: u8 = 2;

#[derive(Clone, Copy, Debug)]
struct RoutingEntry {
   contact: Contact,
   last_seen: Instant,
   failures: u8,
}

/// A 160-bucket Kademlia routing table.
#[derive(Debug)]
pub struct RoutingTable {
   local_id: NodeId,
   bucket_size: usize,
   buckets: Vec<VecDeque<RoutingEntry>>,
}

impl RoutingTable {
   pub fn new(local_id: NodeId, bucket_size: usize) -> Self {
      Self {
         local_id,
         bucket_size,
         buckets: (0..160).map(|_| VecDeque::new()).collect(),
      }
   }

   pub const fn local_id(&self) -> NodeId {
      self.local_id
   }

   pub fn len(&self) -> usize {
      self.buckets.iter().map(VecDeque::len).sum()
   }

   pub fn is_empty(&self) -> bool {
      self.len() == 0
   }

   /// Inserts a verified contact. Recently responsive nodes stay at the back
   /// of their bucket and questionable nodes are evicted before live nodes.
   pub fn insert(&mut self, contact: Contact) -> bool {
      let Some(index) = self.local_id.distance(contact.id).bucket_index() else {
         return false;
      };
      let bucket = &mut self.buckets[index];
      if let Some(position) = bucket
         .iter()
         .position(|entry| entry.contact.id == contact.id)
      {
         bucket.remove(position);
         bucket.push_back(RoutingEntry {
            contact,
            last_seen: Instant::now(),
            failures: 0,
         });
         return true;
      }
      if bucket.len() >= self.bucket_size {
         if let Some(position) = bucket.iter().position(|entry| entry.failures > 0) {
            bucket.remove(position);
         } else {
            return false;
         }
      }
      bucket.push_back(RoutingEntry {
         contact,
         last_seen: Instant::now(),
         failures: 0,
      });
      true
   }

   /// Records a failed query and removes a contact after repeated failures.
   pub fn record_failure(&mut self, id: NodeId) {
      let Some(index) = self.local_id.distance(id).bucket_index() else {
         return;
      };
      let bucket = &mut self.buckets[index];
      let Some(position) = bucket.iter().position(|entry| entry.contact.id == id) else {
         return;
      };
      bucket[position].failures = bucket[position].failures.saturating_add(1);
      if bucket[position].failures >= FAILURE_LIMIT {
         bucket.remove(position);
      }
   }

   /// Returns the closest known contacts ordered by XOR distance.
   pub fn closest(&self, target: NodeId, limit: usize) -> Vec<Contact> {
      let mut contacts = self
         .buckets
         .iter()
         .flat_map(|bucket| bucket.iter())
         .map(|entry| entry.contact)
         .collect::<Vec<_>>();
      contacts.sort_unstable_by_key(|contact| target.distance(contact.id));
      contacts.truncate(limit);
      contacts
   }

   /// Returns contacts that have not responded since the supplied instant.
   pub fn stale(&self, before: Instant) -> Vec<Contact> {
      self
         .buckets
         .iter()
         .flat_map(|bucket| bucket.iter())
         .filter(|entry| entry.last_seen < before)
         .map(|entry| entry.contact)
         .collect()
   }
}

#[cfg(test)]
mod tests {
   use std::net::SocketAddr;

   use super::*;

   fn contact(id: [u8; 20], port: u16) -> Contact {
      Contact::new(
         NodeId::from_bytes(id),
         SocketAddr::from(([127, 0, 0, 1], port)),
      )
   }

   #[test]
   fn routing_table_when_contacts_are_added_then_returns_closest_first() {
      let mut table = RoutingTable::new(NodeId::from_bytes([0; 20]), 8);
      let mut near = [0; 20];
      near[19] = 1;
      let mut far = [0; 20];
      far[0] = 0x80;
      table.insert(contact(far, 1));
      table.insert(contact(near, 2));

      assert_eq!(
         table.closest(NodeId::from_bytes([0; 20]), 2),
         vec![contact(near, 2), contact(far, 1)]
      );
   }

   #[test]
   fn routing_table_when_bucket_is_full_then_retains_responsive_contacts() {
      let mut table = RoutingTable::new(NodeId::from_bytes([0; 20]), 1);
      let mut first = [0; 20];
      first[19] = 2;
      let mut second = [0; 20];
      second[19] = 3;

      assert!(table.insert(contact(first, 1)));
      assert!(!table.insert(contact(second, 2)));
      assert_eq!(
         table.closest(NodeId::from_bytes([0; 20]), 2),
         vec![contact(first, 1)]
      );
   }

   #[test]
   fn routing_table_when_contact_repeatedly_fails_then_removes_it() {
      let mut table = RoutingTable::new(NodeId::from_bytes([0; 20]), 8);
      let mut id = [0; 20];
      id[19] = 1;
      let node = contact(id, 1);
      table.insert(node);

      table.record_failure(node.id);
      assert_eq!(table.len(), 1);
      table.record_failure(node.id);
      assert!(table.is_empty());
   }
}
