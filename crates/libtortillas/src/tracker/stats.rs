use std::{
   fmt,
   sync::{
      Arc,
      atomic::{AtomicUsize, Ordering},
   },
};

use atomic_time::{AtomicInstant, AtomicOptionInstant};
use tokio::time::Instant;

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
