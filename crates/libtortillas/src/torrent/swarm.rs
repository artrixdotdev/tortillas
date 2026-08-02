use futures::{StreamExt, stream};
use kameo::{
   actor::{ActorRef, Spawn},
   mailbox,
   prelude::Message,
   supervision::RestartPolicy,
};
use tokio::{sync::oneshot, time::timeout};
use tracing::{debug, instrument, trace, warn};

use super::{ConfiguredTracker, ConnectedPeer, TorrentActor};
use crate::{
   errors::{TorrentError, map_torrent_send_error},
   peer::{PeerActor, PeerActorArgs, PeerId, WirePeer},
   protocol::{
      messages::{Handshake, PeerMessages},
      stream::{PeerSend, PeerStream, validate_handshake},
   },
   torrent::events::PeerConnected,
   tracker::{Tracker, TrackerActor, TrackerActorArgs, TrackerUpdate},
};
#[cfg(feature = "live")]
use crate::{
   live::{PeerIdentity, PeerView, TrackerStatus, TrackerView},
   metrics::TrackerMetrics,
};

impl TorrentActor {
   #[instrument(skip(self, peer, stream), fields(%self, peer_addr = ?peer.socket_addr(), torrent_id = %self.info_hash()))]
   pub(super) fn append_peer(&self, peer: WirePeer, stream: Option<(PeerStream, [u8; 8])>) {
      self.spawn_peer_connection(peer, stream, None);
   }

   pub(super) fn spawn_peer_connection(
      &self, mut peer: WirePeer, stream: Option<(PeerStream, [u8; 8])>,
      result: Option<oneshot::Sender<Result<ConnectedPeer, TorrentError>>>,
   ) {
      let info_hash = self.info_hash();
      let actor_ref = self.actor_ref.clone();
      let our_id = self.id;
      let utp_server = self.utp_server.clone();
      let handshake_timeout = self.settings.engine.incoming_peer_handshake_timeout;

      // Handshakes may involve network timeouts, so keep them outside the
      // torrent actor's mailbox. Explicit additions receive the result through
      // `result`; discovery paths remain fire-and-report.
      tokio::spawn(async move {
         let mut id = peer.id;
         let connection = async {
            let (stream, reserved, handshake_id) = timeout(handshake_timeout, async {
               match stream {
                  Some((mut stream, reserved)) => {
                     let handshake = Handshake::new(info_hash, our_id);
                     stream.send(PeerMessages::Handshake(handshake)).await?;
                     Ok::<_, TorrentError>((stream, reserved, None))
                  }
                  None => {
                     let mut stream =
                        PeerStream::connect(peer.socket_addr(), Some(utp_server)).await?;
                     stream.send_handshake(our_id, info_hash).await?;
                     let handshake = stream.recv_handshake_message().await?;
                     validate_handshake(&handshake, peer.socket_addr(), info_hash)?;
                     Ok::<_, TorrentError>((stream, handshake.reserved, Some(handshake.peer_id)))
                  }
               }
            })
            .await
            .map_err(|_| {
               TorrentError::PeerActor(crate::errors::PeerActorError::PeerTimeout {
                  seconds: handshake_timeout.as_secs().max(1),
               })
            })??;
            id = id.or(handshake_id);

            let id = id.ok_or_else(|| TorrentError::InvalidOperation {
               operation: "add peer",
               reason: "peer connection completed without a peer id".to_string(),
            })?;

            if id == our_id {
               return Err(TorrentError::InvalidOperation {
                  operation: "add peer",
                  reason: "a torrent cannot connect to its own peer id".to_string(),
               });
            }

            peer.id = Some(id);

            // Registration is the boundary where the connection becomes part
            // of the swarm, so a manual add does not succeed before this ask.
            actor_ref
               .ask(PeerConnected {
                  peer,
                  reserved,
                  stream,
               })
               .await
               .map_err(|error| map_torrent_send_error("register connected peer", error))
         }
         .await;

         if let Some(result_tx) = result {
            let _ = result_tx.send(connection);
         } else if let Err(error) = connection {
            debug!(%error, "Failed to add discovered peer");
         }
      });
   }

