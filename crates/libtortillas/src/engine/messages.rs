use futures::future::try_join_all;
use kameo::{actor::Spawn, mailbox, messages, prelude::ActorRef, supervision::RestartPolicy};
use tokio::time::timeout;
use tracing::{error, warn};

use super::{EngineActor, EngineExport};
use crate::{
   errors::EngineError,
   metainfo::MetaInfo,
   peer::Peer,
   protocol::stream::{PeerStream, validate_handshake_protocol},
   torrent::{self, TorrentActor, TorrentActorArgs, TorrentState},
};

pub(crate) mod commands {
   use anyhow::anyhow;

   use super::*;

   #[messages]
   impl EngineActor {
      /// Handles an incoming peer connection. The peer has been neither
      /// handshaked nor verified at this point.
      #[message]
      pub(crate) async fn incoming_peer(&mut self, mut stream: PeerStream) {
         let handshake_timeout = self.settings.engine.incoming_peer_handshake_timeout;
         let handshake = match timeout(handshake_timeout, stream.recv_handshake_message()).await {
            Ok(Ok(handshake)) => handshake,
            Ok(Err(err)) => {
               warn!(error = %err, %stream, "Failed to read incoming peer handshake");
               return;
            }
            Err(_) => {
               warn!(%stream, timeout = ?handshake_timeout, "Timed out reading incoming peer handshake");
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

         if let Err(err) = validate_handshake_protocol(&handshake, peer_addr) {
            warn!(error = %err, %stream, "Rejected incoming peer handshake");
            return;
         }

         let info_hash = *handshake.info_hash;
         let mut peer = Peer::from_socket_addr(peer_addr);

         // Populate peer fields from parsed handshake.
         peer.id = Some(handshake.peer_id);
         peer.reserved = handshake.reserved;

         if let Some(torrent) = self.torrents.get(&info_hash) {
            if let Err(err) = torrent
               .tell(torrent::events::IncomingPeer { peer, stream })
               .await
            {
               warn!(error = %err, %info_hash, "Failed to route incoming peer to torrent");
            }
         } else {
            error!(%stream, "Received incoming peer for unknown torrent, killing connection");
            drop(stream);
         }
      }

      /// Starts all torrents managed by the engine.
      #[message]
      pub(crate) async fn start_all(&self) {
         for torrent in self.torrents.iter() {
            if let Err(err) = torrent
               .tell(torrent::commands::SetState {
                  state: TorrentState::Downloading,
               })
               .await
            {
               warn!(error = %err, "Failed to start torrent");
            }
         }
      }

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
               autostart: None,
               sufficient_peers: None,
               base_path: self.default_base_path.clone(),
               settings: self.settings.clone(),
            },
         )
         .restart_policy(RestartPolicy::Permanent)
         .restart_limit(
            self.settings.engine.torrent_restart_limit,
            self.settings.engine.torrent_restart_period,
         )
         .spawn_with_mailbox(match self.settings.engine.torrent_mailbox_size {
            0 => {
               warn!(
                  ?info_hash,
                  "Spawning torrent with unbounded mailbox; this could drastically increase memory usage"
               );
               mailbox::unbounded()
            }
            size => mailbox::bounded(size),
         })
         .await;

         self.torrents.insert(info_hash, torrent_ref.clone());
         Ok(torrent_ref)
      }

      /// Exports the current state of the engine.
      #[message]
      pub(crate) async fn export_engine(&self) -> Result<EngineExport, EngineError> {
         let futures = self
            .torrents
            .iter()
            .map(|torrent| {
               let torrent = torrent.clone();
               async move {
                  torrent
                     .ask(torrent::commands::ExportState)
                     .await
                     .map(|export| *export)
                     .map_err(|err| {
                        EngineError::Other(anyhow!("failed to get torrent export: {err}"))
                     })
               }
            })
            .collect::<Vec<_>>();

         let torrents = try_join_all(futures).await?;

         Ok(EngineExport { torrents })
      }
   }
}
