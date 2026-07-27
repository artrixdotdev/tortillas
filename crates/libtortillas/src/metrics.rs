//! Canonical transfer and verified-content measurements shared by actors and
//! presentation views.
//!
//! Bytes are the canonical unit. [`TransferMetrics`] is shared by peer state,
//! peer statistics, live views, and torrent aggregation, so presentation code
//! never performs a KiB/s conversion. [`TrafficTotals`] measures wire traffic
//! and can include duplicate or rejected data; [`ContentProgress`] measures
//! verified torrent payload and must remain separate.
//!
//! Rates are never stored as mutable metrics. [`TransferMetrics`] retains raw
//! cumulative byte counters and the raw counter samples needed to derive a
//! rate. An absent sample means no interval has been collected. A present
//! sample whose counters did not change is a known zero-rate interval. ETA is
//! likewise derived from verified remaining bytes and the sampled download
//! rate rather than stored independently.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// A quantity of bytes.
#[derive(
   Debug,
   Clone,
   Copy,
   Default,
   PartialEq,
   Eq,
   PartialOrd,
   Ord,
   Serialize,
   Deserialize
)]
#[serde(transparent)]
pub struct ByteCount(pub u64);

impl ByteCount {
   pub const ZERO: Self = Self(0);

   #[must_use]
   pub const fn saturating_add(self, other: Self) -> Self {
      Self(self.0.saturating_add(other.0))
   }

   #[must_use]
   pub const fn saturating_sub(self, other: Self) -> Self {
      Self(self.0.saturating_sub(other.0))
   }
}

/// A byte rate measured over one second.
#[derive(
   Debug,
   Clone,
   Copy,
   Default,
   PartialEq,
   Eq,
   PartialOrd,
   Ord,
   Serialize,
   Deserialize
)]
#[serde(transparent)]
pub struct BytesPerSecond(pub u64);

impl BytesPerSecond {
   pub const ZERO: Self = Self(0);

   #[must_use]
   pub const fn saturating_add(self, other: Self) -> Self {
      Self(self.0.saturating_add(other.0))
   }
}

/// A duration represented as whole seconds.
#[derive(
   Debug,
   Clone,
   Copy,
   Default,
   PartialEq,
   Eq,
   PartialOrd,
   Ord,
   Serialize,
   Deserialize
)]
#[serde(transparent)]
pub struct Seconds(pub u64);

/// Wire traffic totals. These values may include duplicate or rejected data
/// and must not be interpreted as verified torrent content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficTotals {
   pub downloaded: ByteCount,
   pub uploaded: ByteCount,
}

impl TrafficTotals {
   #[must_use]
   pub const fn saturating_add(self, other: Self) -> Self {
      Self {
         downloaded: self.downloaded.saturating_add(other.downloaded),
         uploaded: self.uploaded.saturating_add(other.uploaded),
      }
   }
}

/// Measured download and upload rates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferRates {
   pub download: BytesPerSecond,
   pub upload: BytesPerSecond,
}

impl TransferRates {
   /// Aggregates every available sample while preserving unknown-versus-zero
   /// semantics.
   #[must_use]
   pub fn aggregate<'a, T: HasTransferMetrics + ?Sized + 'a>(
      sources: impl IntoIterator<Item = &'a T>,
   ) -> Option<Self> {
      let mut aggregate = None::<Self>;
      for source in sources {
         let Some(rates) = source.transfer_rates() else {
            continue;
         };
         let current = aggregate.get_or_insert_default();
         current.download = current.download.saturating_add(rates.download);
         current.upload = current.upload.saturating_add(rates.upload);
      }
      aggregate
   }

