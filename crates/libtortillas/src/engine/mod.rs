mod actor;
pub(crate) use actor::*;
use kameo::actor::ActorRef;

pub struct Engine(ActorRef<EngineActor>);
