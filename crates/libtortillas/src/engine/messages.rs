use kameo::{
   Actor, Reply, mailbox,
   prelude::{ActorRef, Context, Message},
};
use tracing::{error, warn};

use super::EngineActor;
use crate::{
   actor_request_response,
   errors::EngineError,
   metainfo::MetaInfo,
   peer::Peer,
   protocol::{
      messages::PeerMessages,
      stream::{PeerRecv, PeerStream},
   },
   torrent::{TorrentActor, TorrentActorArgs, TorrentMessage, TorrentState},
};

pub(crate) enum EngineMessage {
   /// Handles an incoming peer connection. The peer has been neither handshaked
   /// nor verified at this point.
   IncomingPeer(Box<PeerStream>),
   /// Starts all torrents managed by the engine.
   StartAll,
}

actor_request_response!(
   #[allow(dead_code)]
   pub(crate) EngineRequest,
   pub(crate) EngineResponse #[derive(Reply)],
   /// Creates a new [Torrent] actor.
   Torrent(Box<MetaInfo>) Torrent(ActorRef<TorrentActor>),

);

impl Message<EngineMessage> for EngineActor {
   type Reply = ();
   async fn handle(
      &mut self, msg: EngineMessage, _: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match msg {
         EngineMessage::IncomingPeer(mut stream) => {
            let msg = stream.recv().await.expect("Can't read from peer stream");
            let peer_addr = stream.remote_addr().expect("Can't get remote address");

            if let PeerMessages::Handshake(handshake) = msg {
               let info_hash = *handshake.info_hash;
               let peer = Peer::from_socket_addr(peer_addr);

               if let Some(torrent) = self.torrents.get(&info_hash) {
                  torrent
                     .tell(TorrentMessage::IncomingPeer(peer, stream))
                     .await
                     .expect("Failed to tell torrent about incoming peer");
               } else {
                  error!(%stream, "Received incoming peer for unknown torrent, killing connection");
                  drop(stream);
               }
            } else {
               error!("Received unexpected message from peer");
            }
         }
         EngineMessage::StartAll => {
            for torrent in self.torrents.iter() {
               torrent
                  .tell(TorrentMessage::SetState(TorrentState::Downloading))
                  .await
                  .expect("Failed to start torrent");
            }
         }
      };
   }
}

impl Message<EngineRequest> for EngineActor {
   type Reply = Result<EngineResponse, EngineError>;

   async fn handle(
      &mut self, msg: EngineRequest, _: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match msg {
         EngineRequest::Torrent(metainfo) => {
            let info_hash = metainfo
               .info_hash()
               .map_err(|e| {
                  error!(error = %e, "Failed to unwrap info hash");
               })
               .expect("Failed to unwrap info hash");

            if self.torrents.contains_key(&info_hash) {
               error!(
                  ?info_hash,
                  "Torrent already exists; ignoring duplicate EngineRequest::Torrent"
               );
               return Err(EngineError::TorrentAlreadyExists(info_hash));
            }

            let torrent_ref = TorrentActor::spawn_in_thread_with_mailbox(
               TorrentActorArgs {
                  peer_id: self.peer_id,
                  metainfo: *metainfo,
                  utp_server: self.utp_socket.clone(),
                  tracker_server: self.udp_server.clone(),
                  primary_addr: None,
                  piece_storage: self.default_piece_storage_strategy.clone(),
                  autostart: self.autostart,
                  sufficient_peers: self.sufficient_peers,
                  base_path: self.default_base_path.clone(),
               },
               // if the size is 0, we use an unbounded mailbox
               match self.mailbox_size {
                  0 => {
                     warn!(
                        ?info_hash,
                        "Spawning torrent with unbounded mailbox; this could drastically increase memory usage"
                     );
                     mailbox::unbounded()
                  }
                  size => mailbox::bounded(size),
               },
            );

            self.actor_ref.link(&torrent_ref).await;

            self.torrents.insert(info_hash, torrent_ref.clone());
            Ok(EngineResponse::Torrent(torrent_ref))
         }
      }
   }
}
