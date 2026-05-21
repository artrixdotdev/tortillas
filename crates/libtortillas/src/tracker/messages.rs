/// A message from an outside source.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub(crate) enum TrackerMessage {
   /// Forces the tracker to make an announce request. By default, announce
   /// requests are made on an interval.
   Announce,
   /// Gets the statistics of the tracker via `tracker.stats()`.
   GetStats,
}
