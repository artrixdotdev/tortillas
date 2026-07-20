use std::ops::ControlFlow;

use kameo::{
   Actor,
   actor::{ActorId, ActorRef, WeakActorRef},
   error::ActorStopReason,
   mailbox::{MailboxReceiver, Signal},
   supervision::SupervisionStrategy,
};
use tracing::{error, warn};

use super::{DhtState, DhtTransport, NodeId, messages::events::IncomingDatagram};
use crate::settings::DhtSettings;

/// Engine-owned actor for DHT routing state and inbound KRPC traffic.
pub(crate) struct DhtActor {
   pub(super) state: DhtState,
   pub(super) transport: DhtTransport,
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
      Ok(Self { state, transport })
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
}
