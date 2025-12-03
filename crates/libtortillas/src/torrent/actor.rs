use std::{
   fmt::{self, Display},
   net::SocketAddr,
   path::PathBuf,
   sync::{Arc, atomic::AtomicU8},
   time::Instant,
};

use anyhow::ensure;
use async_trait::async_trait;
use bitvec::vec::BitVec;
use bytes::Bytes;
use dashmap::DashMap;
use kameo::{Actor, actor::ActorRef, mailbox};
use librqbit_utp::UtpSocketUdp;
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::oneshot};
use tracing::{debug, error, info, instrument, trace, warn};

use super::util;
use crate::{
   errors::TorrentError,
   hashes::InfoHash,
   metainfo::{Info, MetaInfo},
   peer::{Peer, PeerActor, PeerId, PeerTell},
   protocol::{
      messages::{Handshake, PeerMessages},
      stream::{PeerSend, PeerStream},
   },
   torrent::{
      TorrentExport,
      piece_manager::{FilePieceManager, PieceManager},
   },
   tracker::{Event, Tracker, TrackerActor, TrackerMessage, TrackerUpdate, udp::UdpServer},
};
pub const BLOCK_SIZE: usize = 16 * 1024;

/// Defines how torrent pieces are stored and accessed.
///
/// A torrent is composed of multiple pieces, and this enum determines
/// whether those pieces are referenced directly from the downloaded
/// files or written into a separate cache directory.
///
/// # Variants
///
/// - [`Self::InFile`]: References pieces directly from the files that the
///   torrent describes. No extra storage is used; the piece data is read
///   directly from the final output files. This is the default strategy and is
///   efficient when you are downloading directly into the final file layout.
///
/// - [`Self::Disk`]: Stores each piece as a separate file in the specified
///   cache directory. The filename for each piece is its SHA‑1 hash. This
///   strategy is required if you are using a custom output stream, since pieces
///   need to be retrieved later on for future seeding. It is also useful for:
///   - HTTP Streaming or when the file itself is never actually written to disk
///   - Supporting non-standard output backends
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(tag = "strategy", content = "piece_output_path")]
pub enum PieceStorageStrategy {
   /// Reference pieces directly from the downloaded files themselves.
   ///
   /// This avoids extra storage overhead and is the default strategy.
   #[default]
   InFile,
   /// Write each piece to disk separately in the given cache directory.
   ///
   /// Each piece is stored as a file named by its SHA‑1 hash.
   /// This strategy is **required** when using a custom piece receiver.
   ///
   /// The path is not automatically set, and libtortillas will not function
   /// properly without the path being set.
   Disk(PathBuf),
}

/// The current state of the torrent, defaults to
/// [`Inactive`](TorrentState::Inactive)
#[derive(
   Debug,
   Default,
   Clone,
   Copy,
   PartialEq,
   Eq,
   PartialOrd,
   Ord,
   Serialize,
   Deserialize
)]
pub enum TorrentState {
   /// Torrent is downloading new pieces actively
   ///
   /// > Note: Even when in this state, we still seed the pieces that we *do*
   /// > have.
   Downloading,
   /// Torrent is seeding and has already completed the file
   Seeding,
   /// Torrent is paused or currently inactive, no seeding or piece downloading
   /// is happening.
   #[default]
   Inactive,
}

/// A hook that is called when the torrent is ready to start downloading.
/// This is used to implement [`Torrent::poll_ready`].
pub(super) type ReadyHook = oneshot::Sender<()>;

pub(super) enum PieceManagerProxy {
   Custom(Box<dyn PieceManager>),
   Default(FilePieceManager),
}

impl Display for PieceManagerProxy {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      match self {
         Self::Custom(_) => write!(f, "Custom Piece Manager"),
         Self::Default(_) => write!(f, "Default Piece Manager"),
      }
   }
}

#[allow(dead_code)]
impl PieceManagerProxy {
   pub fn is_custom(&self) -> bool {
      matches!(self, Self::Custom(_))
   }
}

#[async_trait]
impl PieceManager for PieceManagerProxy {
   fn info(&self) -> Option<&Info> {
      match self {
         Self::Custom(manager) => manager.info(),
         Self::Default(manager) => manager.info(),
      }
   }
   async fn pre_start(&mut self, info: Info) -> anyhow::Result<()> {
      match self {
         Self::Custom(manager) => manager.pre_start(info).await,
         Self::Default(manager) => manager.pre_start(info).await,
      }
   }
   async fn recv(&self, index: usize, data: Bytes) -> anyhow::Result<()> {
      match self {
         Self::Custom(manager) => manager.recv(index, data).await,
         Self::Default(manager) => manager.recv(index, data).await,
      }
   }
}

pub type BlockMap = DashMap<usize, BitVec<usize>>;

pub(crate) struct TorrentActor {
   pub(crate) peers: Arc<DashMap<PeerId, ActorRef<PeerActor>>>,
   pub(crate) trackers: Arc<DashMap<Tracker, ActorRef<TrackerActor>>>,

