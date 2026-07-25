//! Canonical transfer and verified-content measurements shared by actors and
//! presentation views.

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
   pub fn aggregate<'a, T: HasTransferMetrics + 'a>(
      sources: impl IntoIterator<Item = &'a T>,
   ) -> Option<Self> {
      let mut aggregate = None::<Self>;
      for source in sources {
         let Some(rates) = source.transfer_metrics().rates else {
            continue;
         };
         let current = aggregate.get_or_insert_default();
         current.download = current.download.saturating_add(rates.download);
         current.upload = current.upload.saturating_add(rates.upload);
      }
      aggregate
   }

   #[must_use]
   pub(crate) fn between(
      previous: TrafficTotals, current: TrafficTotals, elapsed: Duration,
   ) -> Self {
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

/// Traffic totals and the latest interval rate sample.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferMetrics {
   pub totals: TrafficTotals,
   /// `None` means no sample has been collected. `Some(default())` is a known
   /// zero-rate sample.
   pub rates: Option<TransferRates>,
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
      let eta = Self::calculate_eta(&progress, traffic.rates);
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

/// Narrow capability used by transfer aggregation algorithms.
pub trait HasTransferMetrics {
   fn transfer_metrics(&self) -> &TransferMetrics;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TransferSample {
   at: Instant,
   totals: TrafficTotals,
}

impl TransferSample {
   #[must_use]
   pub(crate) fn new(at: Instant, totals: TrafficTotals) -> Self {
      Self { at, totals }
   }

   #[must_use]
   pub(crate) fn rates_since(self, previous: Self) -> TransferRates {
      TransferRates::between(
         previous.totals,
         self.totals,
         self.at.saturating_duration_since(previous.at),
      )
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

   #[test]
   fn aggregate_rates_when_no_peers_are_sampled_then_returns_unknown() {
      let peers = [Source(TransferMetrics::default())];
      assert_eq!(TransferRates::aggregate(&peers), None);
   }

   #[test]
   fn transfer_rates_when_sample_is_zero_then_are_known_zero() {
      let rates = TransferSample::new(
         Instant::now() + Duration::from_secs(1),
         TrafficTotals::default(),
      )
      .rates_since(TransferSample::new(
         Instant::now(),
         TrafficTotals::default(),
      ));

      assert_eq!(rates, TransferRates::default());
      assert_eq!(
         TransferRates::aggregate(&[Source(TransferMetrics {
            rates: Some(rates),
            ..TransferMetrics::default()
         })]),
         Some(TransferRates::default())
      );
   }

   #[test]
   fn aggregate_rates_when_some_peers_are_unsampled_then_ignores_them() {
      let peers = [
         Source(TransferMetrics::default()),
         Source(TransferMetrics {
            rates: Some(TransferRates {
               download: BytesPerSecond(10),
               upload: BytesPerSecond(4),
            }),
            ..TransferMetrics::default()
         }),
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
      let metrics = TransferMetrics {
         totals: TrafficTotals {
            downloaded: ByteCount(1_024),
            uploaded: ByteCount(512),
         },
         rates: Some(TransferRates {
            download: BytesPerSecond(300),
            upload: BytesPerSecond(100),
         }),
      };

      let json = serde_json::to_string(&metrics).unwrap();
      assert_eq!(
         serde_json::from_str::<TransferMetrics>(&json).unwrap(),
         metrics
      );
   }
}
