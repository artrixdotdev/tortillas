use futures::future::join_all;
use kameo::{
   actor::Spawn,
   mailbox, messages,
   prelude::{ActorRef, Context, Message},
   supervision::RestartPolicy,
};
use tokio::time::{Duration, timeout};
use tracing::{error, warn};

use super::{EngineActor, EngineExport};
use crate::{
   errors::EngineError,
   metainfo::MetaInfo,
   peer::Peer,
   protocol::{
      messages::PeerMessages,
      stream::{PeerRecv, PeerStream},
   },
   torrent::{ExportState, TorrentActor, TorrentActorArgs, TorrentMessage, TorrentState},
};

const INCOMING_PEER_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) enum EngineMessage {
   /// Handles an incoming peer connection. The peer has been neither handshaked
   /// nor verified at this point.
   IncomingPeer(Box<PeerStream>),
   /// Starts all torrents managed by the engine.
   StartAll,
}

impl Message<EngineMessage> for EngineActor {
   type Reply = ();
   async fn handle(
      &mut self, msg: EngineMessage, _: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match msg {
         EngineMessage::IncomingPeer(mut stream) => {
            let msg = match timeout(INCOMING_PEER_HANDSHAKE_TIMEOUT, stream.recv()).await {
               Ok(Ok(msg)) => msg,
               Ok(Err(err)) => {
                  warn!(error = %err, %stream, "Failed to read incoming peer handshake");
                  return;
               }
               Err(_) => {
                  warn!(%stream, timeout = ?INCOMING_PEER_HANDSHAKE_TIMEOUT, "Timed out reading incoming peer handshake");
                  return;
               }
            };
            let peer_addr = match stream.remote_addr() {
               Ok(addr) => addr,
               Err(err) => {
                  warn!(error = %err, %stream, "Failed to get incoming peer remote address");
                  return;
               }
            };

            if let PeerMessages::Handshake(handshake) = msg {
               let info_hash = *handshake.info_hash;
               let mut peer = Peer::from_socket_addr(peer_addr);

               // Populate peer fields from parsed handshake
               peer.id = Some(handshake.peer_id);
               peer.reserved = handshake.reserved;

               if let Some(torrent) = self.torrents.get(&info_hash) {
                  if let Err(err) = torrent
                     .tell(TorrentMessage::IncomingPeer(peer, stream))
                     .await
                  {
                     warn!(error = %err, %info_hash, "Failed to route incoming peer to torrent");
                  }
               } else {
                  error!(%stream, "Received incoming peer for unknown torrent, killing connection");
                  drop(stream);
               }
            } else {
               error!(message = %msg, "Received unexpected message from peer");
            }
         }
         EngineMessage::StartAll => {
            for torrent in self.torrents.iter() {
               if let Err(err) = torrent
                  .tell(TorrentMessage::SetState(TorrentState::Downloading))
                  .await
               {
                  warn!(error = %err, "Failed to start torrent");
               }
            }
         }
      };
   }
}

#[messages]
impl EngineActor {
   /// Creates a new [`Torrent`](crate::torrent::Torrent) actor.
   #[message]
   pub(crate) async fn create_torrent(
      &mut self, metainfo: Box<MetaInfo>,
   ) -> Result<ActorRef<TorrentActor>, EngineError> {
      let info_hash = metainfo.info_hash().map_err(|e| {
         error!(error = %e, "Failed to unwrap info hash");
         EngineError::Other(e)
      })?;

      if self.torrents.contains_key(&info_hash) {
         error!(
            ?info_hash,
            "Torrent already exists; ignoring duplicate create_torrent request"
         );
         return Err(EngineError::TorrentAlreadyExists(info_hash));
      }

      let torrent_ref = TorrentActor::supervise(
         &self.actor_ref,
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
      )
      .restart_policy(RestartPolicy::Permanent)
      .restart_limit(3, Duration::from_secs(60))
      .spawn_with_mailbox(
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
      )
      .await;

      self.torrents.insert(info_hash, torrent_ref.clone());
      Ok(torrent_ref)
   }

   /// Exports the current state of the engine.
   #[message]
   pub(crate) async fn export_engine(&self) -> Result<EngineExport, EngineError> {
      // Concurrently collect all torrent exports
      let futures = self
         .torrents
         .iter()
         .map(|torrent| {
            let torrent = torrent.clone();
            async move {
               *torrent
                  .ask(ExportState)
                  .await
                  .expect("Failed to get torrent export")
            }
         })
         .collect::<Vec<_>>();

      let torrents = join_all(futures).await;

      Ok(EngineExport { torrents })
   }
}
