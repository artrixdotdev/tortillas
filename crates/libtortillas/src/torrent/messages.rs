use core::panic;
use std::{
   fmt,
   path::PathBuf,
   sync::{Arc, atomic::AtomicU8},
};

use bitvec::vec::BitVec;
use bytes::Bytes;
use kameo::{
   Reply,
   prelude::{Context, Message},
};
use sha1::{Digest, Sha1};
use tokio::fs;
use tracing::{debug, error, info, trace, warn};

use super::{BLOCK_SIZE, PieceStorageStrategy, ReadyHook, TorrentActor, TorrentState, util};
use crate::{
   actor_request_response,
   hashes::InfoHash,
   metainfo::Info,
   peer::{Peer, PeerId, PeerTell},
   protocol::stream::PeerStream,
   torrent::{PieceManagerProxy, piece_manager::PieceManager},
   tracker::Tracker,
};

/// For incoming from outside sources (e.g Peers, Trackers and Engine)
#[allow(dead_code)]
pub(crate) enum TorrentMessage {
   /// A message from an announce actor containing new Peers
   Announce(Vec<Peer>),

   /// Sent after an incoming peer initializes a handshake
   /// The handshake will be preverified and routed to this torrent instance.
   ///
   /// We as the instance are expected to reply to said handshake, this is not
   /// the responsibility of the engine.
   IncomingPeer(Peer, Box<PeerStream>),

   /// Used to manually add a peer. This is primarily used for testing but can
   /// be used to initiate a peer connection without it having to come from an
   /// announce.
   AddPeer(Peer),
   /// Index, Offset, Data
   /// See the corresponding [peer message](PeerMessages::Piece)
   IncomingPiece(usize, usize, Bytes),
   /// Bytes for the [Info] dict from an peer, these info bytes are expected to
   /// be verified by the torrent us before being used.
   InfoBytes(Bytes),

   KillPeer(PeerId),
   KillTracker(Tracker),

   PieceStorage(PieceStorageStrategy),

   /// Sets the current piece manager to a custom implementation.
   PieceManager(Box<dyn PieceManager>),

   /// Sets the output path, should only be used when the [`FilePieceManager`]
   /// is used
   SetOutputPath(PathBuf),

   /// Start the torrenting process & actually start downloading pieces/seeding
   SetState(TorrentState),

   SetAutoStart(bool),

   SetSufficientPeers(usize),
   /// A hook that is called when the torrent is ready to start downloading.
   /// This is used to implement [`Torrent::poll_ready`].
   ///
   /// Only should be used internally.
   ReadyHook(ReadyHook),
}

impl fmt::Debug for TorrentMessage {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      match self {
         TorrentMessage::InfoBytes(bytes) => write!(f, "InfoBytes({bytes:?})"),
         TorrentMessage::KillPeer(peer_id) => write!(f, "KillPeer({peer_id:?})"),
         TorrentMessage::KillTracker(tracker) => write!(f, "KillTracker({tracker:?})"),
         TorrentMessage::AddPeer(peer) => write!(f, "AddPeer({peer:?})"),
         TorrentMessage::IncomingPiece(index, offset, data) => {
            write!(f, "IncomingPiece({index}, {offset}, {})", data.len())
         }
         TorrentMessage::Announce(peers) => write!(f, "Announce({peers:?})"),
         _ => write!(f, "TorrentMessage"), // Add more later,
      }
   }
}