   pub(super) fn insert_peer(
      &mut self, peer: WirePeer, reserved: [u8; 8], stream: PeerStream,
   ) -> Result<ConnectedPeer, TorrentError> {
      let Some(id) = peer.id else {
         return Err(TorrentError::InvalidOperation {
            operation: "register connected peer",
            reason: "connected peer is missing its peer id".to_string(),
         });
      };

      let actor_ref = self.actor_ref.clone();
      let info_hash = self.info_hash();
      let peer_settings = self.settings.peer.clone();
      let peer_mailbox_size = self.settings.torrent.peer_mailbox_size;
      if self.peers.contains_key(&id) {
         return Err(TorrentError::PeerAlreadyConnected { peer_id: id });
      }

      #[cfg(feature = "live")]
      let Some(peer_handle) = self.hub.register_peer_scope(
         PeerIdentity {
            torrent: info_hash,
            peer: id,
         },
         PeerView::connected(peer.socket_addr(), id),
      ) else {
         return Err(TorrentError::ActorCommunicationFailed {
            operation: "register peer live scope",
            reason: "live hub is no longer available".to_string(),
         });
      };

      let peer_args = PeerActorArgs {
         peer,
         reserved,
         stream,
         supervisor: actor_ref,
         info_hash,
         settings: peer_settings,
         #[cfg(feature = "live")]
         live_handle: peer_handle.clone(),
      };
      let peer_actor = PeerActor::spawn_with_mailbox(
         peer_args,
         match peer_mailbox_size {
            0 => mailbox::unbounded(),
            size => mailbox::bounded(size),
         },
      );
      self.peers.insert(id, peer_actor);
      self.publish_updated();
      crate::live_only!(self.hub.emit_peer_connected(&peer_handle));
      #[cfg(feature = "live")]
      return Ok(peer_handle);
      #[cfg(not(feature = "live"))]
      Ok(id)
   }

   pub(super) async fn register_tracker_actor(
      &mut self, tracker: Tracker,
   ) -> Result<ConfiguredTracker, TorrentError> {
      // Startup trackers and runtime additions share this path so the actor
      // registry and live projection cannot drift apart.
      let endpoint = tracker.redacted_endpoint();
      if self.trackers.contains_key(&tracker) {
         return Err(TorrentError::TrackerAlreadyExists { endpoint });
      }
      if matches!(tracker, Tracker::Websocket(_)) {
         return Err(TorrentError::UnsupportedTrackerProtocol {
            protocol: "websocket",
         });
      }

      #[cfg(feature = "live")]
      let Some(tracker_handle) = self.hub.register_tracker_scope(
         self.info_hash(),
         &tracker,
         TrackerView {
            endpoint: endpoint.clone(),
            status: TrackerStatus::Pending,
            metrics: TrackerMetrics::default(),
         },
      ) else {
         return Err(TorrentError::ActorCommunicationFailed {
            operation: "register tracker live scope",
            reason: "live hub is no longer available".to_string(),
         });
      };

      let actor = TrackerActor::supervise(
         &self.actor_ref,
         TrackerActorArgs {
            tracker: tracker.clone(),
            peer_id: self.id,
            server: self.tracker_server.clone(),
            socket_addr: self.primary_addr,
            initial_left: self
               .tracker_announce_progress()
               .map(|progress| progress.left),
            supervisor: self.actor_ref.clone(),
            scheduler: self.scheduler.clone(),
            settings: self.settings.tracker.clone(),
            #[cfg(feature = "live")]
            live_handle: tracker_handle.clone(),
         },
      )
      .restart_policy(RestartPolicy::Transient)
      .restart_limit(
         self.settings.torrent.tracker_restart.limit,
         self.settings.torrent.tracker_restart.period,
      )
      .spawn()
      .await;

      self.trackers.insert(tracker, actor);
      self.publish_updated();
      #[cfg(feature = "live")]
      return Ok(tracker_handle);
      #[cfg(not(feature = "live"))]
      Ok(())
   }

   #[instrument(skip(self, tell), fields(torrent_id = %self.info_hash(), msg = ?tell))]
   pub(super) async fn broadcast_to_peers<M>(&mut self, tell: M)
   where
      PeerActor: Message<M, Reply = ()>,
      M: Clone + std::fmt::Debug + Send + 'static,
   {
      let actor_refs: Vec<(PeerId, ActorRef<PeerActor>)> = self
         .peers
         .iter()
         .map(|(id, actor)| (*id, actor.clone()))
         .collect();
      let mut dead_peers = Vec::new();

      stream::iter(actor_refs)
         .for_each_concurrent(
            self.settings.torrent.peer_broadcast_concurrency,
            |(id, actor)| {
               let msg = tell.clone();
               async move {
                  if actor.is_alive() {
                     if let Err(e) = actor.tell(msg).await {
                        warn!(error = %e, peer_id = %id, "Failed to send to peer");
                     }
                  } else {
                     trace!(peer_id = %id, "Peer actor is dead, removing from peers set");
                  }
               }
            },
         )
         .await;

      for (id, actor) in &self.peers {
         if !actor.is_alive() {
            dead_peers.push(*id);
         }
      }
      let removed_dead_peers = !dead_peers.is_empty();
      for id in dead_peers {
         self.peers.remove(&id);
      }
      if removed_dead_peers {
         self.publish_updated();
      }
   }