   pub(crate) bitfield: Arc<BitVec<AtomicU8>>,
   pub(super) id: PeerId,
   pub(super) info: Option<Info>,
   pub(super) metainfo: MetaInfo,
   #[allow(dead_code)]
   pub(super) tracker_server: UdpServer,
   /// Should only be used to create new connections
   pub(super) utp_server: Arc<UtpSocketUdp>,
   pub(super) actor_ref: ActorRef<Self>,
   pub(super) piece_storage: PieceStorageStrategy,
   pub(super) piece_manager: PieceManagerProxy,
   pub state: TorrentState,
   pub next_piece: usize,
   /// Map of piece indices to block indices. These will be used to track which
   /// blocks we have for each piece. Each entry is deleted when the piece is
   /// completed.
   pub(super) block_map: Arc<BlockMap>,

   pub(super) start_time: Option<Instant>,
   /// The number of peers we need to have before we start downloading, defaults
   /// to 6.
   pub(super) sufficient_peers: usize,

   pub(super) autostart: bool,

   /// If there is already a pending start, we don't want to start a new one
   pub(super) pending_start: bool,

   pub(super) ready_hook: Option<ReadyHook>,
}

impl fmt::Display for TorrentActor {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      let working_trackers = self.trackers.len();
      let working_peers = self.peers.len();
      write!(
         f,
         "Torrent #{} w/ {working_trackers} Trackers & {} Peers",
         self.info_hash(),
         working_peers
      )
   }
}

impl TorrentActor {
   pub fn info_dict(&self) -> Option<&Info> {
      if let Some(info) = &self.info {
         Some(info)
      } else {
         match &self.metainfo {
            MetaInfo::Torrent(t) => Some(&t.info),
            _ => None,
         }
      }
   }

   pub fn info_hash(&self) -> InfoHash {
      if let Some(info) = &self.info_dict() {
         info.hash().expect("Failed to compute info hash")
      } else {
         match &self.metainfo {
            MetaInfo::Torrent(t) => t.info.hash().expect("Failed to compute info hash"),
            MetaInfo::MagnetUri(m) => m
               .info_hash()
               .expect("Magnet URIs should always have info hashes"),
         }
      }
   }
   /// Checks if the torrent is empty (we haven't downloaded any pieces yet) by
   /// checking if our bitfield is filled with zeros.
   ///
   /// The reason why we can't just use `self.bitfield.is_empty()` is because a
   /// bitfield filled with zeros isn't considered "empty" since it still has
   /// data in it
   pub fn is_empty(&self) -> bool {
      self.bitfield.count_zeros() == self.bitfield.len()
   }

   /// Checks if the torrent is ready to autostart (via [`Self::autostart`]) and
   /// torrenting process
   #[instrument(skip(self), fields(torrent_id = %self.info_hash()))]
   pub async fn autostart(&mut self) {
      self.pending_start = true;

      let is_ready = self.is_ready_to_start();
      if is_ready {
         if self.autostart {
            trace!("Autostarting torrent");
            self.start().await;
         } else {
            // Torrent is ready, but auto-start is disabled
            if let Some(err) = self.ready_hook.take().and_then(|hook| hook.send(()).err()) {
               error!(?err, "Failed to send ready hook");
            }
         }
      }

      self.pending_start = false;
   }

   /// Handles an incoming piece block from a peer. This is the main entry point
   /// that orchestrates receiving, validating, and storing piece blocks. If
   /// all blocks for a piece are received, it triggers piece completion
   /// logic.
   pub async fn incoming_piece(&mut self, index: usize, offset: usize, block: Bytes) {
      let info_dict = match &self.info {
         Some(info) => info,
         None => {
            warn!("Received piece block before info dict was available");
            return;
         }
      };

      let piece_length = info_dict.piece_length as usize;
      let expected_blocks = piece_length.div_ceil(BLOCK_SIZE);

      let block_index = offset / BLOCK_SIZE;
      if block_index >= expected_blocks {
         warn!("Received piece block with invalid offset");
         return;
      }

      if self.is_duplicate_block(index, block_index) {
         trace!("Received duplicate piece block");
         return;
      }

      self.initialize_and_mark_block(index, block_index);

      self
         .broadcast_to_peers(PeerTell::CancelPiece(index, offset, block.len()))
         .await;

      self.write_block_to_storage(index, offset, block).await;

      if self.is_piece_complete(index) {
         self.piece_completed(index).await;
      } else {
         let (piece_idx, block_offset, block_length) = self.next_block_coordinates(index);
         self
            .broadcast_to_peers(PeerTell::NeedPiece(piece_idx, block_offset, block_length))
            .await;
         trace!(piece = piece_idx, "Requested next block");
      }
   }

