use std::{net::SocketAddr, time::Duration};

use anyhow::Result;
use kameo::{
   Actor,
   actor::{ActorRef, WeakActorRef},
   error::ActorStopReason,
   messages,
   prelude::{Context, Message},
};
use kameo_actors::scheduler::{Scheduler, SetTimeout};
use tokio::{task::AbortHandle, time::timeout};
use tracing::{error, warn};

use super::{
   Tracker, TrackerBase, TrackerInstance, TrackerStats, TrackerUpdate,
   http::HttpTracker,
   udp::{UdpServer, UdpTracker},
};
use crate::{
   errors::TrackerActorError,
   peer::PeerId,
   settings::TrackerSettings,
   torrent::{self, TorrentActor},
};

/// The actor that handles all communication with a given tracker.
pub(crate) struct TrackerActor {
   source: Tracker,
   tracker: TrackerInstance,
   supervisor: ActorRef<TorrentActor>,
   scheduler: ActorRef<Scheduler>,
   next_announce: Option<AbortHandle>,
   actor_ref: ActorRef<Self>,
   settings: TrackerSettings,
}

#[derive(Clone)]
pub(crate) struct TrackerActorArgs {
   pub(crate) tracker: Tracker,
   pub(crate) peer_id: PeerId,
   pub(crate) server: UdpServer,
   pub(crate) socket_addr: SocketAddr,
   pub(crate) initial_left: Option<usize>,
   pub(crate) supervisor: ActorRef<TorrentActor>,
   pub(crate) scheduler: ActorRef<Scheduler>,
   pub(crate) settings: TrackerSettings,
}

impl Actor for TrackerActor {
   type Args = TrackerActorArgs;
   type Error = TrackerActorError;

   async fn on_start(state: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
      let TrackerActorArgs {
         tracker,
         peer_id,
         server,
         socket_addr,
         initial_left,
         supervisor,
         scheduler,
         settings,
      } = state;

      let info_hash = supervisor
         .ask(torrent::commands::GetInfoHash)
         .await
         .map_err(|e| TrackerActorError::SupervisorCommunicationFailed(e.to_string()))?;

      let source = tracker.clone();
      let tracker_uri = tracker.uri();
      let tracker = match tracker {
         Tracker::Udp(uri) => {
            let udp_tracker = UdpTracker::new_with_settings(
               uri.clone(),
               Some(server),
               info_hash,
               (peer_id, socket_addr),
               settings.clone(),
            )
            .await
            .map_err(|_| TrackerActorError::InitializationFailed {
               tracker_type: format!("UDP: {uri}"),
            })?;
            udp_tracker.initialize().await.map_err(|_| {
               TrackerActorError::InitializationFailed {
                  tracker_type: format!("UDP: {uri}"),
               }
            })?;
            TrackerInstance::Udp(udp_tracker)
         }
         Tracker::Http(uri) => {
            let http_tracker = HttpTracker::new_with_settings(
               uri.clone(),
               info_hash,
               Some(peer_id),
               Some(socket_addr),
               settings.clone(),
            );
            http_tracker.initialize().await.map_err(|_| {
               TrackerActorError::InitializationFailed {
                  tracker_type: format!("HTTP: {uri}"),
               }
            })?;
            TrackerInstance::Http(http_tracker)
         }
         _ => {
            return Err(TrackerActorError::UnsupportedProtocol {
               protocol: tracker_uri,
            });
         }
      };

      if let Some(left) = initial_left {
         tracker.update(TrackerUpdate::Left(left)).await?;
      }

      let next_announce = scheduler
         .ask(SetTimeout::new(
            actor_ref.downgrade(),
            settings.initial_announce_delay,
            Announce,
         ))
         .await
         .map_err(|e| TrackerActorError::SupervisorCommunicationFailed(e.to_string()))?;

      Ok(Self {
         source,
         tracker,
         supervisor,
         scheduler,
         next_announce: Some(next_announce),
         actor_ref,
         settings,
      })
   }

   async fn on_stop(
      &mut self, _: WeakActorRef<Self>, _: ActorStopReason,
   ) -> Result<(), Self::Error> {
      if let Some(next_announce) = self.next_announce.take() {
         next_announce.abort();
      }

      let _ = timeout(self.settings.stop_timeout, self.tracker.stop())
         .await
         .inspect_err(|e| warn!(e = %e.to_string(), "Tracker stop timed out"));

      Ok(())
   }
}

#[messages]
impl TrackerActor {
   async fn schedule_next_announce(&mut self) {
      let interval = self.tracker.interval();
      let delay = if interval == usize::MAX || interval == u32::MAX as usize {
         self.settings.fallback_announce_interval
      } else {
         Duration::from_secs(interval as u64)
      };
      let maximum_announce_interval = self
         .settings
         .maximum_announce_interval
         .max(self.settings.minimum_announce_interval);
      let delay = delay
         .max(self.settings.minimum_announce_interval)
         .min(maximum_announce_interval);
      if let Some(next_announce) = self.next_announce.take() {
         next_announce.abort();
      }
      match self
         .scheduler
         .ask(SetTimeout::new(self.actor_ref.downgrade(), delay, Announce))
         .await
      {
         Ok(next_announce) => self.next_announce = Some(next_announce),
         Err(e) => error!(error = %e, "Failed to schedule next announce"),
      }
   }

   /// Forces the tracker to make an announce request.
   #[message(derive(Debug, Clone, Copy))]
   pub(crate) async fn announce(&mut self) -> Option<TrackerStats> {
      match self.tracker.announce().await {
         Ok(peers) => {
            if let Err(e) = self
               .supervisor
               .tell(torrent::events::Announce {
                  peers,
                  from: torrent::AnnounceFrom::Tracker(self.source.clone()),
               })
               .await
            {
               error!(error = %e, "Failed to send announce to supervisor");
            }
         }
         Err(e) => error!(error = %e, "Announce request failed"),
      }
      self.schedule_next_announce().await;
      None
   }
}

impl Message<TrackerUpdate> for TrackerActor {
   type Reply = ();

   async fn handle(
      &mut self, msg: TrackerUpdate, _: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      if let Err(err) = self.tracker.update(msg).await {
         warn!(error = %err, "Failed to update tracker state");
      }
   }
}