   #[must_use]
   fn between(previous: TrafficTotals, current: TrafficTotals, elapsed: Duration) -> Self {
      fn rate(previous: ByteCount, current: ByteCount, elapsed: Duration) -> BytesPerSecond {
         let elapsed_nanos = elapsed.as_nanos();
         if elapsed_nanos == 0 || current < previous {
            return BytesPerSecond::ZERO;
         }
         let bytes = u128::from(current.0.saturating_sub(previous.0));
         let per_second = bytes
            .saturating_mul(1_000_000_000)
            .checked_div(elapsed_nanos)
            .unwrap_or(0);
         BytesPerSecond(u64::try_from(per_second).unwrap_or(u64::MAX))
      }

      Self {
         download: rate(previous.downloaded, current.downloaded, elapsed),
         upload: rate(previous.uploaded, current.uploaded, elapsed),
      }
   }
}

/// A raw pair of cumulative counter observations and the time between them.
///
/// The sample deliberately stores totals and elapsed time rather than bytes per
/// second. Consumers derive the rate through [`TransferSample::rates`] or
/// [`HasTransferMetrics::transfer_rates`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferSample {
   pub previous_totals: TrafficTotals,
   pub current_totals: TrafficTotals,
   pub elapsed: Duration,
}

impl TransferSample {
   #[must_use]
   pub fn rates(self) -> TransferRates {
      TransferRates::between(self.previous_totals, self.current_totals, self.elapsed)
   }
}

/// Cumulative traffic totals and the raw intervals available for deriving a
/// current rate.
///
/// A leaf actor normally publishes one sample. Aggregated scopes retain one
/// sample per child because each child can have a different sampling interval.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferMetrics {
   pub totals: TrafficTotals,
   #[serde(default, skip_serializing_if = "Vec::is_empty")]
   pub samples: Vec<TransferSample>,
}

impl TransferMetrics {
   #[must_use]
   pub fn from_sample(sample: TransferSample) -> Self {
      Self {
         totals: sample.current_totals,
         samples: vec![sample],
      }
   }

   /// Derives the latest aggregate rate from raw byte-counter samples.
   ///
   /// `None` means no interval has been sampled. `Some(default())` means at
   /// least one interval was measured and no traffic occurred.
   #[must_use]
   pub fn rates(&self) -> Option<TransferRates> {
      let mut aggregate = None::<TransferRates>;
      for sample in &self.samples {
         let rates = sample.rates();
         let current = aggregate.get_or_insert_default();
         current.download = current.download.saturating_add(rates.download);
         current.upload = current.upload.saturating_add(rates.upload);
      }
      aggregate
   }

   /// Combines raw counters and samples from every child metric scope.
   #[must_use]
   pub fn aggregate<'a, T: HasTransferMetrics + ?Sized + 'a>(
      sources: impl IntoIterator<Item = &'a T>,
   ) -> Self {
      let mut aggregate = Self::default();
      for source in sources {
         let metrics = source.transfer_metrics();
         aggregate.totals = aggregate.totals.saturating_add(metrics.totals);
         aggregate.samples.extend_from_slice(&metrics.samples);
      }
      aggregate
   }
}

/// Peer-specific metrics layered on top of the shared transfer measurements.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerMetrics {
   pub transfer: TransferMetrics,
   pub peer_choking: bool,
   pub peer_interested: bool,
   pub client_choking: bool,
   pub client_interested: bool,
   pub available_pieces: u64,
}

impl HasTransferMetrics for PeerMetrics {
   fn transfer_metrics(&self) -> &TransferMetrics {
      &self.transfer
   }
}

/// Tracker-specific metrics layered on top of the shared transfer
/// measurements.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerMetrics {
   pub transfer: TransferMetrics,
   pub announce_attempts: u64,
   pub announce_successes: u64,
   pub total_peers_received: u64,
   pub latest_peers_returned: Option<u64>,
}

impl HasTransferMetrics for TrackerMetrics {
   fn transfer_metrics(&self) -> &TransferMetrics {
      &self.transfer
   }
}