   /// Checks if a block has already been received and initializes the block map
   /// for a piece if it doesn't exist yet. Also marks the current block as
   /// received in the block map.
   fn initialize_and_mark_block(&mut self, index: usize, block_index: usize) {
      if !self.block_map.contains_key(&index) {
         let info_dict = self
            .info_dict()
            .expect("Can't receive piece without info dict");

         let piece_length = info_dict.piece_length as usize;
         let total_blocks = piece_length.div_ceil(BLOCK_SIZE);
         let mut vec = BitVec::with_capacity(total_blocks);
         vec.resize(total_blocks, false);
         self.block_map.insert(index, vec);
      }

      self
         .block_map
         .get_mut(&index)
         .unwrap()
         .set(block_index, true);
   }

   /// Writes a block to the appropriate storage location based on the
   /// configured storage strategy. Currently supports disk-based storage
   /// with file-based storage unimplemented.
   async fn write_block_to_storage(&self, index: usize, offset: usize, block: Bytes) {
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
      }
   }

   /// Handles the completion of a full piece. This validates the piece hash,
   /// sends it to the piece manager, updates the bitfield, notifies peers,
   /// updates trackers, and either requests the next piece or transitions to
   /// seeding mode if done.
   async fn piece_completed(&mut self, index: usize) {
      let info_dict = self
         .info_dict()
         .expect("Can't receive piece without info dict");

      let previous_blocks = self.block_map.remove(&index);
      let cur_piece = self.next_piece;
      let piece_count = info_dict.piece_count();
      let total_length = info_dict.total_length();

      if !self.validate_and_send_piece(index, previous_blocks).await {
         return;
      }

      self.next_piece += 1;
      self.bitfield.set_aliased(index, true);

      debug!(
         piece_index = index,
         pieces_left = piece_count.saturating_sub(index + 1),
         "Piece is now complete"
      );

      self.broadcast_to_peers(PeerTell::Have(cur_piece)).await;

      if let Some(total_downloaded) = self.total_bytes_downloaded() {
         let total_bytes_left = total_length - total_downloaded;
         self
            .update_trackers(TrackerUpdate::Left(total_bytes_left))
            .await;
      }

      if self.next_piece >= piece_count {
         self.state = TorrentState::Seeding;
         self
            .update_trackers(TrackerUpdate::Event(Event::Completed))
            .await;
         self.broadcast_to_trackers(TrackerMessage::Announce).await;
         info!("Torrenting process completed, switching to seeding mode");
      } else {
         let (piece_idx, block_offset, block_length) = self.next_block_coordinates(self.next_piece);
         self
            .broadcast_to_peers(PeerTell::NeedPiece(piece_idx, block_offset, block_length))
            .await;
      }
   }

   /// Validates a completed piece by checking its hash and sends it to the
   /// piece manager. Returns false if validation fails or the piece manager
   /// rejects it, which triggers a re-request of the piece. Returns true if
   /// the piece is successfully validated and stored.
   async fn validate_and_send_piece(
      &mut self, index: usize, previous_blocks: Option<(usize, BitVec)>,
   ) -> bool {
      let info_dict = self
         .info_dict()
         .expect("Can't receive piece without info dict");

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

               tokio::spawn(async move {
                  fs::remove_file(&path_clone).await.unwrap_or_else(|_| {
                     error!(path = ?path_clone.display(), "Failed to delete file piece");
                  });
               });
               return false;
            }

            let data = fs::read(&path).await.unwrap().into();
            if let Err(err) = self.piece_manager.recv(index, data).await {
               warn!(?err, index, path = %path.display(), "Piece manager rejected piece; re-requesting");
               if let Some((_, mut blocks)) = previous_blocks {
                  blocks.fill(false);
                  self.block_map.insert(index, blocks);
               }
               let (piece_idx, block_offset, block_length) = self.next_block_coordinates(index);
               self
                  .broadcast_to_peers(PeerTell::NeedPiece(piece_idx, block_offset, block_length))
                  .await;
               return false;
            }
         }
         PieceStorageStrategy::InFile => {
            unimplemented!()
         }
      }
      true
   }

   /// Calculates the coordinates of the next block to request for a given
   /// piece. Returns a tuple of (piece_index, offset, block_length) where
   /// the offset points to the next unreceived block and the length accounts
   /// for the final block potentially being smaller than the standard block
   /// size.
   pub fn next_block_coordinates(&self, piece_index: usize) -> (usize, usize, usize) {
      let info_dict = self
         .info_dict()
         .expect("Can't receive piece without info dict");

      let piece_length = info_dict.piece_length as usize;

      let next_block_index = self
         .block_map
         .get(&piece_index)
         .and_then(|blocks| blocks.iter().position(|b| !*b))
         .unwrap_or(0);

      let offset = next_block_index * BLOCK_SIZE;
      let is_overflowing = offset + BLOCK_SIZE > piece_length;
      let block_length = if is_overflowing {
         piece_length - offset
      } else {
         BLOCK_SIZE
      };

      (piece_index, offset, block_length)
   }

   fn is_duplicate_block(&self, index: usize, block_index: usize) -> bool {
      self
         .block_map
         .get(&index)
         .map(|block_map| block_map[block_index])
         .unwrap_or(false)
   }

   fn is_piece_complete(&self, index: usize) -> bool {
      self
         .block_map
         .get(&index)
         .map(|blocks| blocks.iter().all(|b| *b))
         .unwrap_or(false)
   }
   pub async fn start(&mut self) {
      if self.is_full() {
         self.state = TorrentState::Seeding;
         info!(id = %self.info_hash(), "Torrent is now seeding");
         self
            .update_trackers(TrackerUpdate::Event(Event::Completed))
            .await;
      } else {
         self.state = TorrentState::Downloading;
         info!(id = %self.info_hash(), "Torrent is now downloading");

         trace!(id = %self.info_hash(), peer_count = self.peers.len(), "Requesting first piece from peers");

         self.next_piece = self.bitfield.first_zero().unwrap_or_default();
         // Announce that we have started
         self
            .update_trackers(TrackerUpdate::Event(Event::Started))
            .await;

         // Force announce
         self.broadcast_to_trackers(TrackerMessage::Announce).await;

         // Now apperently we're supposed to set our event back to "empty" for the next
         // announce (done via the interval), no clue why, just the way it's
         // specified in the spec.
         self
            .update_trackers(TrackerUpdate::Event(Event::Empty))
            .await;

         // Request first piece from peers
         self
            .broadcast_to_peers(PeerTell::NeedPiece(self.next_piece, 0, BLOCK_SIZE))
            .await;
         self.start_time = Some(Instant::now());
      }
      // Send ready hook
      if let Some(err) = self.ready_hook.take().and_then(|hook| hook.send(()).err()) {
         error!(?err, "Failed to send ready hook");
      }

      let Some(info) = self.info.as_ref() else {
         warn!(id = %self.info_hash(), "Start requested before info dict is available; deferring");
         return;
      };

      // Start piece manager
      self
         .piece_manager
         // Probably not the best to clone here, but should be fine for now
         .pre_start(info.clone())
         .await
         .expect("Failed to pre-start piece manager");

      info!(
         torrent_id = %self.info_hash(),
         piece_manager = %self.piece_manager,
         storage_strategy = ?self.piece_storage,
         peer_count = self.peers.len(),
         tracker_count = self.trackers.len(),
         total_pieces = info.piece_count(),
         peer_id = %self.id,
         state = ?self.state,
         "Started torrenting process"
      );
   }

   /// Calculates the total number of bytes downloaded by the torrent. Returns
   /// None if the info dict is not present.
   pub fn total_bytes_downloaded(&self) -> Option<usize> {
      let info = self.info_dict()?;
      let total_length = info.total_length();
      let piece_length = info.piece_length as usize;

      let num_pieces = self.bitfield.len();
      let mut total_bytes = 0usize;

      // Calculate the size of the last piece
      let last_piece_len = if total_length % piece_length == 0 {
         piece_length
      } else {
         total_length % piece_length
      };

      // Sum bytes from completed pieces
      for piece_idx in 0..num_pieces {
         if self.bitfield[piece_idx] {
            let piece_size = if piece_idx == num_pieces - 1 {
               last_piece_len
            } else {
               piece_length
            };
            total_bytes = total_bytes.saturating_add(piece_size);
         }
      }

      // Sum bytes from incomplete pieces via block_map
      for (piece_idx, block) in self.block_map.iter().enumerate() {
         if piece_idx < num_pieces && !self.bitfield[piece_idx] {
            let piece_size = if piece_idx == num_pieces - 1 {
               last_piece_len
            } else {
               piece_length
            };

            let mut piece_offset = 0usize;
            for block_idx in 0..block.len() {
               if block[block_idx] {
                  let block_size = (piece_size - piece_offset).min(BLOCK_SIZE);
                  total_bytes = total_bytes.saturating_add(block_size);
                  piece_offset = piece_offset.saturating_add(block_size);
               } else {
                  piece_offset =
                     piece_offset.saturating_add(BLOCK_SIZE.min(piece_size - piece_offset));
               }
            }
         }
      }

      Some(total_bytes)
   }
   pub fn export(&self) -> TorrentExport {
      TorrentExport {
         info_hash: self.info_hash(),
         state: self.state,
         auto_start: self.autostart,
         sufficient_peers: self.sufficient_peers,
         output_path: match &self.piece_manager {
            PieceManagerProxy::Default(manager) => manager.path().cloned(),
            _ => None,
         },
         metainfo: self.metainfo.clone(),
         piece_storage: self.piece_storage.clone(),
         info_dict: self.info_dict().cloned(),
         bitfield: (*self.bitfield).clone(),
         block_map: (*self.block_map).clone(),
      }
   }

   /// Checks if the torrent has all of the pieces (we've downloaded/have
   /// started with the entire file) by checking if our bitfield is filled with
   /// zeroes.
   pub fn is_full(&self) -> bool {
      self.bitfield.count_ones() == self.bitfield.len()
   }

   pub fn is_ready(&self) -> bool {
      self.info.is_some() && self.peers.len() >= self.sufficient_peers
   }

   pub fn is_ready_to_start(&self) -> bool {
      self.is_ready() && self.state == TorrentState::Inactive
   }

   /// Spawns a new [`PeerActor`] for the given [`Peer`] and adds it to the
   /// torrent's peer set.
   ///
   /// - If a [`PeerStream`] is provided, a handshake is sent immediately.
   /// - If no stream is provided, this function attempts to connect to the peer
   ///   and performs the handshake sequence inline.
   ///
   /// The peer is ignored if:
   /// - The handshake fails,
   /// - The peer ID matches our own, or
   /// - The peer already exists in the peer set.
   #[instrument(skip(self, peer, stream), fields(%self, peer_addr = ?peer.socket_addr(), torrent_id = %self.info_hash()))]
   pub(super) fn append_peer(&self, mut peer: Peer, stream: Option<PeerStream>) {
      let info_hash = Arc::new(self.info_hash());
      let actor_ref = self.actor_ref.clone();
      let our_id = self.id;
      let utp_server = self.utp_server.clone();
      let peers = self.peers.clone();

      tokio::spawn(async move {
         // Should pass the stream to PeerActor at some point
         let mut id = peer.id;
         let stream = match stream {
            Some(mut stream) => {
               let handshake = Handshake::new(info_hash.clone(), our_id);
               if let Err(err) = stream.send(PeerMessages::Handshake(handshake)).await {
                  debug!(error = %err, peer_addr = %peer.socket_addr(), "Failed to send handshake to peer");
                  return;
               }
               stream
            }
            None => {
               let stream = PeerStream::connect(peer.socket_addr(), Some(utp_server)).await;
               match stream {
                  Ok(mut stream) => {
                     match stream.send_handshake(our_id, Arc::clone(&info_hash)).await {
                        Ok(_) => match stream.recv_handshake().await {
                           Ok((peer_id, reserved)) => {
                              id = Some(peer_id);
                              peer.reserved = reserved;
                              peer.determine_supported().await;
                              stream
                           }
                           Err(err) => {
                              trace!(
                                 error = %err,
                                 peer_addr = %peer.socket_addr(),
                                 "Failed to receive handshake from peer; exiting"
                              );
                              return;
                           }
                        },
                        Err(err) => {
                           trace!(
                              error = %err,
                              peer_addr = %peer.socket_addr(),
                              "Failed to send handshake to peer; exiting"
                           );
                           return;
                        }
                     }
                  }
                  Err(err) => {
                     trace!(error = %err, "Failed to connect to peer; exiting");
                     return;
                  }
               }
            }
         };
         // Safe because we always know the id is defined by the lines above
         let id = id.unwrap();

         // Dont add ourselves as peers
         if id == our_id {
            return;
         }

         peer.id = Some(id);

         // Prevents a TOCTOU bug. Checks if peer id is in dashmap. The closure
         // atomically prevents the `PeerActor` from being created unless there
         // is no entry for the peer id (?)
         peers.entry(id).or_insert_with(|| {
            PeerActor::spawn_with_mailbox(
               (peer.clone(), stream, actor_ref, *info_hash),
               mailbox::bounded(120),
            )
         });
      });
   }

   /// Broadcasts a message to all peers concurrently.
   ///
   /// This function snapshots the current set of peer actor references before
   /// sending, which avoids holding the [`DashMap`] lock across `.await`
   /// points. This means other tasks can continue to access and modify the
   /// peer set while the broadcast is in progress.
   ///
   /// Each peer receives the message in parallel using a
   /// [`tokio::task::JoinSet`]. This prevents a slow or unresponsive peer
   /// from blocking delivery to others. However, this also means that
   /// broadcasting may use more memory, since all messages are cloned and
   /// dispatched at once.
   ///
   /// Any errors from individual peers are logged, but do not stop the
   /// broadcast from continuing to other peers.
   #[instrument(skip(self, tell), fields(torrent_id = %self.info_hash(), msg = ?tell))]
   pub(super) async fn broadcast_to_peers(&self, tell: PeerTell) {
      // Snapshot actor refs to release DashMap locks before awaiting.
      let peers = self.peers.clone(); // assuming Arc<DashMap<..>>

      let actor_refs: Vec<(PeerId, ActorRef<PeerActor>)> = peers
         .iter()
         .map(|entry| (*entry.key(), entry.value().clone()))
         .collect();

      for (id, actor) in actor_refs {
         let msg = tell.clone();
         let peers = peers.clone();

         tokio::spawn(async move {
            if actor.is_alive() {
               if let Err(e) = actor.tell(msg).await {
                  warn!(error = %e, peer_id = %id, "Failed to send to peer");
               }
            } else {
               trace!(peer_id = %id, "Peer actor is dead, removing from peers set");
               peers.remove(&id);
            }
         });
      }
      // Returns immediately, without waiting for any peer responses
   }

   /// Broadcasts a [`TrackerUpdate`] to all trackers concurrently. similar to
   /// [`Self::broadcast_to_peers`], but for trackers.
   #[instrument(skip(self, message), fields(torrent_id = %self.info_hash()))]
   pub(super) async fn update_trackers(&self, message: TrackerUpdate) {
      let trackers = self.trackers.clone();

      let actor_refs: Vec<(Tracker, ActorRef<TrackerActor>)> = trackers
         .iter()
         .map(|entry| (entry.key().clone(), entry.value().clone()))
         .collect();

      for (uri, actor) in actor_refs {
         let msg = message.clone();
         let trackers = trackers.clone();

         tokio::spawn(async move {
            if actor.is_alive() {
               if let Err(e) = actor.tell(msg).await {
                  warn!(error = %e, tracker_uri = ?uri, "Failed to send to tracker");
               }
            } else {
               trace!(tracker_uri = ?uri, "Tracker actor is dead, removing from trackers set");
               trackers.remove(&uri);
            }
         });
      }
   }

   pub(super) async fn broadcast_to_trackers(&self, message: TrackerMessage) {
      let trackers = self.trackers.clone();

      let actor_refs: Vec<(Tracker, ActorRef<TrackerActor>)> = trackers
         .iter()
         .map(|entry| (entry.key().clone(), entry.value().clone()))
         .collect();

      for (uri, actor) in actor_refs {
         let trackers = trackers.clone();

         tokio::spawn(async move {
            if actor.is_alive() {
               if let Err(e) = actor.tell(message).await {
                  warn!(error = %e, tracker_uri = ?uri, "Failed to send to tracker");
               }
            } else {
               trace!(tracker_uri = ?uri, "Tracker actor is dead, removing from trackers set");
               trackers.remove(&uri);
            }
         });
      }
   }

   /// Gets the path to a piece file based on the index. Only should be used
   /// when the piece storage strategy is [`Disk`](PieceStorageStrategy::Disk),
   /// this function will panic otherwise.
   pub(super) fn get_piece_path(&self, index: usize) -> anyhow::Result<PathBuf> {
      let info_dict = self.info_dict().ok_or(TorrentError::MissingInfoDict)?;
      ensure!(info_dict.pieces.len() > index, "Index out of bounds");

      let hash = info_dict.pieces[index];

      // Panic because this is a user error, this function should never be called if
      // the storage strategy is not Disk
      assert!(
         matches!(self.piece_storage, PieceStorageStrategy::Disk(_)),
         "Piece storage strategy is not Disk"
      );

      if let PieceStorageStrategy::Disk(path) = &self.piece_storage {
         let mut path = path.clone();
         path.push(format!("{hash}.piece"));
         Ok(path.to_path_buf())
      } else {
         unreachable!()
      }
   }
}

