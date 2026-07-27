use std::{
   collections::HashMap,
   sync::{Arc, atomic::AtomicU8},
   time::{Duration, Instant},
};

use bitvec::vec::BitVec;

use crate::{peer::PeerId, torrent::BLOCK_SIZE};

#[derive(Debug)]
pub(crate) struct BlockRequest {
   pub(crate) piece_index: usize,
   pub(crate) block_index: usize,
   pub(crate) length: usize,
}

impl BlockRequest {
   pub(crate) fn offset(&self) -> usize {
      self.block_index * BLOCK_SIZE
   }
}

#[derive(Debug)]
pub(crate) struct PieceScheduler {
   completed_pieces: BitVec,
   completed_blocks: HashMap<usize, BitVec>,
   in_flight: HashMap<usize, Vec<Option<InFlightBlock>>>,
   in_flight_per_peer: HashMap<PeerId, usize>,
   peer_availability: HashMap<PeerId, Arc<BitVec<AtomicU8>>>,
   request_cursors: HashMap<PeerId, usize>,
   next_piece: usize,
}

#[derive(Debug)]
struct InFlightBlock {
   peer_id: PeerId,
   requested_at: Instant,
}

impl PieceScheduler {
   pub(crate) fn new(piece_count: usize) -> Self {
      Self {
         completed_pieces: BitVec::repeat(false, piece_count),
         completed_blocks: HashMap::new(),
         in_flight: HashMap::new(),
         in_flight_per_peer: HashMap::new(),
         peer_availability: HashMap::new(),
         request_cursors: HashMap::new(),
         next_piece: 0,
      }
   }

   pub(crate) fn next_piece(&self) -> usize {
      self.next_piece
   }

   pub(crate) fn mark_piece_complete(&mut self, index: usize) {
      self.completed_blocks.remove(&index);
      if index < self.completed_pieces.len() {
         self.completed_pieces.set(index, true);
      }
      while self.next_piece < self.completed_pieces.len() && self.completed_pieces[self.next_piece]
      {
         self.next_piece += 1;
      }
   }

   pub(crate) fn restore_piece_blocks(&mut self, index: usize, blocks: BitVec) {
      self.completed_blocks.insert(index, blocks);
   }

   #[cfg(all(test, feature = "live"))]
   pub(crate) fn set_piece_blocks(&mut self, index: usize, blocks: BitVec) {
      self.completed_blocks.insert(index, blocks);
   }

   pub(crate) fn mark_block_complete(
      &mut self, piece_index: usize, block_index: usize, total_blocks: usize,
   ) -> Option<PeerId> {
      let blocks = self.completed_blocks.entry(piece_index).or_insert_with(|| {
         let mut blocks = BitVec::with_capacity(total_blocks);
         blocks.resize(total_blocks, false);
         blocks
      });
      if block_index < blocks.len() {
         blocks.set(block_index, true);
      }
      let request = self.remove_in_flight(piece_index, block_index)?;
      self.decrement_in_flight(request.peer_id);
      Some(request.peer_id)
   }

   pub(crate) fn remove_piece_blocks(&mut self, piece_index: usize) -> Option<BitVec> {
      self.completed_blocks.remove(&piece_index)
   }

   pub(crate) fn is_duplicate_block(&self, piece_index: usize, block_index: usize) -> bool {
      self
         .completed_blocks
         .get(&piece_index)
         .and_then(|blocks| blocks.get(block_index).as_deref().copied())
         .unwrap_or(false)
   }

   pub(crate) fn is_piece_complete(&self, piece_index: usize) -> bool {
      self
         .completed_blocks
         .get(&piece_index)
         .map(|blocks| blocks.iter().all(|b| *b))
         .unwrap_or(false)
   }

