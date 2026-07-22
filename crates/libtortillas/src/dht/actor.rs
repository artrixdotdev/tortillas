use std::{collections::HashMap, ops::ControlFlow};

use kameo::{
   Actor,
   actor::{ActorId, ActorRef, WeakActorRef},
   error::ActorStopReason,
   mailbox::{MailboxReceiver, Signal},
   supervision::SupervisionStrategy,
};
use tokio::{
   net::lookup_host,
   task::{AbortHandle, JoinSet},
   time::sleep,
};
use tracing::{debug, error, warn};

use super::{
   DhtState, DhtTransport, NodeId, Query, lookup_peers,
   messages::{
      commands::{LookupTorrent, RefreshTorrents},
      events::{IncomingDatagram, LookupFinished},
   },
};
use crate::{hashes::InfoHash, settings::DhtSettings, torrent::TorrentActor};

/// Engine-owned actor for DHT routing state and inbound KRPC traffic.
pub(crate) struct DhtActor {
   pub(super) state: DhtState,
   pub(super) transport: DhtTransport,
   bootstrap_task: Option<AbortHandle>,
   actor_ref: ActorRef<Self>,
   pub(super) settings: DhtSettings,
   pub(super) torrents: HashMap<InfoHash, DhtTorrent>,
   pub(super) lookup_tasks: HashMap<InfoHash, AbortHandle>,
   pub(super) announce_tasks: HashMap<InfoHash, AbortHandle>,
}

#[derive(Clone)]
pub(super) struct DhtTorrent {
   pub(super) actor: ActorRef<TorrentActor>,
   pub(super) port: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct DhtActorArgs {
   pub(crate) id: Option<NodeId>,
   pub(crate) settings: DhtSettings,
}

impl Actor for DhtActor {
   type Args = DhtActorArgs;
   type Error = anyhow::Error;

   fn supervision_strategy() -> SupervisionStrategy {
      SupervisionStrategy::OneForOne
   }

   async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
      let transport = DhtTransport::bind(
         args.settings.bind_addr,
         args.settings.query_timeout,
         args.settings.receive_buffer_size,
      )
      .await?;
      let state = DhtState::new(args.id.unwrap_or_else(NodeId::random), &args.settings);
      let bootstrap_task = (!args.settings.bootstrap_nodes.is_empty()).then(|| {
         tokio::spawn(bootstrap(
            transport.clone(),
            args.settings.bootstrap_nodes.clone(),
            state.id(),
            actor_ref.clone(),
         ))
         .abort_handle()
      });
      Ok(Self {
         state,
         transport,
         bootstrap_task,
         actor_ref,
         settings: args.settings,
         torrents: HashMap::new(),
         lookup_tasks: HashMap::new(),
         announce_tasks: HashMap::new(),
      })
   }

   async fn next(
      &mut self, actor_ref: WeakActorRef<Self>, mailbox_rx: &mut MailboxReceiver<Self>,
   ) -> Result<Option<Signal<Self>>, Self::Error> {
      loop {
         let received = tokio::select! {
            signal = mailbox_rx.recv() => return Ok(signal),
            received = self.transport.receive() => received,
         };
         let (message, addr) = match received {
            Ok(received) => received,
            Err(err) => {
               warn!(error = %err, "Ignored invalid DHT datagram");
               continue;
            }
         };
         let Some(actor_ref) = actor_ref.upgrade() else {
            return Ok(None);
         };
         return Ok(Some(Signal::Message {
            message: Box::new(IncomingDatagram { message, addr }),
            actor_ref,
            reply: None,
            sent_within_actor: true,
            message_name: "IncomingDatagram",
            caller_span: tracing::Span::current(),
         }));
      }
   }

   async fn on_link_died(
      &mut self, _: WeakActorRef<Self>, id: ActorId, reason: ActorStopReason,
   ) -> Result<ControlFlow<ActorStopReason>, Self::Error> {
      error!(?id, ?reason, "Linked DHT child died");
      Ok(ControlFlow::Continue(()))
   }

   async fn on_stop(
      &mut self, _: WeakActorRef<Self>, _: ActorStopReason,
   ) -> Result<(), Self::Error> {
      if let Some(task) = self.bootstrap_task.take() {
         task.abort();
      }
      for (_, task) in self.lookup_tasks.drain() {
         task.abort();
      }
      for (_, task) in self.announce_tasks.drain() {
         task.abort();
      }
      Ok(())
   }
}

impl DhtActor {
   pub(super) fn start_lookup(&mut self, info_hash: InfoHash) {
      if !self.torrents.contains_key(&info_hash) {
         return;
      }
      if let Some(task) = self.lookup_tasks.remove(&info_hash) {
         task.abort();
      }
      let target = NodeId::from(info_hash);
      let seeds = self
         .state
         .routing()
         .closest(target, self.settings.bucket_size);
      if seeds.is_empty() {
         return;
      }
      let transport = self.transport.clone();
      let actor_ref = self.actor_ref.clone();
      let id = self.state.id();
      let concurrency = self.settings.lookup_concurrency;
      let peer_limit = self.settings.lookup_peer_limit;
      let bucket_size = self.settings.bucket_size;
      let task = tokio::spawn(async move {
         let result = lookup_peers(
            transport,
            id,
            target,
            seeds,
            concurrency,
            peer_limit,
            bucket_size,
         )
         .await;
         if let Err(err) = actor_ref.tell(LookupFinished { info_hash, result }).await {
            warn!(error = %err, %info_hash, "Failed to return DHT lookup result");
         }
      });
      self.lookup_tasks.insert(info_hash, task.abort_handle());
   }

   pub(super) fn schedule_lookup(&mut self, info_hash: InfoHash) {
      let actor_ref = self.actor_ref.clone();
      let interval = self.settings.lookup_interval;
      let task = tokio::spawn(async move {
         sleep(interval).await;
         if let Err(err) = actor_ref.tell(LookupTorrent { info_hash }).await {
            warn!(error = %err, %info_hash, "Failed to schedule DHT lookup");
         }
      });
      self.lookup_tasks.insert(info_hash, task.abort_handle());
   }

   pub(super) fn remove_torrent_registration(&mut self, info_hash: InfoHash) {
      self.torrents.remove(&info_hash);
      if let Some(task) = self.lookup_tasks.remove(&info_hash) {
         task.abort();
      }
      if let Some(task) = self.announce_tasks.remove(&info_hash) {
         task.abort();
      }
   }
}

async fn bootstrap(
   transport: DhtTransport, routers: Vec<String>, id: NodeId, actor_ref: ActorRef<DhtActor>,
) {
   let mut queries = JoinSet::new();
   for router in routers {
      match lookup_host(&router).await {
         Ok(addresses) => {
            for addr in addresses {
               let transport = transport.clone();
               queries.spawn(async move {
                  transport
                     .query(addr, Query::FindNode { id, target: id })
                     .await
               });
            }
         }
         Err(err) => warn!(error = %err, %router, "Failed to resolve DHT bootstrap router"),
      }
   }
   while let Some(result) = queries.join_next().await {
      match result {
         Ok(Ok(response)) => debug!(node_id = %response.id, "DHT bootstrap node responded"),
         Ok(Err(err)) => debug!(error = %err, "DHT bootstrap query failed"),
         Err(err) => debug!(error = %err, "DHT bootstrap task failed"),
      }
   }
   if let Err(err) = actor_ref.tell(RefreshTorrents).await {
      debug!(error = %err, "Failed to refresh torrents after DHT bootstrap");
   }
}
