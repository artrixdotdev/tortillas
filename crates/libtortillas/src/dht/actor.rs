use std::ops::ControlFlow;

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
};
use tracing::{debug, error, warn};

use super::{DhtState, DhtTransport, NodeId, Query, messages::events::IncomingDatagram};
use crate::settings::DhtSettings;

/// Engine-owned actor for DHT routing state and inbound KRPC traffic.
pub(crate) struct DhtActor {
   pub(super) state: DhtState,
   pub(super) transport: DhtTransport,
   bootstrap_task: Option<AbortHandle>,
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

   async fn on_start(args: Self::Args, _actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
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
            args.settings.bootstrap_nodes,
            state.id(),
         ))
         .abort_handle()
      });
      Ok(Self {
         state,
         transport,
         bootstrap_task,
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
      Ok(())
   }
}

async fn bootstrap(transport: DhtTransport, routers: Vec<String>, id: NodeId) {
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
}