/// Configuration arguments for creating a [`TorrentActor`].
///
/// This struct provides a well-documented way to configure the torrent actor
/// instead of using an unlabeled tuple. Some fields are required; optional
/// fields have sensible defaults.
#[derive(Debug, Clone)]
pub struct TorrentActorArgs {
   /// Peer ID for this torrent instance.
   ///
   /// This should typically match the engine's peer ID.
   pub peer_id: PeerId,

   /// Meta information for the torrent (either from a .torrent file or magnet
   /// URI).
   ///
   /// This contains all the necessary information to start downloading/seeding.
   pub metainfo: MetaInfo,

   /// uTP server for peer connections.
   ///
   /// This is used to establish uTP connections with peers.
   pub utp_server: Arc<UtpSocketUdp>,

   /// UDP server for tracker communication.
   ///
   /// This is used to communicate with UDP trackers.
   pub tracker_server: UdpServer,

   /// Primary address for this torrent instance.
   ///
   /// If not provided, defaults to the uTP server's bind address.
   pub primary_addr: Option<SocketAddr>,

   /// Strategy for storing torrent pieces.
   ///
   /// This determines how pieces are stored and accessed for this torrent.
   pub piece_storage: PieceStorageStrategy,

   /// Whether to automatically start this torrent when it becomes ready.
   ///
   /// If not provided, defaults to `true`.
   pub autostart: Option<bool>,

