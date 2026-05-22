use std::{net::SocketAddr, time::Duration};

use anyhow::Result;
use kameo::{
   Actor,
   actor::{ActorRef, WeakActorRef},
   error::ActorStopReason,
   mailbox::{MailboxReceiver, Signal},
   prelude::{Context, Message},
};
use tokio::time::{Interval, interval, timeout};
use tracing::{Span, error, warn};

use super::{
   Tracker, TrackerBase, TrackerInstance, TrackerMessage, TrackerStats, TrackerUpdate,
   http::HttpTracker,
   udp::{UdpServer, UdpTracker},
};
use crate::{
   errors::TrackerActorError,
   peer::PeerId,
   torrent::{TorrentActor, TorrentMessage, TorrentRequest, TorrentResponse},
};

/// The actor that handles all communication with a given tracker.
pub(crate) struct TrackerActor {
   tracker: TrackerInstance,
   supervisor: ActorRef<TorrentActor>,
   interval: Interval,
}

impl Actor for TrackerActor {
   type Args = (
      Tracker,
      PeerId,
      UdpServer,
      SocketAddr,
      ActorRef<TorrentActor>,
   );
   type Error = TrackerActorError;

   async fn on_start(state: Self::Args, _: ActorRef<Self>) -> Result<Self, Self::Error> {
      let (tracker, peer_id, server, socket_addr, supervisor) = state;

      let torrent_response = supervisor
         .ask(TorrentRequest::InfoHash)
         .await
         .map_err(|e| TrackerActorError::SupervisorCommunicationFailed(e.to_string()))?;
      let info_hash = match torrent_response {
         TorrentResponse::InfoHash(info_hash) => info_hash,
         _ => unreachable!(),
      };

      let tracker_uri = tracker.uri();
      let tracker = match tracker {
         Tracker::Udp(uri) => {
            let udp_tracker =
               UdpTracker::new(uri.clone(), Some(server), info_hash, (peer_id, socket_addr))
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
            let http_tracker =
               HttpTracker::new(uri.clone(), info_hash, Some(peer_id), Some(socket_addr));
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

      Ok(Self {
         tracker,
         supervisor,
         interval: interval(Duration::from_secs(30)),
      })
   }

   async fn on_stop(
      &mut self, _: WeakActorRef<Self>, _: ActorStopReason,
   ) -> Result<(), Self::Error> {
      let _ = timeout(Duration::from_secs(5), self.tracker.stop())
         .await
         .inspect_err(|e| warn!(e = %e.to_string(), "Tracker stop timed out"));

      Ok(())
   }

   async fn next(
      &mut self, actor_ref: WeakActorRef<Self>, mailbox_rx: &mut MailboxReceiver<Self>,
   ) -> Result<Option<Signal<Self>>, Self::Error> {
      Ok(tokio::select! {
         signal = mailbox_rx.recv() => signal,
         _ = self.interval.tick() => {
            let Some(actor_ref) = actor_ref.upgrade() else {
               return Ok(None);
            };
            Some(Signal::Message {
               message: Box::new(TrackerMessage::Announce),
               actor_ref,
               reply: None,
               sent_within_actor: true,
               message_name: "TrackerMessage",
                caller_span: Span::current(),
            })
         }
      })
   }
}

impl Message<TrackerMessage> for TrackerActor {
   type Reply = Option<TrackerStats>;

   async fn handle(
      &mut self, msg: TrackerMessage, _ctx: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match msg {
         TrackerMessage::Announce => {
            match self.tracker.announce().await {
               Ok(peers) => {
                  if let Err(e) = self.supervisor.tell(TorrentMessage::Announce(peers)).await {
                     error!(error = %e, "Failed to send announce to supervisor");
                  } else {
                     let delay = self.tracker.interval().max(1) as u64;
                     self.interval = interval(Duration::from_secs(delay));
                     self.interval.tick().await;
                  }
               }
               Err(e) => error!(error = %e, "Announce request failed"),
            }
            None
         }
         TrackerMessage::GetStats => Some(self.tracker.stats()),
      }
   }
}

impl Message<TrackerUpdate> for TrackerActor {
   type Reply = ();

   async fn handle(
      &mut self, msg: TrackerUpdate, _ctx: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      if let Err(err) = self.tracker.update(msg).await {
         warn!(error = %err, "Failed to update tracker state");
      }
   }
}
