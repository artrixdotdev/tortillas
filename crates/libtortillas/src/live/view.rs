use std::{net::SocketAddr, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
   engine::EngineStatus,
   hashes::InfoHash,
   metrics::{HasTransferMetrics, PeerMetrics, TorrentMetrics, TrackerMetrics, TransferMetrics},
   peer::Peer,
   torrent::TorrentState,
};

/// Current engine state maintained by a listener.
///
/// Unlike persistence snapshots, views are current projections updated by the
/// actor hierarchy. A listener always reads the projection directly, including
/// after a lagged event subscription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineView {
   pub status: EngineStatus,
   pub torrents: Vec<TorrentView>,
}

impl EngineView {
   #[must_use]
   pub fn torrent_count(&self) -> usize {
      self.torrents.len()
   }
}

/// Current state of one torrent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TorrentView {
   pub info_hash: InfoHash,
   pub name: String,
   pub state: TorrentState,
   pub auto_start: bool,
   pub sufficient_peers: u64,
   pub peer_count: u64,
   pub tracker_count: u64,
   pub output_path: Option<PathBuf>,
   pub metrics: TorrentMetrics,
}

impl TorrentView {
   #[must_use]
   pub const fn has_metadata(&self) -> bool {
      self.metrics.progress.total_bytes.is_some()
   }

   #[must_use]
   pub const fn is_ready(&self) -> bool {
      matches!(self.state, TorrentState::Ready)
   }
}

/// Current state of a connected or recently disconnected peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerView {
   pub address: Option<SocketAddr>,
   pub client: Option<String>,
   pub connected: bool,
   pub metrics: PeerMetrics,
}

impl PeerView {
   pub(crate) fn from_peer(peer: &Peer, connected: bool) -> Self {
      Self::from_peer_with_samples(peer, connected, Vec::new())
   }

   pub(crate) fn from_peer_with_samples(
      peer: &Peer, connected: bool, samples: Vec<crate::metrics::TransferSample>,
   ) -> Self {
      let mut metrics = peer.metrics();
      metrics.transfer.samples = samples;
      Self::from_peer_with_metrics(peer, connected, metrics)
   }

   pub(crate) fn from_peer_with_metrics(
      peer: &Peer, connected: bool, metrics: PeerMetrics,
   ) -> Self {
      Self {
         address: Some(peer.socket_addr()),
         client: peer.id.map(|id| id.client_name().to_string()),
         connected,
         metrics,
      }
   }
}

impl HasTransferMetrics for PeerView {
   fn transfer_metrics(&self) -> &TransferMetrics {
      self.metrics.transfer_metrics()
   }
}

/// Public tracker identity and latest announce outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerView {
   /// Tracker URL with credentials removed.
   pub endpoint: String,
   pub status: TrackerStatus,
   pub metrics: TrackerMetrics,
}

impl HasTransferMetrics for TrackerView {
   fn transfer_metrics(&self) -> &TransferMetrics {
      self.metrics.transfer_metrics()
   }
}

/// Lifecycle and latest announce outcome for a tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackerStatus {
   /// The tracker actor is running but has not completed an announce.
   Pending,
   /// The latest announce completed successfully.
   Healthy,
   /// The latest announce failed while the actor remained available.
   Degraded,
   /// The actor stopped abnormally and supervision may restart it.
   Restarting,
   /// The tracker actor stopped and will emit no more events.
   Stopped,
}

impl TrackerStatus {
   /// Whether the tracker actor can still produce announcements.
   #[must_use]
   pub const fn is_active(self) -> bool {
      !matches!(self, Self::Stopped)
   }
}

#[cfg(test)]
mod tests {
   use std::time::Duration;

   use super::*;
   use crate::metrics::{ByteCount, BytesPerSecond, TrafficTotals, TransferRates, TransferSample};

   #[test]
   fn peer_view_uses_canonical_byte_units() {
      let peer = PeerView {
         address: None,
         client: None,
         connected: true,
         metrics: PeerMetrics {
            peer_interested: true,
            available_pieces: 1,
            transfer: TransferMetrics::from_sample(TransferSample {
               previous_totals: TrafficTotals::default(),
               current_totals: TrafficTotals {
                  downloaded: ByteCount(3),
                  uploaded: ByteCount(2),
               },
               elapsed: Duration::from_secs(1),
            }),
            ..Default::default()
         },
      };

      let peers = [peer.clone(), peer];
      let rates = TransferRates::aggregate(&peers).unwrap();

      assert_eq!(rates.download, BytesPerSecond(6));
      assert_eq!(rates.upload, BytesPerSecond(4));
   }
}