   /// Minimum number of peers required before starting download.
   ///
   /// If not provided, defaults to 6.
   pub sufficient_peers: Option<usize>,

   /// Base path for torrent downloads.
   ///
   /// If not provided, torrents will use their own default paths.
   pub base_path: Option<PathBuf>,
}

impl Actor for TorrentActor {
   type Args = TorrentActorArgs;

   type Error = TorrentError;

   async fn on_start(args: Self::Args, us: ActorRef<Self>) -> Result<Self, Self::Error> {
      let TorrentActorArgs {
         peer_id,
         metainfo,
         utp_server,
         tracker_server,
         primary_addr,
         piece_storage,
         autostart,
         sufficient_peers,
         base_path,
      } = args;

      let primary_addr = primary_addr.unwrap_or_else(|| {
         let addr = utp_server.bind_addr();
         debug!(torrent_id = %metainfo.info_hash().unwrap(), %addr, "No primary address provided, using default");
         addr
      });
      if let PieceStorageStrategy::Disk(dir) = &piece_storage {
         util::create_dir(dir).await?;
      }

      info!(
         torrent_id = %metainfo.info_hash().unwrap(),
         "Starting new torrent instance",
      );

      // Create tracker actors
      let tracker_list = metainfo.announce_list();
      let trackers = DashMap::new();
      for tracker in tracker_list {
         let actor = TrackerActor::spawn((
            tracker.clone(),
            peer_id,
            tracker_server.clone(),
            primary_addr,
            us.clone(),
         ));
         trackers.insert(tracker, actor);
      }
      let info = match &metainfo {
         MetaInfo::Torrent(t) => Some(t.info.clone()),
         _ => None,
      };
      if info.is_none() {
         debug!(torrent_id = %metainfo.info_hash().unwrap(), "No info dict found in metainfo, you're probably using a magnet uri");
      }
      let bitfield: Arc<BitVec<AtomicU8>> = if let Some(info) = &info {
         debug!(torrent_id = %metainfo.info_hash().unwrap(), "Using bitfield length {}", info.piece_count());
         Arc::new(BitVec::repeat(false, info.piece_count()))
      } else {
         Arc::new(BitVec::EMPTY)
      };
      let default_manager = FilePieceManager(base_path, info.clone());

      Ok(Self {
         peers: Arc::new(DashMap::new()),
         bitfield,
         tracker_server,
         utp_server,
         trackers: Arc::new(trackers),
         id: peer_id,
         metainfo,
         info,
         actor_ref: us,
         piece_storage,
         state: TorrentState::default(),
         next_piece: 0,
         block_map: Arc::new(DashMap::new()),
         start_time: None,
         sufficient_peers: sufficient_peers.unwrap_or(6),
         autostart: autostart.unwrap_or(true),
         pending_start: false,
         ready_hook: None,
         piece_manager: PieceManagerProxy::Default(default_manager),
      })
   }

