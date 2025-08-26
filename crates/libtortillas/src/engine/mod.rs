mod actor;
pub(crate) use actor::*;
use kameo::{Actor, actor::ActorRef};

pub struct Engine(ActorRef<EngineActor>);

impl Engine {
   pub fn new(addrs: Option<EngineActorArgs>) -> Self {
      let addrs = addrs.unwrap_or_default();
      let actor = EngineActor::spawn(addrs);
      Engine(actor)
   }
   
   /// Just a helper function so we don't have to write `&self.0` all the time.
   fn actor(&self) -> &ActorRef<EngineActor> {
      &self.0
   }
}