actor_request_response!(
   #[allow(dead_code)]
   pub(crate) TorrentRequest,
   pub(crate) TorrentResponse #[derive(Reply)],

   /// Bitfield of the torrent
   Bitfield
   Bitfield(Arc<BitVec<AtomicU8>>),

   /// Current peers of the torrent
   CurrentPeers
   CurrentPeers(Vec<&'static Peer>),

   PeerCount
   PeerCount(usize),
   /// Current trackers of the torrent
   CurrentTrackers
   CurrentTrackers(Vec<&'static Tracker>),
   /// Info hash of the torrent
   InfoHash
   InfoHash(InfoHash),
   /// Sends the current info dict if we have it
   HasInfoDict
   HasInfoDict(Option<Info>),
   /// Requests a piece from the torrent
   Request(usize, usize, usize)
   Request(usize, usize, Bytes),

   GetState
   GetState(TorrentState),
);

impl Message<TorrentMessage> for TorrentActor {
   type Reply = ();

   async fn handle(
      &mut self, message: TorrentMessage, _: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      trace!(message = ?message, "Received message");
      match message {
         TorrentMessage::Announce(peers) => {
            for peer in peers {
               self.append_peer(peer, None);
            }
         }
         TorrentMessage::IncomingPeer(peer, stream) => self.append_peer(peer, Some(*stream)),
         TorrentMessage::AddPeer(peer) => {
            self.append_peer(peer, None);
         }

         TorrentMessage::InfoBytes(bytes) => {
            if self.info.is_some() {
               debug!(
                  dict = %String::from_utf8_lossy(&bytes),
                  "Received info dict when we already have one"
               );
               return;
            }
            let mut hasher = Sha1::new();

            hasher.update(&bytes);
            let hash = hex::encode(hasher.finalize());
            if hash == self.info_hash().to_hex() {
               info!("Received valid info dict, starting torrent process...");
               let info: Info =
                  serde_bencode::from_bytes(&bytes).expect("Failed to parse info dict");
               self.bitfield = Arc::new(BitVec::with_capacity(info.piece_count()));
               self.info = Some(info);
               self
                  .broadcast_to_peers(PeerTell::HaveInfoDict(self.bitfield.clone()))
                  .await;
            } else {
               warn!(
                  dict = %String::from_utf8_lossy(&bytes),
                  "Received invalid info hash"
               );
            }
         }
         TorrentMessage::KillPeer(id) => {
            // Kill the actor quietly
            if let Some(actor) = self.peers.get(&id) {
               actor.kill();
               self.peers.remove(&id);
            } else {
               warn!("Received kill peer message for unknown peer");
            }
         }
         TorrentMessage::KillTracker(tracker) => {
            // Kill the actor quietly
            if let Some(actor) = self.trackers.get(&tracker) {
               actor.kill();
               self.trackers.remove(&tracker);
            } else {
               warn!("Received kill tracker message for unknown tracker");
            }
         }
         TorrentMessage::IncomingPiece(index, offset, block) => {
            let info_dict = self
               .info_dict()
               .expect("Can't receive piece without info dict");

            let block_index = offset / BLOCK_SIZE;
            let piece_count = info_dict.piece_count();

            if let Some(block_map) = &self.block_map.get_mut(&index) {
               self
                  .broadcast_to_peers(PeerTell::CancelPiece(index, offset, block.len()))
                  .await;
               if block_map[block_index] {
                  warn!("Received duplicate piece block");
                  return;
               }
            } else {
               let total_blocks = (info_dict.piece_length as usize).div_ceil(BLOCK_SIZE);
               let mut vec = BitVec::with_capacity(total_blocks);
               vec.resize(total_blocks, false);
               self.block_map.insert(index, vec);
            };

            self
               .block_map
               .get_mut(&index)
               .unwrap() // Unwrap is safe because we just inserted the block map
               .set(block_index, true);

            let block_len = block.len();

            let is_piece_complete = self
               .block_map
               .get(&index)
               .map(|blocks| blocks.iter().all(|b| *b))
               .unwrap_or(false);

            match &self.piece_storage {
               PieceStorageStrategy::Disk(_) => {
                  let path = self
                     .get_piece_path(index)
                     .expect("Failed to get piece path");
                  util::write_block_to_file(path, offset, block)
                     .await
                     .expect("Failed to write block to file")
               }
               PieceStorageStrategy::InFile => {
                  unimplemented!()
               }
            };

            // We now have the full piece
            if is_piece_complete {
               let previous_blocks = self.block_map.remove(&index);
               let cur_piece = self.next_piece;

               match &self.piece_storage {
                  PieceStorageStrategy::Disk(_) => {
                     let path = self
                        .get_piece_path(index)
                        .expect("Failed to get piece path");
                     if util::validate_piece_file(path.clone(), info_dict.pieces[index])
                        .await
                        .is_err()
                     {
                        warn!(path = %path.display(), index, "Piece file is invalid, clearing it");
                        let path_clone = path.clone();

                        // Clears the piece on a new thread
                        tokio::spawn(async move {
                           fs::remove_file(&path_clone).await.unwrap_or_else(|_| {
                              error!("Failed to delete file for piece {}", &path_clone.display());
                           });
                        });
                        return;
                     }

                     let data = fs::read(&path).await.unwrap().into();
                     if let Err(err) = self.piece_manager.recv(index, data).await {
                        warn!(?err, index, path = %path.display(), "Piece manager rejected piece; re-requesting");
                        if let Some((_, mut blocks)) = previous_blocks {
                           blocks.fill(false);
                           self.block_map.insert(index, blocks);
                        }
                        self
                           .broadcast_to_peers(PeerTell::NeedPiece(index, 0, BLOCK_SIZE))
                           .await;
                        return;
                     }
                  }
                  PieceStorageStrategy::InFile => {
                     unimplemented!()
                  }
               }

               self.next_piece += 1;
               self.bitfield.set_aliased(index, true);
               trace!(id = %self.info_hash(), piece = index, "Piece is now complete");

               // Announce to peers that we have this piece
               self.broadcast_to_peers(PeerTell::Have(cur_piece)).await;
               if self.next_piece >= piece_count - 1 {
                  // Handle end of torrenting process
                  self.state = TorrentState::Seeding;
                  info!("Torrenting process completed, switching to seeding mode");
               } else {
                  self
                     .broadcast_to_peers(PeerTell::NeedPiece(self.next_piece, 0, BLOCK_SIZE))
                     .await;
               }
            } else {
               // We need more blocks
               // Requests the next block at the next offset
               let offset = offset + block_len;

               // Check if we're overflowing the piece, this is only when we're requesting the
               // last block. This happens because if a piece is lets say 100 bytes, and we
               // request 40 bytes per block, when we're on piece 2, we'll
               // overflow and request 120 bytes instargo ad of 100. This checks if we're
               // overflowing and if so, we'll request the remaining bytes of
               // the piece
               let is_overflowing = offset + BLOCK_SIZE > info_dict.piece_length as usize;
               let next_block_len = if is_overflowing {
                  info_dict.piece_length as usize - offset
               } else {
                  BLOCK_SIZE
               };

               self
                  .broadcast_to_peers(PeerTell::NeedPiece(index, offset, next_block_len))
                  .await;
               trace!(id = %self.info_hash(), piece = index, "Requested next block");
            };
         }
         TorrentMessage::PieceStorage(strategy) => {
            if !self.is_empty() {
               // Intentional panic because this is unintended behavior
               panic!("Cannot change piece storage strategy after we've already received pieces");
            }
            if let PieceStorageStrategy::Disk(dir) = &strategy {
               util::create_dir(dir).await.unwrap(); // Intended panic
            }
            self.piece_storage = strategy;
         }
         TorrentMessage::PieceManager(manager) => {
            // Intnetional panic, the program should not run if this is not the case
            assert!(
               matches!(self.piece_storage, PieceStorageStrategy::Disk(_)),
               "Storage strategy **must** be set to disk before the piece manager is changed",
            );

            self.piece_manager = PieceManagerProxy::Custom(manager);
         }
         TorrentMessage::SetOutputPath(path) => match &mut self.piece_manager {
            PieceManagerProxy::Default(manager) => manager.set_path(path),
            _ => {
               warn!(path = ?path, "Cannot set output path when using a custom piece manager; ignoring.")
            }
         },
         TorrentMessage::SetState(state) => {
            self.state = state;
            if let TorrentState::Downloading = state {
               self.start().await;
            }
         }
         TorrentMessage::SetAutoStart(auto) => {
            self.autostart = auto;
            if !self.pending_start {
               self.autostart().await;
            }
         }
         TorrentMessage::SetSufficientPeers(peers) => {
            self.sufficient_peers = peers;
            if !self.pending_start {
               self.autostart().await;
            }
         }
         TorrentMessage::ReadyHook(hook) => {
            // If torrent has already transitioned from Inactive state, immediately send
            // ready signal
            if self.state != TorrentState::Inactive {
               let _ = hook.send(());
               return;
            }

            let is_ready = self.is_ready_to_start();
            if is_ready && !self.autostart {
               let _ = hook.send(());
            } else {
               self.ready_hook = Some(hook);
               self.autostart().await;
            }
         }
      }
   }
}

impl Message<TorrentRequest> for TorrentActor {
   type Reply = TorrentResponse;

   // TODO: Figure out a way to send the peers back to the engine (if needed)
   async fn handle(
      &mut self, message: TorrentRequest, _: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match message {
         TorrentRequest::Bitfield => TorrentResponse::Bitfield(self.bitfield.clone()),
         TorrentRequest::PeerCount => TorrentResponse::PeerCount(self.peers.len()),
         TorrentRequest::CurrentPeers => {
            unimplemented!()
            // TorrentResponse::CurrentPeers(self.peers.values().map(|peer|
            // peer).collect());
         }
         TorrentRequest::CurrentTrackers => {
            unimplemented!()
            // TorrentResponse::CurrentTrackers(self.trackers.keys().collect())
         }
         TorrentRequest::InfoHash => TorrentResponse::InfoHash(self.info_hash()),

         TorrentRequest::HasInfoDict => TorrentResponse::HasInfoDict(self.info.clone()),
         TorrentRequest::Request(_, _, _) => {
            unimplemented!()
         }
         TorrentRequest::GetState => TorrentResponse::GetState(self.state),
      }
   }
}
