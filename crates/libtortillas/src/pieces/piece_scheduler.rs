use std::collections::HashMap;

use bitvec::vec::BitVec;

use crate::{
   peer::PeerId,
   torrent::{BLOCK_SIZE, BlockMap},
};

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
   in_flight: HashMap<(usize, usize), PeerId>,
   next_piece: usize,
}

impl PieceScheduler {
   pub(crate) fn new(piece_count: usize) -> Self {
      Self {
         completed_pieces: BitVec::repeat(false, piece_count),
         completed_blocks: HashMap::new(),
         in_flight: HashMap::new(),
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

   #[cfg(test)]
   pub(crate) fn set_piece_blocks(&mut self, index: usize, blocks: BitVec) {
      self.completed_blocks.insert(index, blocks);
   }

   pub(crate) fn mark_block_complete(
      &mut self, piece_index: usize, block_index: usize, total_blocks: usize,
   ) {
      let blocks = self.completed_blocks.entry(piece_index).or_insert_with(|| {
         let mut blocks = BitVec::with_capacity(total_blocks);
         blocks.resize(total_blocks, false);
         blocks
      });
      if block_index < blocks.len() {
         blocks.set(block_index, true);
      }
      self.in_flight.remove(&(piece_index, block_index));
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

      let last_piece_index = self.completed_pieces.len().saturating_sub(1);
      let last_piece_len = if total_length.is_multiple_of(piece_length) {
         piece_length
      } else {
         total_length % piece_length
      };

      for piece_index in self.next_piece..self.completed_pieces.len() {
         if self.completed_pieces[piece_index] {
            continue;
         }

         // Compute per-piece length
         let piece_len = if piece_index == last_piece_index {
            last_piece_len
         } else {
            piece_length
         };
         let total_blocks = piece_len.div_ceil(BLOCK_SIZE);

         for block_index in 0..total_blocks {
            let key = (piece_index, block_index);
            if self.in_flight.contains_key(&key)
               || self
                  .completed_blocks
                  .get(&piece_index)
                  .and_then(|blocks| blocks.get(block_index).as_deref().copied())
                  .unwrap_or(false)
            {
               continue;
            }

            self.in_flight.insert(key, peer_id);
            requests.push(self.block_request(piece_index, block_index, piece_len));
            if requests.len() >= limit {
               return requests;
            }
         }
      }

      requests
   }

   pub(crate) fn peer_disconnected(&mut self, peer_id: PeerId) {
      self.in_flight.retain(|_, owner| *owner != peer_id);
   }

   pub(crate) fn release_request(&mut self, piece_index: usize, offset: usize) {
      self.in_flight.remove(&(piece_index, offset / BLOCK_SIZE));
   }

   pub(crate) fn block_map_export(&self) -> BlockMap {
      let block_map = BlockMap::new();
      for (piece, blocks) in &self.completed_blocks {
         block_map.insert(*piece, blocks.clone());
      }
      block_map
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
}