/// Verified torrent payload progress, deliberately separate from peer wire
/// traffic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentProgress {
   pub total_bytes: Option<ByteCount>,
   pub verified_bytes: ByteCount,
   pub remaining_bytes: Option<ByteCount>,
   pub progress_fraction: Option<f64>,
   pub completed_pieces: u64,
   pub partial_pieces: u64,
   pub total_pieces: u64,
}

/// A coherent metrics publication unit for any application adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TorrentMetrics {
   pub traffic: TransferMetrics,
   pub progress: ContentProgress,
   pub eta: Option<Seconds>,
}

impl TorrentMetrics {
   #[must_use]
   pub fn new(traffic: TransferMetrics, progress: ContentProgress) -> Self {
      let eta = Self::calculate_eta(&progress, traffic.rates());
      Self {
         traffic,
         progress,
         eta,
      }
   }

   #[must_use]
   pub fn calculate_eta(
      progress: &ContentProgress, rates: Option<TransferRates>,
   ) -> Option<Seconds> {
      let remaining = progress.remaining_bytes?;
      let download_rate = rates?.download;
      (download_rate.0 > 0).then(|| Seconds(remaining.0.div_ceil(download_rate.0)))
   }
}

impl HasTransferMetrics for TorrentMetrics {
   fn transfer_metrics(&self) -> &TransferMetrics {
      &self.traffic
   }
}

/// Narrow capability used by transfer aggregation algorithms.
pub trait HasTransferMetrics {
   fn transfer_metrics(&self) -> &TransferMetrics;

   /// Calculates transfer rates from raw cumulative counter samples.
   #[must_use]
   fn transfer_rates(&self) -> Option<TransferRates> {
      self.transfer_metrics().rates()
   }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TimedTransferSample {
   at: Instant,
   totals: TrafficTotals,
}

impl TimedTransferSample {
   #[must_use]
   pub(crate) fn new(at: Instant, totals: TrafficTotals) -> Self {
      Self { at, totals }
   }

