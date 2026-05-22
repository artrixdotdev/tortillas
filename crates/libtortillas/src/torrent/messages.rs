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
use tracing::{info, instrument, trace, warn};

use super::{
   PieceStorageStrategy, TorrentActor, TorrentExport, TorrentState,
   actor::{PieceManagerProxy, ReadyHook},
   util,
};
use crate::{
   actor_request_response,
   hashes::InfoHash,
   metainfo::Info,
   peer::{Peer, PeerId, PeerTell},
   pieces::PieceManager,
   protocol::stream::PeerStream,
   tracker::Tracker,
};

const MAX_IN_FLIGHT_PER_PEER: usize = 32;

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

    /// Sent by a connection task after peer handshaking completes.
    PeerConnected(Peer, Box<PeerStream>),

    /// Index, Offset, Data
   /// See the corresponding [peer message](PeerMessages::Piece)
   IncomingPiece(PeerId, usize, usize, Bytes),
   /// Release a scheduler entry for a request a peer could not accept.
   PeerRejectedRequest(usize, usize),
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

   /// Sent after the `PeerActor::on_start` is ran
   PeerReady(PeerId),
}

impl fmt::Debug for TorrentMessage {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      match self {
         TorrentMessage::InfoBytes(bytes) => write!(f, "InfoBytes({bytes:?})"),
         TorrentMessage::KillPeer(peer_id) => write!(f, "KillPeer({peer_id:?})"),
         TorrentMessage::KillTracker(tracker) => write!(f, "KillTracker({tracker:?})"),
         TorrentMessage::AddPeer(peer) => write!(f, "AddPeer({peer:?})"),
         TorrentMessage::IncomingPiece(_, index, offset, data) => {
            write!(f, "IncomingPiece({index}, {offset}, {})", data.len())
         }
         TorrentMessage::PeerRejectedRequest(index, offset) => {
            write!(f, "PeerRejectedRequest({index}, {offset})")
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

   PeerCount
   PeerCount(usize),
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

   Export
   Export(Box<TorrentExport>),
);

impl Message<TorrentMessage> for TorrentActor {
   type Reply = ();

   #[instrument(skip(self, message), fields(torrent_id = %self.info_hash()))]
   async fn handle(
      &mut self, message: TorrentMessage, _: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match message {
         TorrentMessage::Announce(peers) => {
            trace!(peer_count = peers.len(), "Received announce message");
            for peer in peers {
               self.append_peer(peer, None);
            }
         }
          TorrentMessage::IncomingPeer(peer, stream) => self.append_peer(peer, Some(*stream)),
          TorrentMessage::AddPeer(peer) => self.append_peer(peer, None),
          TorrentMessage::PeerConnected(peer, stream) => self.insert_peer(peer, *stream),

         TorrentMessage::InfoBytes(bytes) => {
            if self.info.is_some() {
               trace!(
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
               self.bitfield = BitVec::repeat(false, info.piece_count());
               self.info = Some(info);
               self
                  .broadcast_to_peers(PeerTell::HaveInfoDict(Arc::new(self.bitfield.clone())))
                  .await;
            } else {
               warn!(
                  dict = %String::from_utf8_lossy(&bytes),
                  "Received invalid info hash"
               );
            }
         }
         TorrentMessage::KillPeer(id) => {
            self.piece_scheduler.peer_disconnected(id);
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
         // If we're in the downloading state, send a piece request to the peer
         // that just connected
         TorrentMessage::PeerReady(id) => {
            if let Some(actor) = self.peers.get(&id)
               && actor.is_alive()
               && self.state == TorrentState::Downloading
               && self.is_ready()
            {
               self
                  .request_blocks_from_peer(id, MAX_IN_FLIGHT_PER_PEER)
                  .await;
               trace!(peer_id = %id, "Filled peer request window");
            } else {
               trace!(peer_id = %id, state = ?self.state, ready = self.is_ready(), "Ignoring PeerReady: peer unknown, dead, or torrent not in download state");
            }
         }
         TorrentMessage::IncomingPiece(peer_id, index, offset, block) => {
            self.incoming_piece(peer_id, index, offset, block).await
         }
         TorrentMessage::PeerRejectedRequest(index, offset) => {
            self.piece_scheduler.release_request(index, offset);
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
            // If we already have metadata, initialize the replacement manager now
            if let Some(info) = self.info.clone()
               && let Err(err) = self.piece_manager.pre_start(info).await
            {
               warn!(?err, "Failed to pre-start custom piece manager");
            }
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

   async fn handle(
      &mut self, message: TorrentRequest, _: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match message {
         TorrentRequest::Bitfield => TorrentResponse::Bitfield(Arc::new(self.bitfield.clone())),
         TorrentRequest::PeerCount => TorrentResponse::PeerCount(self.peers.len()),
         TorrentRequest::InfoHash => TorrentResponse::InfoHash(self.info_hash()),

         TorrentRequest::HasInfoDict => TorrentResponse::HasInfoDict(self.info.clone()),
         TorrentRequest::Request(index, offset, length) => {
            let data = if self
               .bitfield
               .get(index)
               .as_deref()
               .copied()
               .unwrap_or(false)
            {
               match self.read_piece_block(index, offset, length).await {
                  Ok(data) => data,
                  Err(err) => {
                     warn!(
                        ?err,
                        index, offset, length, "Failed to read requested piece block"
                     );
                     Bytes::new()
                  }
               }
            } else {
               warn!(index, offset, length, "Peer requested piece we do not have");
               Bytes::new()
            };
            TorrentResponse::Request(index, offset, data)
         }
         TorrentRequest::GetState => TorrentResponse::GetState(self.state),
         TorrentRequest::Export => TorrentResponse::Export(Box::new(self.export())),
      }
   }
}
