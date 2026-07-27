use std::{
   fmt,
   sync::{
      Arc,
      atomic::{AtomicUsize, Ordering},
   },
};

use atomic_time::{AtomicInstant, AtomicOptionInstant};
use tokio::time::Instant;

#[cfg(feature = "live")]
use crate::metrics::{ByteCount, TrackerMetrics, TrafficTotals, TransferMetrics};

/// Tracker statistics.
///
/// All usages of [`AtomicOptionInstant`] or [`AtomicInstant`] are a bit hacky,
/// due to the fact that they only support `std::time::Instant`, not Tokio's
/// instant type. See the getter/setter methods for conversion examples.
#[derive(Clone)]
pub struct TrackerStats {
   announce_attempts: Arc<AtomicUsize>,
   announce_successes: Arc<AtomicUsize>,
   total_peers_received: Arc<AtomicUsize>,
   bytes_sent: Arc<AtomicUsize>,
   bytes_received: Arc<AtomicUsize>,
   last_interaction: Arc<AtomicOptionInstant>,
   session_start: Arc<AtomicInstant>,
}

impl fmt::Debug for TrackerStats {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.debug_struct("TrackerStats")
         .field("announce_attempts", &self.get_announce_attempts())
         .field("announce_successes", &self.get_announce_successes())
         .field("total_peers_received", &self.get_total_peers_received())
         .field("bytes_sent", &self.get_bytes_sent())
         .field("bytes_received", &self.get_bytes_received())
         .field("last_interaction", &self.get_last_interaction())
         .field("session_start", &self.get_session_start())
         .finish()
   }
}

impl fmt::Display for TrackerStats {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      let success_rate = if self.get_announce_attempts() > 0 {
         self.get_announce_successes() as f64 / self.get_announce_attempts() as f64
      } else {
         0.0
      };
      write!(
         f,
         "Stats (success rate: {:.2}%, peers received: {:?})",
         success_rate * 100.0,
         self.get_total_peers_received()
      )
   }
}

impl Default for TrackerStats {
   fn default() -> Self {
      Self {
         announce_attempts: Arc::new(AtomicUsize::new(0)),
         announce_successes: Arc::new(AtomicUsize::new(0)),
         total_peers_received: Arc::new(AtomicUsize::new(0)),
         bytes_sent: Arc::new(AtomicUsize::new(0)),
         bytes_received: Arc::new(AtomicUsize::new(0)),
         last_interaction: Arc::new(AtomicOptionInstant::new(Some(Instant::now().into_std()))),
         session_start: Arc::new(AtomicInstant::new(Instant::now().into_std())),
      }
   }
}

impl TrackerStats {
   pub fn get_announce_attempts(&self) -> usize {
      self.announce_attempts.load(Ordering::Acquire)
   }

   pub fn increment_announce_attempts(&self) {
      self.announce_attempts.fetch_add(1, Ordering::AcqRel);
   }

   pub fn get_announce_successes(&self) -> usize {
      self.announce_successes.load(Ordering::Acquire)
   }

   pub fn increment_announce_successes(&self) {
      self.announce_successes.fetch_add(1, Ordering::AcqRel);
   }

   pub fn get_total_peers_received(&self) -> usize {
      self.total_peers_received.load(Ordering::Acquire)
   }

   pub fn increment_total_peers_received(&self, value: usize) {
      self.total_peers_received.fetch_add(value, Ordering::AcqRel);
   }

   pub fn get_bytes_sent(&self) -> usize {
      self.bytes_sent.load(Ordering::Acquire)
   }

   pub fn increment_bytes_sent(&self, value: usize) {
      self.bytes_sent.fetch_add(value, Ordering::AcqRel);
   }

   pub fn get_bytes_received(&self) -> usize {
      self.bytes_received.load(Ordering::Acquire)
   }

   pub fn increment_bytes_received(&self, value: usize) {
      self.bytes_received.fetch_add(value, Ordering::AcqRel);
   }

   /// Returns all application bytes exchanged with this tracker.
   #[cfg(feature = "live")]
   #[must_use]
   pub(crate) fn traffic_totals(&self) -> TrafficTotals {
      TrafficTotals {
         downloaded: ByteCount(u64::try_from(self.get_bytes_received()).unwrap_or(u64::MAX)),
         uploaded: ByteCount(u64::try_from(self.get_bytes_sent()).unwrap_or(u64::MAX)),
      }
   }

   /// Creates a typed snapshot with shared transfer metrics and tracker-only
   /// counters.
   #[cfg(feature = "live")]
   #[must_use]
   pub(crate) fn metrics(&self) -> TrackerMetrics {
      TrackerMetrics {
         transfer: TransferMetrics {
            totals: self.traffic_totals(),
            samples: Vec::new(),
         },
         announce_attempts: u64::try_from(self.get_announce_attempts()).unwrap_or(u64::MAX),
         announce_successes: u64::try_from(self.get_announce_successes()).unwrap_or(u64::MAX),
         total_peers_received: u64::try_from(self.get_total_peers_received()).unwrap_or(u64::MAX),
         latest_peers_returned: None,
      }
   }

   pub fn get_last_interaction(&self) -> Option<Instant> {
      Some(
         self
            .last_interaction
            .load(Ordering::Acquire)
            .unwrap()
            .into(),
      )
   }

   pub fn set_last_interaction(&self) {
      self
         .last_interaction
         .store(Some(Instant::now().into_std()), Ordering::Release)
   }

   pub fn get_session_start(&self) -> Instant {
      self.session_start.load(Ordering::Acquire).into()
   }

   pub fn set_session_start(&self) {
      self
         .session_start
         .store(Instant::now().into_std(), Ordering::Release)
   }
}

#[cfg(all(test, feature = "live"))]
mod tests {
   use super::*;

   #[test]
   fn tracker_stats_when_wire_bytes_are_recorded_then_exposes_canonical_totals() {
      let stats = TrackerStats::default();

      stats.increment_bytes_sent(12);
      stats.increment_bytes_received(34);

      assert_eq!(
         stats.traffic_totals(),
         TrafficTotals {
            downloaded: ByteCount(34),
            uploaded: ByteCount(12),
         }
      );
      stats.increment_announce_attempts();
      stats.increment_announce_successes();
      stats.increment_total_peers_received(7);
      let mut metrics = stats.metrics();
      metrics.latest_peers_returned = Some(3);
      assert_eq!(metrics.announce_attempts, 1);
      assert_eq!(metrics.announce_successes, 1);
      assert_eq!(metrics.total_peers_received, 7);
      assert_eq!(metrics.latest_peers_returned, Some(3));
   }
}
