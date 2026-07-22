use crate::tracker::Tracker;

/// Identifies the service that discovered peers in a torrent announce event.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum AnnounceFrom {
   /// Peers returned by a mainline DHT lookup.
   Dht,
   /// Peers returned by the specified tracker.
   Tracker(Tracker),
}
