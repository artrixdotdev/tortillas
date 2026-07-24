use std::sync::Arc;

use futures::{StreamExt, stream};
use kameo::{
   actor::{ActorRef, Spawn},
   mailbox,
   prelude::Message,
};
use tracing::{debug, instrument, trace, warn};

use super::TorrentActor;
use crate::{
   frontend::PeerView,
   peer::{Peer, PeerActor, PeerId},
   protocol::{
      messages::{Handshake, PeerMessages},
      stream::{PeerSend, PeerStream, validate_handshake},
   },
   torrent::events::PeerConnected,
   tracker::{Tracker, TrackerActor, TrackerUpdate},
};

impl TorrentActor {
   #[instrument(skip(self, peer, stream), fields(%self, peer_addr = ?peer.socket_addr(), torrent_id = %self.info_hash()))]
   pub(super) fn append_peer(&self, mut peer: Peer, stream: Option<PeerStream>) {
      let info_hash = Arc::new(self.info_hash());
      let actor_ref = self.actor_ref.clone();
      let our_id = self.id;
      let utp_server = self.utp_server.clone();

      tokio::spawn(async move {
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
                        Ok(_) => match stream.recv_handshake_message().await {
                           Ok(handshake) => {
                              if let Err(err) = validate_handshake(
                                 &handshake,
                                 peer.socket_addr(),
                                 Arc::clone(&info_hash),
                              ) {
                                 trace!(error = %err, peer_addr = %peer.socket_addr(), "Failed to validate peer handshake; exiting");
                                 return;
                              }
                              id = Some(handshake.peer_id);
                              peer.reserved = handshake.reserved;
                              peer.determine_supported().await;
                              stream
                           }
                           Err(err) => {
                              trace!(error = %err, peer_addr = %peer.socket_addr(), "Failed to receive handshake from peer; exiting");
                              return;
                           }
                        },
                        Err(err) => {
                           trace!(error = %err, peer_addr = %peer.socket_addr(), "Failed to send handshake to peer; exiting");
                           return;
                        }
                     }
                  }
                  Err(err) => {
                     trace!(error = %err, peer_addr = %peer.socket_addr(), "Failed to connect to peer; exiting");
                     return;
                  }
               }
            }
         };

         let Some(id) = id else {
            trace!(peer_addr = %peer.socket_addr(), "Peer connection completed without a peer id; exiting");
            return;
         };

         if id == our_id {
            return;
         }

         peer.id = Some(id);

         if let Err(err) = actor_ref.tell(PeerConnected { peer, stream }).await {
            warn!(?err, peer_id = %id, "Failed to route connected peer back to torrent actor");
         }
      });
   }

   pub(super) fn insert_peer(&mut self, peer: Peer, stream: PeerStream) {
      let Some(id) = peer.id else {
         trace!(peer_addr = %peer.socket_addr(), "Connected peer missing peer id; ignoring");
         return;
      };

      let actor_ref = self.actor_ref.clone();
      let info_hash = self.info_hash();
      let peer_settings = self.settings.peer.clone();
      let peer_mailbox_size = self.settings.torrent.peer_mailbox_size;
      let peer_view = PeerView {
         address: Some(peer.socket_addr()),
         client: Some(id.client_name().to_string()),
         connected: true,
      };

      if self.peers.contains_key(&id) {
         return;
      }

      let peer_actor = PeerActor::spawn_with_mailbox(
         (peer, stream, actor_ref, info_hash, peer_settings),
         match peer_mailbox_size {
            0 => mailbox::unbounded(),
            size => mailbox::bounded(size),
         },
      );
      self.peers.insert(id, peer_actor);
      self.frontend.peer_connected(self.live_view(), peer_view);
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
      for id in dead_peers {
         self.peers.remove(&id);
         self.frontend.peer_disconnected(
            self.live_view(),
            PeerView {
               address: None,
               client: Some(id.client_name().to_string()),
               connected: false,
            },
         );
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
