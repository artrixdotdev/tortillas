use std::{io::SeekFrom, path::PathBuf};

use bytes::Bytes;
use tokio::{
   fs::File,
   io::{AsyncReadExt, AsyncSeekExt},
};
use tracing::{debug, info, trace, warn};

use super::{TorrentActor, util};
#[cfg(all(test, feature = "live"))]
use crate::live::Hub;
use crate::{
   errors::TorrentError,
   peer::commands::{CancelPiece, Have, NeedPiece},
   pieces::{PieceManager, ValidateAndRead, WriteBlock},
   torrent::{BLOCK_SIZE, PieceStorageStrategy, TorrentState, actor::PieceManagerProxy},
   tracker::Event,
};

impl TorrentActor {
   /// Handles an incoming piece block from a peer.
   pub async fn handle_incoming_piece(
      &mut self, peer_id: crate::peer::PeerId, index: usize, offset: usize, block: Bytes,
   ) {
      let info_dict = match self.info_dict() {
         Some(info) => info,
         None => {
            warn!("Received piece block before info dict was available");
            return;
         }
      };

      let piece_length = info_dict.piece_length as usize;
      let total_length = info_dict.total_length();
      let piece_count = info_dict.piece_count();
      if index >= piece_count {
         warn!(
            index,
            piece_count, "Received piece block outside the piece range"
         );
         return;
      }
      let last_piece_index = piece_count.saturating_sub(1);

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
         warn!(
            index,
            offset, concrete_piece_len, "Received piece block with offset out of bounds"
         );
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
            warn!(
               index,
               offset,
               block_len = block.len(),
               concrete_piece_len,
               "Received piece block that exceeds piece bounds"
            );
            return;
         }
      } else {
         warn!(
            index,
            offset,
            block_len = block.len(),
            "Offset + block length overflows"
         );
         return;
      }

      let expected_blocks = concrete_piece_len.div_ceil(BLOCK_SIZE);
      let block_index = offset / BLOCK_SIZE;
      if block_index >= expected_blocks {
         warn!(
            index,
            offset, block_index, expected_blocks, "Received piece block with invalid block index"
         );
         return;
      }

      let expected_block_len = (concrete_piece_len - offset).min(BLOCK_SIZE);
      if block.len() != expected_block_len {
         warn!(
            index,
            offset,
            actual = block.len(),
            expected = expected_block_len,
            "Received piece block with unexpected length"
         );
         return;
      }

      if self.is_duplicate_block(index, block_index) {
         trace!("Received duplicate piece block");
         return;
      }

      if !self.write_block_to_storage(index, offset, block).await {
         return;
      }

      // Only mark block complete after successful write
      let assigned_peer =
         self
            .piece_scheduler
            .mark_block_complete(index, block_index, expected_blocks);
      if let Some(assigned_peer) = assigned_peer
         && assigned_peer != peer_id
         && let Some(peer) = self.peers.get(&assigned_peer)
      {
         let _ = peer
            .tell(CancelPiece {
               index,
               begin: offset,
               length: expected_block_len,
            })
            .try_send();
      }

      if self.is_piece_complete(index) {
         self.piece_completed(peer_id, index).await;
      } else {
         self.fill_peer_request_window(peer_id);
         trace!(%peer_id, "Requested replacement block from peer");
      }
   }

   pub(super) fn fill_initial_peer_request_window(&mut self, peer_id: crate::peer::PeerId) {
      self.fill_peer_request_window_to(peer_id, self.settings.torrent.initial_peer_request_window);
   }

   pub(super) fn fill_peer_request_window(&mut self, peer_id: crate::peer::PeerId) {
      self.fill_peer_request_window_to(peer_id, self.settings.torrent.max_in_flight_per_peer);
   }

   pub(super) fn fill_all_peer_request_windows(&mut self) {
      if self.state != TorrentState::Downloading || !self.is_ready() {
         return;
      }

      let peer_ids: Vec<_> = self.peers.keys().copied().collect();
      for peer_id in peer_ids {
         self.fill_peer_request_window(peer_id);
      }
   }

   fn fill_peer_request_window_to(&mut self, peer_id: crate::peer::PeerId, target_size: usize) {
      if self.state != TorrentState::Downloading || !self.is_ready() {
         return;
      }
      let Some(info) = self.info_dict() else {
         return;
      };
      let Some(peer) = self.peers.get(&peer_id).cloned() else {
         return;
      };

      let current_size = self.piece_scheduler.in_flight_for_peer(peer_id);
      let available_slots = target_size.saturating_sub(current_size);
      if available_slots == 0 {
         return;
      }

      let requests = self.piece_scheduler.requests_for_peer(
         peer_id,
         available_slots,
         info.piece_length as usize,
         info.total_length(),
      );
      for request in requests {
         if let Err(err) = peer
            .tell(NeedPiece {
               index: request.piece_index,
               begin: request.offset(),
               length: request.length,
            })
            .try_send()
         {
            self.piece_scheduler.release_peer_request(
               peer_id,
               request.piece_index,
               request.offset(),
            );
            trace!(?err, %peer_id, "Peer mailbox unavailable for block request");
         }
      }
   }

   async fn write_block_to_storage(&self, index: usize, offset: usize, block: Bytes) -> bool {
      match &self.piece_storage {
         PieceStorageStrategy::Disk(_) => {
            // Disk storage is the piece-cache strategy: blocks are assembled in
            // standalone `.piece` files before being flushed to output files.
            let Ok(path) = self.get_piece_path(index) else {
               warn!(piece_index = index, "Failed to get piece path");
               return false;
            };
            match self
               .piece_store
               .ask(WriteBlock {
                  path,
                  offset,
                  block,
               })
               .await
            {
               Ok(()) => true,
               Err(err) => {
                  warn!(
                     ?err,
                     piece_index = index,
                     offset,
                     "Piece store failed to write block"
                  );
                  false
               }
            }
         }
         PieceStorageStrategy::InFile => match &self.piece_manager {
            // In-file storage skips the `.piece` cache and writes the incoming
            // block straight to its final file offset.
            PieceManagerProxy::Default(manager) => {
               match manager.write_block(index, offset, block).await {
                  Ok(()) => true,
                  Err(err) => {
                     warn!(
                        ?err,
                        piece_index = index,
                        offset,
                        "Failed to write in-file block"
                     );
                     false
                  }
               }
            }
            PieceManagerProxy::Custom(_) => {
               warn!(
                  piece_index = index,
                  offset, "In-file storage requires the default piece manager"
               );
               false
            }
         },
      }
   }

   async fn piece_completed(&mut self, peer_id: crate::peer::PeerId, index: usize) {
      self.piece_scheduler.remove_piece_blocks(index);
      let info_dict = self
         .info_dict()
         .expect("Can't receive piece without info dict");
      let piece_count = info_dict.piece_count();

      if !self.validate_and_commit_piece(index).await {
         #[cfg(feature = "live")]
         self.publish_live_view(|view| {
            crate::live::TorrentEventKind::MetricsChanged(view.metrics.clone())
         });
         self.fill_peer_request_window(peer_id);
         return;
      }

      self.piece_scheduler.mark_piece_complete(index);
      self.bitfield.set_aliased(index, true);

      debug!(
         piece_index = index,
         pieces_left = piece_count.saturating_sub(index + 1),
         "Piece is now complete"
      );

      self.broadcast_to_peers_best_effort(Have { piece: index });

      // Piece completion is the meaningful progress boundary. Publishing for
      // every 16 KiB block creates an event storm without improving the view.
      #[cfg(feature = "live")]
      self.publish_live_view(|view| {
         crate::live::TorrentEventKind::MetricsChanged(view.metrics.clone())
      });

      if self.piece_scheduler.next_piece() >= piece_count {
         self.update_tracker_progress().await;
         self.transition_state(TorrentState::Seeding);
         self.announce_tracker_event(Event::Completed).await;
         info!("Torrenting process completed, switching to seeding mode");
         self.rechoke_peers().await;
         self.schedule_next_rechoke().await;
      } else {
         // The block that completed this piece consumed one request-window
         // slot. Refill it just like every other accepted block so the
         // pipeline cannot drain by one slot per completed piece.
         self.fill_peer_request_window(peer_id);
      }
   }

   async fn validate_and_commit_piece(&mut self, index: usize) -> bool {
      let info_dict = self
         .info_dict()
         .expect("Can't receive piece without info dict");

      match &self.piece_storage {
         PieceStorageStrategy::Disk(_) => {
            // Cached pieces are validated as standalone files, then handed to
            // the piece manager to populate the final output files.
            let Ok(path) = self.get_piece_path(index) else {
               warn!(index, "Failed to get piece path; re-requesting");
               return false;
            };

            let data = match self
               .piece_store
               .ask(ValidateAndRead {
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
               return false;
            }
         }
         PieceStorageStrategy::InFile => {
            // Blocks were already written into output files, so validation must
            // hash the piece bytes read back from those files.
            let PieceManagerProxy::Default(manager) = &self.piece_manager else {
               warn!(
                  index,
                  "In-file storage requires the default piece manager; re-requesting"
               );
               return false;
            };

            let data = match manager.read_piece(index).await {
               Ok(data) => data,
               Err(err) => {
                  warn!(?err, index, "Failed to read in-file piece; re-requesting");
                  return false;
               }
            };

            if let Err(err) = util::validate_piece_bytes(&data, info_dict.pieces[index]) {
               warn!(
                  ?err,
                  index, "Failed to validate in-file piece; re-requesting"
               );
               return false;
            }
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
      match &self.piece_storage {
         PieceStorageStrategy::Disk(_) => {
            let mut file = File::open(self.get_piece_path(index)?).await?;
            let mut buffer = vec![0; length];
            file.seek(SeekFrom::Start(offset as u64)).await?;
            file.read_exact(&mut buffer).await?;
            Ok(buffer.into())
         }
         PieceStorageStrategy::InFile => match &self.piece_manager {
            PieceManagerProxy::Default(manager) => {
               manager.read_piece_block(index, offset, length).await
            }
            PieceManagerProxy::Custom(_) => {
               anyhow::bail!("in-file reads require the default piece manager")
            }
         },
      }
   }
}

#[cfg(test)]
mod tests {
   use std::{collections::HashMap, sync::atomic::AtomicU8};

   use bitvec::vec::BitVec;
   use bytes::Bytes;
   use kameo::actor::Spawn;
   use kameo_actors::scheduler::Scheduler;
   use librqbit_utp::UtpSocket;

   use super::*;
   use crate::{
      hashes::HashVec,
      metainfo::{Info, InfoKeys, MetaInfo, TorrentFile},
      pieces::{FilePieceManager, PieceScheduler, PieceStoreActor},
      settings::Settings,
      testing,
      torrent::{
         actor::{PieceManagerProxy, TorrentActorArgs},
         choking::ChokingScheduler,
      },
      tracker::{Tracker, udp::UdpServer},
   };

   fn test_info() -> Info {
      Info {
         name: "data.bin".to_string(),
         piece_length: 4,
         pieces: HashVec::from(vec![testing::piece_hash(b"abcd")]),
         file: InfoKeys::Single {
            length: 4,
            md5sum: None,
         },
         is_private: None,
         publisher: None,
         publisher_url: None,
         source: None,
      }
   }

   fn test_metainfo(info: Info) -> MetaInfo {
      MetaInfo::Torrent(TorrentFile {
         announce: Some(Tracker::Http("http://127.0.0.1/announce".to_string())),
         announce_list: None,
         comment: None,
         created_by: None,
         creation_date: None,
         encoding: None,
         info,
         url_list: None,
      })
   }

   async fn test_actor(
      piece_storage: PieceStorageStrategy, base_path: std::path::PathBuf,
   ) -> TorrentActor {
      let info = test_info();
      let metainfo = test_metainfo(info.clone());
      let peer_id = testing::peer_id();
      let tracker_server = UdpServer::new(None).await.unwrap();
      let utp_server = UtpSocket::new_udp(testing::ephemeral_socket_addr())
         .await
         .unwrap();

      let actor_ref = TorrentActor::spawn(TorrentActorArgs {
         peer_id,
         metainfo: metainfo.clone(),
         utp_server: utp_server.clone(),
         tracker_server: tracker_server.clone(),
         primary_addr: None,
         piece_storage: piece_storage.clone(),
         autostart: Some(false),
         sufficient_peers: Some(usize::MAX),
         base_path: Some(base_path.clone()),
         settings: Settings::default(),
         hub: Hub::default(),
      });

      TorrentActor {
         hub: Hub::default(),
         peers: HashMap::new(),
         trackers: HashMap::new(),
         bitfield: BitVec::<AtomicU8>::repeat(false, info.piece_count()),
         id: peer_id,
         resolved_magnet_info: None,
         metainfo,
         tracker_server,
         scheduler: Scheduler::spawn(Scheduler::new()),
         utp_server,
         actor_ref,
         piece_storage,
         piece_store: PieceStoreActor::spawn(()),
         piece_manager: PieceManagerProxy::Default(FilePieceManager(Some(base_path), Some(info))),
         state: TorrentState::Downloading,
         piece_scheduler: PieceScheduler::new(1),
         choking_scheduler: ChokingScheduler::default(),
         next_rechoke: None,
         start_time: None,
         sufficient_peers: 6,
         autostart: false,
         pending_start: false,
         ready_hook: Vec::new(),
         settings: Settings::default(),
      }
   }

   #[tokio::test]
   async fn torrent_actor_when_using_in_file_storage_then_downloads_and_reads_back_piece() {
      let base_path = testing::torrent_temp_path();
      let mut actor = test_actor(PieceStorageStrategy::InFile, base_path.clone()).await;

      assert!(
         actor
            .write_block_to_storage(0, 0, Bytes::from_static(b"ab"))
            .await
      );
      assert!(
         actor
            .write_block_to_storage(0, 2, Bytes::from_static(b"cd"))
            .await
      );
      assert!(actor.validate_and_commit_piece(0).await);

      actor.bitfield.set_aliased(0, true);
      assert_eq!(
         actor.read_piece_block(0, 1, 2).await.unwrap(),
         Bytes::from_static(b"bc")
      );

      actor.actor_ref.kill();
      actor.piece_store.kill();
      actor.scheduler.kill();
      tokio::fs::remove_dir_all(base_path).await.unwrap();
   }

   #[tokio::test]
   async fn torrent_actor_when_using_disk_storage_then_keeps_piece_cache_and_writes_output_file() {
      let piece_path = testing::torrent_temp_path();
      let base_path = testing::torrent_temp_path();
      tokio::fs::create_dir_all(&piece_path).await.unwrap();
      let mut actor = test_actor(
         PieceStorageStrategy::Disk(piece_path.clone()),
         base_path.clone(),
      )
      .await;

      assert!(
         actor
            .write_block_to_storage(0, 0, Bytes::from_static(b"ab"))
            .await
      );
      assert!(
         actor
            .write_block_to_storage(0, 2, Bytes::from_static(b"cd"))
            .await
      );
      assert!(actor.validate_and_commit_piece(0).await);

      actor.bitfield.set_aliased(0, true);
      assert_eq!(
         actor.read_piece_block(0, 1, 2).await.unwrap(),
         Bytes::from_static(b"bc")
      );
      assert_eq!(
         tokio::fs::read(base_path.join("data.bin")).await.unwrap(),
         b"abcd"
      );

      actor.actor_ref.kill();
      actor.piece_store.kill();
      actor.scheduler.kill();
      tokio::fs::remove_dir_all(piece_path).await.unwrap();
      tokio::fs::remove_dir_all(base_path).await.unwrap();
   }
}
