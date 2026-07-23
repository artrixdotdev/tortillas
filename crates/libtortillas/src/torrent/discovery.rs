use crate::tracker::Tracker;

/// Identifies the service that discovered peers in a torrent announce event.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum AnnounceFrom {
   /// Peers returned by a mainline DHT lookup.
   Dht,
   /// Peers returned by the specified tracker.
   Tracker(Tracker),
}

impl AnnounceFrom {
   /// Returns a credential-free label suitable for metrics and tracing.
   pub const fn kind(&self) -> &'static str {
      match self {
         Self::Dht => "dht",
         Self::Tracker(_) => "tracker",
      }
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn announce_source_kind_never_includes_tracker_credentials() {
      let source = AnnounceFrom::Tracker(Tracker::Http(
         "https://tracker.example/secret-passkey/announce?token=secret".into(),
      ));

      assert_eq!(source.kind(), "tracker");
      assert_eq!(AnnounceFrom::Dht.kind(), "dht");
   }
}
