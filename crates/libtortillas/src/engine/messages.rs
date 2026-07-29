use futures::future::try_join_all;
use kameo::{actor::Spawn, mailbox, messages, prelude::ActorRef, supervision::RestartPolicy};
use tokio::time::timeout;
use tracing::{error, warn};

use super::{ENGINE_SNAPSHOT_VERSION, EngineActor, EngineSnapshot};
#[cfg(feature = "live")]
use crate::torrent::Torrent;
use crate::{
   dht::messages::commands::{RegisterTorrent, UnregisterTorrent},
   errors::{EngineError, map_torrent_send_error},
   hashes::InfoHash,
   metainfo::MetaInfo,
   peer::WirePeer,
   protocol::stream::{PeerStream, validate_handshake_protocol},
   torrent::{
      self, RestoreVerification, TorrentActor, TorrentActorArgs, TorrentSnapshot, TorrentState,
      ValidatedTorrentSnapshot,
   },
};

#[derive(Debug)]
pub(crate) enum CreateTorrentRequest {
   New(Box<MetaInfo>),
   Restore {
      snapshot: RestoreSnapshotInput,
      verification: RestoreVerification,
   },
}

#[derive(Debug)]
pub(crate) enum RestoreSnapshotInput {
   Unvalidated(Box<TorrentSnapshot>),
   Validated(Box<ValidatedTorrentSnapshot>),
}

pub(crate) mod commands {
   use super::*;

   impl EngineActor {
      async fn discard_failed_torrent(
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
         crate::live_only!(self.hub.remove_torrent_scope(info_hash));
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

         let info_hash = handshake.info_hash;
         let mut peer = WirePeer::from_socket_addr(peer_addr);

         peer.id = Some(handshake.peer_id);

         if let Some(torrent) = self.torrents.get(&info_hash) {
            if let Err(err) = torrent
               .tell(torrent::events::IncomingPeer {
                  peer,
                  reserved: handshake.reserved,
                  stream,
               })
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
      pub(crate) async fn start_all(&self) -> Result<(), EngineError> {
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
         Ok(())
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

      /// Creates a new [`Torrent`] actor.
      #[message]
      pub(crate) async fn create_torrent(
         &mut self, request: CreateTorrentRequest,
      ) -> Result<ActorRef<TorrentActor>, EngineError> {
         let (metainfo, restore, piece_storage, base_path, resume) = match request {
            CreateTorrentRequest::New(metainfo) => (
               metainfo,
               None,
               self.default_piece_storage_strategy.clone(),
               self.default_base_path.clone(),
               false,
            ),
            CreateTorrentRequest::Restore {
               snapshot,
               verification,
            } => {
               let snapshot = match snapshot {
                  RestoreSnapshotInput::Unvalidated(snapshot) => {
                     ValidatedTorrentSnapshot::try_from(*snapshot)?
                  }
                  RestoreSnapshotInput::Validated(snapshot) => *snapshot,
               }
               .reconcile_storage(verification)
               .await?;
               let piece_storage = snapshot.snapshot().piece_storage.clone();
               let base_path = snapshot.snapshot().output_path.clone();
               let resume = snapshot.snapshot().state.is_transfer_active();
               let (metainfo, state) = snapshot.into_restore_parts();
               (
                  Box::new(metainfo),
                  Some(state),
                  piece_storage,
                  base_path,
                  resume,
               )
            }
         };
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
               #[cfg(feature = "live")]
               hub: self.hub.weak(),
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

         if let Some(snapshot) = restore {
            match torrent_ref
               .ask(torrent::commands::RestoreSnapshot { snapshot })
               .await
            {
               Ok(result) => match result.0 {
                  Ok(_) => {}
                  Err(error) => {
                     self.discard_failed_torrent(info_hash, &torrent_ref).await;
                     return Err(error.into());
                  }
               },
               Err(error) => {
                  self.discard_failed_torrent(info_hash, &torrent_ref).await;
                  return Err(EngineError::ActorCommunicationFailed {
                     operation: "restore torrent snapshot",
                     reason: error.to_string(),
                  });
               }
            }
         }

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
            self.discard_failed_torrent(info_hash, &torrent_ref).await;
            return Err(EngineError::Torrent(map_torrent_send_error(
               "resume restored torrent",
               error,
            )));
         }
         crate::live_only! {
            let initial_view = match torrent_ref.ask(torrent::commands::GetLiveView).await {
               Ok(view) => *view,
               Err(error) => {
                  self.discard_failed_torrent(info_hash, &torrent_ref).await;
                  return Err(EngineError::ActorCommunicationFailed {
                     operation: "initialize torrent live state",
                     reason: error.to_string(),
                  });
               }
            };
            self.hub.register_torrent_scope(Torrent::new_with_hub(
               info_hash,
               torrent_ref.clone(),
               &self.hub,
               Some(initial_view),
            ));
         }
         Ok(torrent_ref)
      }

      /// Atomically validates and restores an engine snapshot against the
      /// authoritative actor state.
      #[message]
      pub(crate) async fn restore_engine(
         &mut self, snapshot: EngineSnapshot, verification: RestoreVerification,
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
               .create_torrent(CreateTorrentRequest::Restore {
                  snapshot: RestoreSnapshotInput::Validated(Box::new(
                     ValidatedTorrentSnapshot::new_validated(torrent),
                  )),
                  verification,
               })
               .await;
            match result {
               Ok(_) => restored.push(info_hash),
               Err(error) => {
                  for info_hash in restored.drain(..) {
                     match self.remove_torrent(info_hash).await {
                        Ok(torrent) => {
                           torrent.kill();
                           crate::live_only!(self.hub.remove_torrent_scope(info_hash));
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
                     .map_err(|error| {
                        EngineError::Torrent(map_torrent_send_error("snapshot torrent", error))
                     })
               }
            })
            .collect::<Vec<_>>();

         let mut torrents = try_join_all(futures).await?;
         torrents.sort_by(|left, right| left.info_hash.as_bytes().cmp(right.info_hash.as_bytes()));

         Ok(EngineSnapshot {
            version: ENGINE_SNAPSHOT_VERSION,
            torrents,
         })
      }
   }
}