   pub(crate) fn requests_for_peer(
      &mut self, peer_id: PeerId, limit: usize, piece_length: usize, total_length: usize,
   ) -> Vec<BlockRequest> {
      let mut requests = Vec::new();

      // Early guard for zero limit
      if limit == 0 {
         return requests;
      }

      let piece_count = self.completed_pieces.len();
      if piece_count == 0 {
         return requests;
      }

      let Some(available_pieces) = self.peer_availability.get(&peer_id) else {
         return requests;
      };

      let blocks_per_piece = piece_length.div_ceil(BLOCK_SIZE);
      if blocks_per_piece == 0 {
         return requests;
      }

      let last_piece_index = piece_count - 1;
      let last_piece_len = if total_length.is_multiple_of(piece_length) {
         piece_length
      } else {
         total_length % piece_length
      };
      let first_slot = self.next_piece.saturating_mul(blocks_per_piece);
      let total_slots = last_piece_index
         .saturating_mul(blocks_per_piece)
         .saturating_add(last_piece_len.div_ceil(BLOCK_SIZE));
      let cursor = self
         .request_cursors
         .get(&peer_id)
         .copied()
         .unwrap_or(first_slot)
         .clamp(first_slot, total_slots);

      for slot in (cursor..total_slots).chain(first_slot..cursor) {
         let piece_index = slot / blocks_per_piece;
         let block_index = slot % blocks_per_piece;
         if self.completed_pieces[piece_index]
            || !available_pieces
               .get(piece_index)
               .as_deref()
               .copied()
               .unwrap_or(false)
         {
            continue;
         }

         // Compute per-piece length
         let piece_len = if piece_index == last_piece_index {
            last_piece_len
         } else {
            piece_length
         };
         let total_blocks = piece_len.div_ceil(BLOCK_SIZE);
         if block_index >= total_blocks {
            continue;
         }

         if self.is_in_flight(piece_index, block_index)
            || self
               .completed_blocks
               .get(&piece_index)
               .and_then(|blocks| blocks.get(block_index).as_deref().copied())
               .unwrap_or(false)
         {
            continue;
         }

         let blocks = self.in_flight.entry(piece_index).or_default();
         if blocks.len() <= block_index {
            blocks.resize_with(block_index + 1, || None);
         }
         blocks[block_index] = Some(InFlightBlock {
            peer_id,
            requested_at: Instant::now(),
         });
         *self.in_flight_per_peer.entry(peer_id).or_default() += 1;
         let next_cursor = slot.saturating_add(1);
         self.request_cursors.insert(
            peer_id,
            if next_cursor < total_slots {
               next_cursor
            } else {
               first_slot
            },
         );
         requests.push(self.block_request(piece_index, block_index, piece_len));
         if requests.len() >= limit {
            return requests;
         }
      }

      requests
   }

   pub(crate) fn in_flight_for_peer(&self, peer_id: PeerId) -> usize {
      self.in_flight_per_peer.get(&peer_id).copied().unwrap_or(0)
   }

   pub(crate) fn update_peer_availability(
      &mut self, peer_id: PeerId, available_pieces: Arc<BitVec<AtomicU8>>,
   ) {
      self.peer_availability.insert(peer_id, available_pieces);
   }

   pub(crate) fn peer_disconnected(&mut self, peer_id: PeerId) {
      self.in_flight.retain(|_, blocks| {
         for block in blocks.iter_mut() {
            if block
               .as_ref()
               .is_some_and(|request| request.peer_id == peer_id)
            {
               *block = None;
            }
         }
         blocks.iter().any(Option::is_some)
      });
      self.in_flight_per_peer.remove(&peer_id);
      self.peer_availability.remove(&peer_id);
      self.request_cursors.remove(&peer_id);
   }

   pub(crate) fn release_stale_requests(&mut self, timeout: Duration) -> usize {
      let now = Instant::now();
      let mut released = 0;
      let mut in_flight_per_peer = HashMap::new();
      self.in_flight.retain(|_, blocks| {
         for block in blocks.iter_mut() {
            let Some(request) = block else {
               continue;
            };
            if now.saturating_duration_since(request.requested_at) >= timeout {
               *block = None;
               released += 1;
            } else {
               *in_flight_per_peer.entry(request.peer_id).or_default() += 1;
            }
         }
         blocks.iter().any(Option::is_some)
      });
      self.in_flight_per_peer = in_flight_per_peer;
      released
   }

   pub(crate) fn release_peer_request(
      &mut self, peer_id: PeerId, piece_index: usize, offset: usize,
   ) {
      if self
         .in_flight
         .get(&piece_index)
         .and_then(|blocks| blocks.get(offset / BLOCK_SIZE))
         .and_then(Option::as_ref)
         .is_some_and(|request| request.peer_id == peer_id)
      {
         self.remove_in_flight(piece_index, offset / BLOCK_SIZE);
         self.decrement_in_flight(peer_id);
      }
   }

   pub(crate) fn completed_blocks(&self) -> &HashMap<usize, BitVec> {
      &self.completed_blocks
   }

