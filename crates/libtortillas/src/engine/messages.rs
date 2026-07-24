use futures::future::try_join_all;
use kameo::{actor::Spawn, mailbox, messages, prelude::ActorRef, supervision::RestartPolicy};
use tokio::time::timeout;
use tracing::{error, warn};

use super::{ENGINE_SNAPSHOT_VERSION, EngineActor, EngineSnapshot};
use crate::{
   dht::messages::commands::{RegisterTorrent, UnregisterTorrent},
   errors::EngineError,
   hashes::InfoHash,
   metainfo::MetaInfo,
   peer::Peer,
   protocol::stream::{PeerStream, validate_handshake_protocol},
   torrent::{self, Torrent, TorrentActor, TorrentActorArgs, TorrentSnapshot, TorrentState},
};

pub(crate) mod commands {
   use anyhow::anyhow;

   use super::*;

   impl EngineActor {
      async fn discard_restored_torrent(
         &mut self, info_hash: InfoHash, torrent: &ActorRef<TorrentActor>,
      ) {
         if self.torrents.remove(&info_hash).is_some()
            && let Some(dht) = &self.dht
            && let Err(error) = dht.tell(UnregisterTorrent { info_hash }).await
         {
            warn!(error = %error, %info_hash, "Failed to unregister rejected restored torrent from DHT");
         }
         if let Err(error) = torrent.stop_gracefully().await {
            warn!(error = %error, %info_hash, "Failed to stop rejected restored torrent");
         }
         self.frontend.torrent_removed(info_hash);
      }
   }

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

      /// Returns a managed torrent actor for public handle construction.
      #[message]
      pub(crate) fn get_torrent(
         &self, info_hash: InfoHash,
      ) -> Result<ActorRef<TorrentActor>, EngineError> {
         self
            .torrents
            .get(&info_hash)
            .map(|torrent| torrent.clone())
            .ok_or(EngineError::TorrentNotFound(info_hash))
      }

      /// Removes a torrent actor from the engine and stops it gracefully.
      #[message]
      pub(crate) async fn remove_torrent(
         &mut self, info_hash: InfoHash,
      ) -> Result<ActorRef<TorrentActor>, EngineError> {
         let Some((_, torrent)) = self.torrents.remove(&info_hash) else {
            return Err(EngineError::TorrentNotFound(info_hash));
         };

         if let Some(dht) = &self.dht
            && let Err(err) = dht.tell(UnregisterTorrent { info_hash }).await
         {
            warn!(error = %err, %info_hash, "Failed to unregister torrent from DHT");
         }

         Ok(torrent)
      }