   async fn next(
      &mut self, _: kameo::prelude::WeakActorRef<Self>,
      mailbox_rx: &mut kameo::prelude::MailboxReceiver<Self>,
   ) -> Option<mailbox::Signal<Self>> {
      if !self.pending_start {
         self.autostart().await;
      }
      mailbox_rx.recv().await
   }
}

#[cfg(test)]
mod tests {
   use std::{net::SocketAddr, str::FromStr, time::Duration};

   use librqbit_utp::UtpSocket;
   use tokio::{fs, time::sleep};
   use tracing::trace;

   use super::*;
   use crate::{
      metainfo::{MagnetUri, TorrentFile},
      torrent::{Torrent, TorrentRequest, TorrentResponse},
   };

   #[tokio::test(flavor = "multi_thread")]
   async fn test_torrent_actor() {
      tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .init();
      let metainfo = TorrentFile::parse(include_bytes!(
         "../../tests/torrents/big-buck-bunny.torrent"
      ))
      .unwrap();

      let peer_id = PeerId::default();

      let udp_server = UdpServer::new(None).await;
      let utp_server =
         UtpSocket::new_udp(SocketAddr::from_str("0.0.0.0:0").expect("Failed to parse"))
            .await
            .unwrap();
      let sufficient_peers = 6;

      let info_hash = metainfo.clone().info_hash().unwrap();

      let actor = TorrentActor::spawn(TorrentActorArgs {
         peer_id,
         metainfo,
         utp_server,
         tracker_server: udp_server.clone(),
         primary_addr: None,
         piece_storage: PieceStorageStrategy::default(),
         autostart: Some(false), /* We don't need to autostart because we're only checking if we
                                  * have peers */
         sufficient_peers: Some(sufficient_peers),
         base_path: None,
      });

      let torrent = Torrent::new(info_hash, actor.clone());

      assert!(torrent.poll_ready().await.is_ok());

      actor.stop_gracefully().await.expect("Failed to stop");
   }

