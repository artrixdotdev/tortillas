use std::{io::SeekFrom, path::PathBuf};

use bytes::Bytes;
use tokio::{
   fs::File,
   io::{AsyncReadExt, AsyncSeekExt},
};
use tracing::{debug, info, trace, warn};

use super::TorrentActor;
use crate::{
   errors::TorrentError,
   peer::PeerTell,
   pieces::{PieceManager, PieceStoreMessage, PieceStoreRequest},
   torrent::{BLOCK_SIZE, PieceStorageStrategy, TorrentState},
   tracker::{Event, TrackerMessage, TrackerUpdate},
};

impl TorrentActor {
   /// Handles an incoming piece block from a peer.
   pub async fn incoming_piece(
      &mut self, peer_id: crate::peer::PeerId, index: usize, offset: usize, block: Bytes,
   ) {
      let info_dict = match &self.info {
         Some(info) => info,
         None => {
            warn!("Received piece block before info dict was available");
            return;
         }
      };

      let piece_length = info_dict.piece_length as usize;
      let total_length = info_dict.total_length();
      let last_piece_index = info_dict.piece_count().saturating_sub(1);

      // Compute concrete length for this specific piece
      let concrete_piece_len = if index == last_piece_index {
         if total_length % piece_length == 0 {
            piece_length
         } else {
            total_length % piece_length
         }
      } else {
         piece_length
      };

      // Validate offset is within bounds
      if offset >= concrete_piece_len {
         warn!(index, offset, concrete_piece_len, "Received piece block with offset out of bounds");
         return;
      }

      // Validate block is non-empty
      if block.is_empty() {
         warn!(index, offset, "Received piece block with zero length");
         return;
      }

      // Validate that offset + block.len() does not exceed piece bounds
      if let Some(end) = offset.checked_add(block.len()) {
         if end > concrete_piece_len {
            warn!(index, offset, block_len = block.len(), concrete_piece_len, "Received piece block that exceeds piece bounds");
            return;
         }
      } else {
         warn!(index, offset, block_len = block.len(), "Offset + block length overflows");
         return;
      }

      let expected_blocks = concrete_piece_len.div_ceil(BLOCK_SIZE);
      let block_index = offset / BLOCK_SIZE;
      if block_index >= expected_blocks {
         warn!(index, offset, block_index, expected_blocks, "Received piece block with invalid block index");
         return;
      }

      if self.is_duplicate_block(index, block_index) {
         trace!("Received duplicate piece block");
         return;
      }

      let block_len = block.len();
      self.write_block_to_storage(index, offset, block).await;

      // Only mark block complete after successful write
      self
         .piece_scheduler
         .mark_block_complete(index, block_index, expected_blocks);

      self
         .broadcast_to_peers(PeerTell::CancelPiece(index, offset, block_len))
         .await;

      if self.is_piece_complete(index) {
         self.piece_completed(peer_id, index).await;
      } else {
         self.request_blocks_from_peer(peer_id, 1).await;
         trace!(%peer_id, "Requested replacement block from peer");
      }
   }

   pub(super) async fn request_blocks_from_peer(
      &mut self, peer_id: crate::peer::PeerId, limit: usize,
   ) {
      let Some(info) = self.info.as_ref() else {
         return;
      };
      let Some(peer) = self.peers.get(&peer_id).cloned() else {
         return;
      };
      let requests =
         self
            .piece_scheduler
            .requests_for_peer(peer_id, limit, info.piece_length as usize, info.total_length());
      for request in requests {
         if let Err(err) = peer
            .tell(PeerTell::NeedPiece(
               request.piece_index,
               request.offset(),
               request.length,
            ))
            .await
         {
            self
               .piece_scheduler
               .release_request(request.piece_index, request.offset());
            warn!(?err, %peer_id, "Failed to request block from peer");
         }
      }
   }

   async fn write_block_to_storage(&self, index: usize, offset: usize, block: Bytes) {
      match &self.piece_storage {
         PieceStorageStrategy::Disk(_) => {
            let Ok(path) = self.get_piece_path(index) else {
               warn!(piece_index = index, "Failed to get piece path");
               return;
            };
            match self
               .piece_store
               .ask(PieceStoreMessage::WriteBlock {
                  path,
                  offset,
                  block,
               })
               .await
            {
               Ok(()) => {}
               Err(err) => warn!(
                  ?err,
                  piece_index = index,
                  offset,
                  "Piece store failed to write block"
               ),
            }
         }
         PieceStorageStrategy::InFile => {
            warn!(
               piece_index = index,
               offset, "In-file block writes are not implemented yet"
            );
         }
      }
   }