      /// Creates a new [`Torrent`](crate::torrent::Torrent) actor.
      #[message]
      pub(crate) async fn create_torrent(
         &mut self, metainfo: Box<MetaInfo>, restore: Option<Box<TorrentSnapshot>>,
      ) -> Result<ActorRef<TorrentActor>, EngineError> {
         if let Some(snapshot) = restore.as_ref() {
            snapshot.validate()?;
         }
         let info_hash = metainfo.info_hash().map_err(|e| {
            error!(error = %e, "Failed to unwrap info hash");
            EngineError::Other(e)
         })?;
         let is_private = metainfo.is_private();

         if self.torrents.contains_key(&info_hash) {
            error!(
               ?info_hash,
               "Torrent already exists; ignoring duplicate create_torrent request"
            );
            return Err(EngineError::TorrentAlreadyExists(info_hash));
         }

         let restoring = restore.is_some();
         let piece_storage = restore.as_ref().map_or_else(
            || self.default_piece_storage_strategy.clone(),
            |snapshot| snapshot.piece_storage.clone(),
         );
         let base_path = restore
            .as_ref()
            .and_then(|snapshot| snapshot.output_path.clone())
            .or_else(|| self.default_base_path.clone());
         let torrent_ref = TorrentActor::supervise(
            &self.actor_ref,
            TorrentActorArgs {
               peer_id: self.peer_id,
               metainfo: *metainfo,
               utp_server: self.utp_socket.clone(),
               tracker_server: self.udp_server.clone(),
               primary_addr: None,
               piece_storage,
               autostart: restoring.then_some(false),
               sufficient_peers: restoring.then_some(usize::MAX),
               base_path,
               settings: self.settings.clone(),
               frontend: self.frontend.weak(),
            },
         )
         .restart_policy(RestartPolicy::Transient)
         .restart_limit(
            self.settings.engine.torrent_restart.limit,
            self.settings.engine.torrent_restart.period,
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

         let resume = if let Some(snapshot) = restore {
            match torrent_ref
               .ask(torrent::commands::RestoreSnapshot {
                  snapshot: *snapshot,
               })
               .await
            {
               Ok(result) => match result.0 {
                  Ok(resume) => resume,
                  Err(error) => {
                     self.discard_restored_torrent(info_hash, &torrent_ref).await;
                     return Err(error.into());
                  }
               },
               Err(error) => {
                  self.discard_restored_torrent(info_hash, &torrent_ref).await;
                  return Err(EngineError::Other(anyhow!(
                     "failed to restore torrent snapshot: {error}"
                  )));
               }
            }
         } else {
            false
         };

         self.torrents.insert(info_hash, torrent_ref.clone());
         // BEP 27 requires private torrents to use only their declared trackers:
         // https://www.bittorrent.org/beps/bep_0027.html
         if !is_private && let Some(dht) = &self.dht {
            match self.tcp_socket.local_addr() {
               Ok(addr) => {
                  if let Err(err) = dht
                     .tell(RegisterTorrent {
                        info_hash,
                        torrent: torrent_ref.clone(),
                        port: addr.port(),
                     })
                     .await
                  {
                     warn!(error = %err, %info_hash, "Failed to register torrent with DHT");
                  }
               }
               Err(err) => {
                  warn!(error = %err, %info_hash, "Failed to resolve local port for DHT registration");
               }
            }
         }
         if resume
            && let Err(error) = torrent_ref
               .ask(torrent::commands::SetState {
                  state: TorrentState::Downloading,
               })
               .await
         {
            self.discard_restored_torrent(info_hash, &torrent_ref).await;
            return Err(EngineError::Other(anyhow!(
               "failed to resume restored torrent: {error}"
            )));
         }
         let initial_view = match torrent_ref.ask(torrent::commands::GetLiveView).await {
            Ok(view) => *view,
            Err(error) => {
               self.discard_restored_torrent(info_hash, &torrent_ref).await;
               return Err(EngineError::Other(anyhow!(
                  "failed to initialize torrent frontend: {error}"
               )));
            }
         };
         self.frontend.torrent_added(Torrent::new_with_frontend(
            info_hash,
            torrent_ref.clone(),
            &self.frontend,
            Some(initial_view),
         ));
         Ok(torrent_ref)
      }

      /// Atomically validates and restores an engine snapshot against the
      /// authoritative actor state.
      #[message]
      pub(crate) async fn restore_engine(
         &mut self, snapshot: EngineSnapshot,
      ) -> Result<Vec<InfoHash>, EngineError> {
         snapshot.validate()?;
         if !self.torrents.is_empty() {
            return Err(EngineError::InvalidSnapshot {
               reason: "target engine already manages torrents".to_string(),
            });
         }

         let mut restored = Vec::with_capacity(snapshot.torrents.len());
         for torrent in snapshot.torrents {
            let info_hash = torrent.info_hash;
            let result = self
               .create_torrent(Box::new(torrent.metainfo.clone()), Some(Box::new(torrent)))
               .await;
            match result {
               Ok(_) => restored.push(info_hash),
               Err(error) => {
                  for info_hash in restored.drain(..) {
                     match self.remove_torrent(info_hash).await {
                        Ok(torrent) => {
                           torrent.kill();
                           self.frontend.torrent_removed(info_hash);
                        }
                        Err(remove_error) => {
                           warn!(
                              error = %remove_error,
                              %info_hash,
                              "Failed to roll back restored torrent"
                           );
                        }
                     }
                  }
                  return Err(error);
               }
            }
         }

         Ok(restored)
      }

      /// Captures resumable state for every managed torrent.
      #[message]
      pub(crate) async fn snapshot_engine(&self) -> Result<EngineSnapshot, EngineError> {
         let futures = self
            .torrents
            .iter()
            .map(|torrent| {
               let torrent = torrent.clone();
               async move {
                  torrent
                     .ask(torrent::commands::SnapshotState)
                     .await
                     .map(|snapshot| *snapshot)
                     .map_err(|err| {
                        EngineError::Other(anyhow!("failed to get torrent snapshot: {err}"))
                     })
               }
            })
            .collect::<Vec<_>>();

         let torrents = try_join_all(futures).await?;

         Ok(EngineSnapshot {
            version: ENGINE_SNAPSHOT_VERSION,
            torrents,
         })
      }
   }
}