   #[tokio::test(flavor = "multi_thread")]
   async fn test_info_dict_retrieval() {
      tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .init();

      // Test with a magnet URI, since magnet URIs don't come with an info dict
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MagnetUri::parse(contents).unwrap();

      let peer_id = PeerId::default();

      let udp_server = UdpServer::new(None).await;
      let utp_server =
         UtpSocket::new_udp(SocketAddr::from_str("0.0.0.0:0").expect("Failed to parse"))
            .await
            .unwrap();

      let actor = TorrentActor::spawn(TorrentActorArgs {
         peer_id,
         metainfo,
         utp_server,
         tracker_server: udp_server.clone(),
         primary_addr: None,
         piece_storage: PieceStorageStrategy::default(),
         autostart: Some(false),
         sufficient_peers: None,
         base_path: None,
      });

      // Blocking loop that runs until we get an info dict
      loop {
         match actor.ask(TorrentRequest::HasInfoDict).await.unwrap() {
            TorrentResponse::HasInfoDict(maybe_info_dict) => {
               if maybe_info_dict.is_some() {
                  trace!("Got info dict!");
                  break;
               }
            }
            _ => unreachable!(),
         };
         sleep(Duration::from_millis(100)).await;
      }

      actor.stop_gracefully().await.expect("Failed to stop");
   }