   async fn piece_completed(&mut self, peer_id: crate::peer::PeerId, index: usize) {
      let previous_blocks = self.piece_scheduler.remove_piece_blocks(index);
      let info_dict = self
         .info_dict()
         .expect("Can't receive piece without info dict");
      let piece_count = info_dict.piece_count();
      let total_length = info_dict.total_length();

      if !self
         .validate_and_send_piece(peer_id, index, previous_blocks)
         .await
      {
         return;
      }

      self.piece_scheduler.mark_piece_complete(index);
      self.bitfield.set_aliased(index, true);

      debug!(
         piece_index = index,
         pieces_left = piece_count.saturating_sub(index + 1),
         "Piece is now complete"
      );

      self.broadcast_to_peers(PeerTell::Have(index)).await;

      if let Some(total_downloaded) = self.total_bytes_downloaded() {
         let total_bytes_left = total_length - total_downloaded;
         self
            .update_trackers(TrackerUpdate::Left(total_bytes_left))
            .await;
      }

      if self.piece_scheduler.next_piece() >= piece_count {
         self.state = TorrentState::Seeding;
         self
            .update_trackers(TrackerUpdate::Event(Event::Completed))
            .await;
         self.broadcast_to_trackers(TrackerMessage::Announce).await;
         info!("Torrenting process completed, switching to seeding mode");
      }
   }

   async fn validate_and_send_piece(
      &mut self, peer_id: crate::peer::PeerId, index: usize,
      previous_blocks: Option<bitvec::vec::BitVec>,
   ) -> bool {
      let info_dict = self
         .info_dict()
         .expect("Can't receive piece without info dict");

      match &self.piece_storage {
         PieceStorageStrategy::Disk(_) => {
            let Ok(path) = self.get_piece_path(index) else {
               warn!(index, "Failed to get piece path; re-requesting");
               return false;
            };

            let data = match self
               .piece_store
               .ask(PieceStoreRequest::ValidateAndRead {
                  path: path.clone(),
                  hash: info_dict.pieces[index],
               })
               .await
            {
               Ok(data) => data,
               Err(err) => {
                  warn!(?err, index, path = %path.display(), "Failed to validate piece through piece store actor; re-requesting");
                  return false;
               }
            };
            if let Err(err) = self.piece_manager.recv(index, data).await {
               warn!(?err, index, path = %path.display(), "Piece manager rejected piece; re-requesting");
               if let Some(blocks) = previous_blocks {
                  self.piece_scheduler.restore_piece_blocks(index, blocks);
               }
               self.request_blocks_from_peer(peer_id, 1).await;
               return false;
            }
         }
         PieceStorageStrategy::InFile => {
            warn!(
               index,
               "In-file piece validation is not implemented yet; re-requesting"
            );
            return false;
         }
      }
      true
   }

   fn is_duplicate_block(&self, index: usize, block_index: usize) -> bool {
      self.piece_scheduler.is_duplicate_block(index, block_index)
   }

   fn is_piece_complete(&self, index: usize) -> bool {
      self.piece_scheduler.is_piece_complete(index)
   }

   pub(super) fn get_piece_path(&self, index: usize) -> anyhow::Result<PathBuf> {
      let info_dict = self.info_dict().ok_or(TorrentError::MissingInfoDict)?;
      anyhow::ensure!(info_dict.pieces.len() > index, "Index out of bounds");

      let hash = info_dict.pieces[index];

      if let PieceStorageStrategy::Disk(path) = &self.piece_storage {
         let mut path = path.clone();
         path.push(format!("{hash}.piece"));
         Ok(path.to_path_buf())
      } else {
         anyhow::bail!("Piece storage strategy is not Disk")
      }
   }

   pub(super) async fn read_piece_block(
      &self, index: usize, offset: usize, length: usize,
   ) -> anyhow::Result<Bytes> {
      anyhow::ensure!(
         matches!(self.piece_storage, PieceStorageStrategy::Disk(_)),
         "in-file piece reads are not implemented yet"
      );

      let mut file = File::open(self.get_piece_path(index)?).await?;
      let mut buffer = vec![0; length];
      file.seek(SeekFrom::Start(offset as u64)).await?;
      file.read_exact(&mut buffer).await?;
      Ok(buffer.into())
   }
}
