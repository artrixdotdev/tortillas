use std::{
   fmt,
   sync::{Arc, atomic::AtomicU8},
};

use bitvec::vec::BitVec;
use bytes::Bytes;
use kameo::{
   Reply,
   prelude::{Context, Message},
};
use sha1::{Digest, Sha1};
use tracing::{debug, info, trace, warn};

use super::{PieceStorageStrategy, TorrentActor};
use crate::{
   actor_request_response,
   hashes::InfoHash,
   metainfo::Info,
   peer::{Peer, PeerId, PeerTell},
   protocol::stream::PeerStream,
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
}

impl fmt::Debug for TorrentMessage {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      match self {
         TorrentMessage::InfoBytes(bytes) => write!(f, "InfoBytes({:?})", bytes),
         TorrentMessage::KillPeer(peer_id) => write!(f, "KillPeer({:?})", peer_id),
         TorrentMessage::KillTracker(tracker) => write!(f, "KillTracker({:?})", tracker),
         TorrentMessage::AddPeer(peer) => write!(f, "AddPeer({:?})", peer),
         TorrentMessage::IncomingPiece(index, offset, data) => {
            write!(f, "IncomingPiece({}, {}, {:?})", index, offset, data)
         }
         TorrentMessage::Announce(peers) => write!(f, "Announce({:?})", peers),
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
         TorrentMessage::IncomingPiece(_, _, _) => unimplemented!(),
         TorrentMessage::PieceStorage(strategy) => {
            if !self.is_empty() {
               // Intentional panic because this is unintended behavior
               panic!("Cannot change piece storage strategy after we've already received pieces");
            }
            self.piece_storage = strategy;
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
      }
   }
}