   #[must_use]
   pub(crate) fn sample_since(self, previous: Self) -> TransferSample {
      TransferSample {
         previous_totals: previous.totals,
         current_totals: self.totals,
         elapsed: self.at.saturating_duration_since(previous.at),
      }
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[derive(Debug)]
   struct Source(TransferMetrics);

   impl HasTransferMetrics for Source {
      fn transfer_metrics(&self) -> &TransferMetrics {
         &self.0
      }
   }

   fn one_second_sample(downloaded: u64, uploaded: u64) -> TransferSample {
      TransferSample {
         previous_totals: TrafficTotals::default(),
         current_totals: TrafficTotals {
            downloaded: ByteCount(downloaded),
            uploaded: ByteCount(uploaded),
         },
         elapsed: Duration::from_secs(1),
      }
   }

   #[test]
   fn aggregate_rates_when_no_peers_are_sampled_then_returns_unknown() {
      let peers = [Source(TransferMetrics::default())];
      assert_eq!(TransferRates::aggregate(&peers), None);
   }

   #[test]
   fn transfer_rates_when_sample_is_zero_then_are_known_zero() {
      let sample = TimedTransferSample::new(
         Instant::now() + Duration::from_secs(1),
         TrafficTotals::default(),
      )
      .sample_since(TimedTransferSample::new(
         Instant::now(),
         TrafficTotals::default(),
      ));

      assert_eq!(sample.rates(), TransferRates::default());
      assert_eq!(
         TransferRates::aggregate(&[Source(TransferMetrics::from_sample(sample))]),
         Some(TransferRates::default())
      );
   }

   #[test]
   fn aggregate_rates_when_some_peers_are_unsampled_then_ignores_them() {
      let peers = [
         Source(TransferMetrics::default()),
         Source(TransferMetrics::from_sample(one_second_sample(10, 4))),
      ];

      assert_eq!(
         TransferRates::aggregate(&peers),
         Some(TransferRates {
            download: BytesPerSecond(10),
            upload: BytesPerSecond(4),
         })
      );
   }

   #[test]
   fn aggregate_rates_when_metric_scopes_differ_then_uses_shared_transfer_metrics() {
      let peer = PeerMetrics {
         transfer: TransferMetrics::from_sample(one_second_sample(10, 4)),
         ..Default::default()
      };
      let tracker = TrackerMetrics {
         transfer: TransferMetrics::from_sample(one_second_sample(2, 1)),
         ..Default::default()
      };
      let torrent = TorrentMetrics::new(
         TransferMetrics::from_sample(one_second_sample(3, 0)),
         ContentProgress {
            total_bytes: None,
            verified_bytes: ByteCount::ZERO,
            remaining_bytes: None,
            progress_fraction: None,
            completed_pieces: 0,
            partial_pieces: 0,
            total_pieces: 0,
         },
      );
      let scopes: [&dyn HasTransferMetrics; 3] = [&peer, &tracker, &torrent];
      let aggregate = TransferMetrics::aggregate(scopes.iter().copied());

      assert_eq!(
         aggregate.totals,
         TrafficTotals {
            downloaded: ByteCount(15),
            uploaded: ByteCount(5),
         }
      );
      assert_eq!(
         aggregate.rates(),
         Some(TransferRates {
            download: BytesPerSecond(15),
            upload: BytesPerSecond(5),
         })
      );
   }

   #[test]
   fn transfer_rates_when_counters_increase_then_use_bytes_per_second() {
      let rates = TransferRates::between(
         TrafficTotals::default(),
         TrafficTotals {
            downloaded: ByteCount(1_500),
            uploaded: ByteCount(500),
         },
         Duration::from_millis(500),
      );

      assert_eq!(rates.download, BytesPerSecond(3_000));
      assert_eq!(rates.upload, BytesPerSecond(1_000));
   }

   #[test]
   fn transfer_rates_when_counters_reset_then_do_not_underflow() {
      let rates = TransferRates::between(
         TrafficTotals {
            downloaded: ByteCount(10),
            uploaded: ByteCount(10),
         },
         TrafficTotals::default(),
         Duration::from_secs(1),
      );

      assert_eq!(rates, TransferRates::default());
   }

   #[test]
   fn eta_when_remaining_bytes_are_known_then_rounds_up() {
      let progress = ContentProgress {
         total_bytes: Some(ByteCount(13)),
         verified_bytes: ByteCount::ZERO,
         remaining_bytes: Some(ByteCount(13)),
         progress_fraction: Some(0.0),
         completed_pieces: 0,
         partial_pieces: 0,
         total_pieces: 1,
      };

      assert_eq!(
         TorrentMetrics::calculate_eta(
            &progress,
            Some(TransferRates {
               download: BytesPerSecond(6),
               upload: BytesPerSecond::ZERO,
            })
         ),
         Some(Seconds(3))
      );
      assert_eq!(
         TorrentMetrics::calculate_eta(&progress, Some(TransferRates::default())),
         None
      );
   }

   #[test]
   fn serialized_metrics_round_trip_without_unit_conversion() {
      let metrics = TransferMetrics::from_sample(TransferSample {
         previous_totals: TrafficTotals {
            downloaded: ByteCount(724),
            uploaded: ByteCount(412),
         },
         current_totals: TrafficTotals {
            downloaded: ByteCount(1_024),
            uploaded: ByteCount(512),
         },
         elapsed: Duration::from_secs(1),
      });

      let json = serde_json::to_string(&metrics).unwrap();
      assert!(!json.contains("rates"));
      assert!(!json.contains("bytes_per_second"));
      assert_eq!(
         serde_json::from_str::<TransferMetrics>(&json).unwrap(),
         metrics
      );
      assert_eq!(
         metrics.rates(),
         Some(TransferRates {
            download: BytesPerSecond(300),
            upload: BytesPerSecond(100),
         })
      );
   }
}