   /// Enqueues an advisory peer message without allowing a slow peer mailbox
   /// to block the torrent actor's piece-processing loop.
   pub(super) fn broadcast_to_peers_best_effort<M>(&mut self, message: M)
   where
      PeerActor: Message<M, Reply = ()>,
      M: Clone + std::fmt::Debug + Send + 'static,
   {
      let mut dead_peers = Vec::new();

      for (id, actor) in &self.peers {
         if !actor.is_alive() {
            dead_peers.push(*id);
            continue;
         }

         if let Err(error) = actor.tell(message.clone()).try_send() {
            trace!(%error, peer_id = %id, "Peer mailbox unavailable for advisory message");
         }
      }

      for id in dead_peers {
         self.peers.remove(&id);
         self.piece_scheduler.peer_disconnected(id);
      }
   }

   #[instrument(skip(self, message), fields(torrent_id = %self.info_hash()))]
   pub(super) async fn update_trackers(&mut self, message: TrackerUpdate) {
      let actor_refs: Vec<(Tracker, ActorRef<TrackerActor>)> = self
         .trackers
         .iter()
         .map(|(tracker, actor)| (tracker.clone(), actor.clone()))
         .collect();
      let mut dead_trackers = Vec::new();

      stream::iter(actor_refs)
          .for_each_concurrent(self.settings.torrent.tracker_broadcast_concurrency, |(uri, actor)| {
            let msg = message.clone();
            async move {
               if actor.is_alive() {
                  if let Err(e) = actor.tell(msg).await {
                     warn!(error = %e, tracker_uri = ?uri, "Failed to send to tracker");
                  }
               } else {
                  trace!(tracker_uri = ?uri, "Tracker actor is dead, removing from trackers set");
               }
            }
         })
         .await;

      for (tracker, actor) in &self.trackers {
         if !actor.is_alive() {
            dead_trackers.push(tracker.clone());
         }
      }
      for tracker in dead_trackers {
         self.trackers.remove(&tracker);
      }
   }

   /// Enqueues coalescible tracker state without allowing a slow announce
   /// actor to stop piece processing. Lifecycle and explicit announce
   /// messages continue to use the reliable async broadcast path.
   pub(super) fn update_trackers_best_effort(&mut self, message: TrackerUpdate) {
      let mut dead_trackers = Vec::new();

      for (tracker, actor) in &self.trackers {
         if !actor.is_alive() {
            dead_trackers.push(tracker.clone());
            continue;
         }

         if let Err(error) = actor.tell(message.clone()).try_send() {
            trace!(%error, tracker_uri = ?tracker, "Tracker mailbox unavailable for progress update");
         }
      }

      for tracker in dead_trackers {
         self.trackers.remove(&tracker);
      }
   }

   pub(super) async fn broadcast_to_trackers<M>(&mut self, tell: M)
   where
      TrackerActor: Message<M>,
      M: Clone + std::fmt::Debug + Send + 'static,
   {
      let actor_refs: Vec<(Tracker, ActorRef<TrackerActor>)> = self
         .trackers
         .iter()
         .map(|(tracker, actor)| (tracker.clone(), actor.clone()))
         .collect();
      let mut dead_trackers = Vec::new();

      stream::iter(actor_refs)
          .for_each_concurrent(self.settings.torrent.tracker_broadcast_concurrency, |(uri, actor)| {
            let msg = tell.clone();
            async move {
               if actor.is_alive() {
                  if let Err(e) = actor.tell(msg).await {
                     warn!(error = %e, tracker_uri = ?uri, "Failed to send to tracker");
                  }
               } else {
                  trace!(tracker_uri = ?uri, "Tracker actor is dead, removing from trackers set");
               }
            }
         })
         .await;

      for (tracker, actor) in &self.trackers {
         if !actor.is_alive() {
            dead_trackers.push(tracker.clone());
         }
      }
      for tracker in dead_trackers {
         self.trackers.remove(&tracker);
      }
   }
}