   fn block_request(
      &self, piece_index: usize, block_index: usize, piece_length: usize,
   ) -> BlockRequest {
      let offset = block_index * BLOCK_SIZE;
      let length = if offset + BLOCK_SIZE > piece_length {
         piece_length - offset
      } else {
         BLOCK_SIZE
      };

      BlockRequest {
         piece_index,
         block_index,
         length,
      }
   }

   fn decrement_in_flight(&mut self, peer_id: PeerId) {
      let remove = self
         .in_flight_per_peer
         .get_mut(&peer_id)
         .is_some_and(|count| {
            *count = count.saturating_sub(1);
            *count == 0
         });
      if remove {
         self.in_flight_per_peer.remove(&peer_id);
      }
   }

   fn is_in_flight(&self, piece_index: usize, block_index: usize) -> bool {
      self
         .in_flight
         .get(&piece_index)
         .and_then(|blocks| blocks.get(block_index))
         .is_some_and(Option::is_some)
   }

   fn remove_in_flight(&mut self, piece_index: usize, block_index: usize) -> Option<InFlightBlock> {
      let (request, piece_has_requests) = {
         let blocks = self.in_flight.get_mut(&piece_index)?;
         let request = blocks.get_mut(block_index)?.take()?;
         (request, blocks.iter().any(Option::is_some))
      };
      if !piece_has_requests {
         self.in_flight.remove(&piece_index);
      }
      Some(request)
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn scheduler_assigns_only_pieces_available_from_peer() {
      let peer_id = PeerId::Unknown([1; 20]);
      let mut scheduler = PieceScheduler::new(3);
      scheduler.update_peer_availability(
         peer_id,
         Arc::new([false, true, false].into_iter().collect()),
      );

      let requests = scheduler.requests_for_peer(peer_id, 4, BLOCK_SIZE, BLOCK_SIZE * 3);

      assert_eq!(requests.len(), 1);
      assert_eq!(requests[0].piece_index, 1);
   }

   #[test]
   fn scheduler_releases_unanswered_requests_after_timeout() {
      let peer_id = PeerId::Unknown([2; 20]);
      let mut scheduler = PieceScheduler::new(1);
      scheduler.update_peer_availability(peer_id, Arc::new([true].into_iter().collect()));
      assert_eq!(
         scheduler
            .requests_for_peer(peer_id, 1, BLOCK_SIZE, BLOCK_SIZE)
            .len(),
         1
      );

      assert_eq!(scheduler.release_stale_requests(Duration::ZERO), 1);
      assert_eq!(scheduler.in_flight_for_peer(peer_id), 0);
   }

   #[test]
   fn scheduler_when_request_is_released_then_wraps_cursor() {
      let peer_id = PeerId::Unknown([5; 20]);
      let mut scheduler = PieceScheduler::new(2);
      scheduler.update_peer_availability(peer_id, Arc::new([true, false].into_iter().collect()));
      assert_eq!(
         scheduler
            .requests_for_peer(peer_id, 1, BLOCK_SIZE, BLOCK_SIZE * 2)
            .len(),
         1
      );

      scheduler.release_peer_request(peer_id, 0, 0);
      let requests = scheduler.requests_for_peer(peer_id, 1, BLOCK_SIZE, BLOCK_SIZE * 2);

      assert_eq!(requests.len(), 1);
      assert_eq!(requests[0].piece_index, 0);
      assert_eq!(requests[0].block_index, 0);
   }

   #[test]
   fn late_rejection_does_not_release_reassigned_request() {
      let original_peer = PeerId::Unknown([3; 20]);
      let replacement_peer = PeerId::Unknown([4; 20]);
      let mut scheduler = PieceScheduler::new(1);
      let availability: Arc<BitVec<AtomicU8>> = Arc::new([true].into_iter().collect());
      scheduler.update_peer_availability(original_peer, availability.clone());
      scheduler.update_peer_availability(replacement_peer, availability);
      assert_eq!(
         scheduler
            .requests_for_peer(original_peer, 1, BLOCK_SIZE, BLOCK_SIZE)
            .len(),
         1
      );
      scheduler.release_stale_requests(Duration::ZERO);
      assert_eq!(
         scheduler
            .requests_for_peer(replacement_peer, 1, BLOCK_SIZE, BLOCK_SIZE)
            .len(),
         1
      );

      scheduler.release_peer_request(original_peer, 0, 0);

      assert_eq!(scheduler.in_flight_for_peer(original_peer), 0);
      assert_eq!(scheduler.in_flight_for_peer(replacement_peer), 1);
      assert_eq!(
         scheduler.mark_block_complete(0, 0, 1),
         Some(replacement_peer)
      );
   }
}