   #[tokio::test(flavor = "multi_thread")]
   async fn test_torrent_actor_piece_storage() {
      tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .init();
      let metainfo = TorrentFile::parse(include_bytes!(
         "../../tests/torrents/big-buck-bunny.torrent"
      ))
      .unwrap();
      let info_dict = match &metainfo {
         MetaInfo::Torrent(file) => file.info.clone(),
         _ => unreachable!(),
      };
      let info_hash = info_dict.hash().unwrap();

      // Clears piece files
      async fn clear_piece_files(piece_path: &PathBuf) {
         let mut entries = fs::read_dir(&piece_path).await.unwrap();
         while let Some(entry) = entries.next_entry().await.unwrap() {
            let path = entry.path();
            if let Some(ext) = path.extension()
               && ext == "piece"
            {
               fs::remove_file(&path).await.unwrap();
            }
         }
      }

      let piece_path = std::env::temp_dir().join("tortillas");
      let file_path = piece_path.join("files");

      let peer_id = PeerId::default();

      let udp_server = UdpServer::new(None).await;
      let utp_server =
         UtpSocket::new_udp(SocketAddr::from_str("0.0.0.0:0").expect("Failed to parse"))
            .await
            .unwrap();

      let actor = TorrentActor::spawn(TorrentActorArgs {
         peer_id,
         metainfo,
         utp_server,
         tracker_server: udp_server.clone(),
         primary_addr: None,
         piece_storage: PieceStorageStrategy::Disk(piece_path.clone()),
         autostart: None,
         sufficient_peers: None,
         base_path: Some(file_path),
      });

      let torrent = Torrent::new(info_hash, actor.clone());

      assert!(torrent.poll_ready().await.is_ok());

      loop {
         let mut entries = fs::read_dir(&piece_path).await.unwrap();
         let mut found_piece = false;
         while let Some(entry) = entries.next_entry().await.unwrap() {
            let path = entry.path();
            if let Some(ext) = path.extension()
               && ext == "piece"
            {
               let metadata = entry.metadata().await.unwrap();
               if metadata.len() == info_dict.piece_length {
                  found_piece = true;
                  break;
               }
            }
         }
         if found_piece {
            break;
         }

         sleep(Duration::from_millis(200)).await;
      }

      actor.stop_gracefully().await.unwrap();
      clear_piece_files(&piece_path).await;
   }

   #[tokio::test(flavor = "multi_thread")]
   async fn test_torrent_export() {
      tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .init();
      let metainfo = TorrentFile::parse(include_bytes!(
         "../../tests/torrents/big-buck-bunny.torrent"
      ))
      .unwrap();
      let info_dict = match &metainfo {
         MetaInfo::Torrent(file) => file.info.clone(),
         _ => unreachable!(),
      };

      let info_hash = info_dict.hash().unwrap();

      let piece_path = std::env::temp_dir().join("tortillas");
      let file_path = piece_path.join("files");

      let peer_id = PeerId::default();

      let udp_server = UdpServer::new(None).await;
      let utp_server =
         UtpSocket::new_udp(SocketAddr::from_str("0.0.0.0:0").expect("Failed to parse"))
            .await
            .unwrap();

      let actor = TorrentActor::spawn(TorrentActorArgs {
         peer_id,
         metainfo,
         utp_server,
         tracker_server: udp_server.clone(),
         primary_addr: None,
         piece_storage: PieceStorageStrategy::Disk(piece_path.clone()),
         autostart: None,
         sufficient_peers: None,
         base_path: Some(file_path),
      });

      let torrent = Torrent::new(info_hash, actor.clone());

      assert!(torrent.poll_ready().await.is_ok());

      loop {
         let bitfield = match actor.ask(TorrentRequest::Bitfield).await.unwrap() {
            TorrentResponse::Bitfield(bitfield) => bitfield,
            _ => unreachable!(),
         };
         // Wait until we have at least 2 pieces
         if bitfield.count_ones() > 2 {
            break;
         }
         sleep(Duration::from_millis(100)).await;
      }

      let export = torrent.export().await;

      assert_eq!(export.info_hash, info_hash);
      assert!(
         export.info_dict.is_some(),
         "Torrent shouldn't have started without info dict"
      );

      // Test serialization
      use serde_json::{from_str, to_string};

      let export_str = to_string(&export).unwrap();

      let from_export: TorrentExport = from_str(&export_str).unwrap();
      assert_eq!(export.info_hash, from_export.info_hash);
      assert_eq!(export.state, from_export.state);
      assert_eq!(export.auto_start, from_export.auto_start);
      assert_eq!(export.sufficient_peers, from_export.sufficient_peers);
      assert_eq!(export.output_path, from_export.output_path);
      assert_eq!(export.bitfield, from_export.bitfield);

      assert!(
         export.info_dict.is_some(),
         "Torrent shouldn't have started without info dict"
      );

      trace!("Export: {export_str}");

      actor.stop_gracefully().await.unwrap();
   }
}
