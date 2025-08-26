mod actor;
pub(crate) use actor::*;
use kameo::actor::ActorRef;

/// Should always be used through the [Engine]
pub struct Torrent(ActorRef<TorrentActor>);
