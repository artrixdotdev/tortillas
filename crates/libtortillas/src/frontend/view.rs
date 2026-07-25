use std::{net::SocketAddr, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
   engine::EngineStatus,
   hashes::InfoHash,
   metrics::{
      ByteCount, HasTransferMetrics, TorrentMetrics, TrafficTotals, TransferMetrics, TransferRates,
   },
   peer::Peer,
   torrent::TorrentState,
};

/// Current live engine state maintained by a frontend listener.
///
/// Unlike persistence snapshots, views are presentation-oriented projections
/// updated by the actor hierarchy. A listener always reads the current
/// projection directly, including after a lagged event subscription.
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

/// Current live state of one torrent.
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
   /// Whether the torrent has resolved payload metadata.
   #[must_use]
   pub const fn has_metadata(&self) -> bool {
      self.metrics.progress.total_bytes.is_some()
   }

   /// Whether the torrent has reached its ready lifecycle state.
   #[must_use]
   pub const fn is_ready(&self) -> bool {
      matches!(self.state, TorrentState::Ready)
   }
}

/// Live view of a connected or recently disconnected peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerView {
   /// Network address for the peer, when known.
   pub address: Option<SocketAddr>,
   /// Parsed peer-client family, when known.
   pub client: Option<String>,
   /// Whether this peer is currently connected.
   pub connected: bool,
   pub peer_choking: bool,
   pub peer_interested: bool,
   pub client_choking: bool,
   pub client_interested: bool,
   pub available_pieces: u64,
   pub transfer: TransferMetrics,
}

impl PeerView {
   pub(crate) fn from_peer(peer: &Peer, connected: bool) -> Self {
      Self::from_peer_with_rates(peer, connected, None)
   }

   pub(crate) fn from_peer_with_rates(
      peer: &Peer, connected: bool, rates: Option<TransferRates>,
   ) -> Self {
      Self::from_peer_with_transfer(
         peer,
         connected,
         TransferMetrics {
            totals: TrafficTotals {
               downloaded: ByteCount(u64::try_from(peer.bytes_downloaded()).unwrap_or(u64::MAX)),
               uploaded: ByteCount(u64::try_from(peer.bytes_uploaded()).unwrap_or(u64::MAX)),
            },
            rates,
         },
      )
   }

   pub(crate) fn from_peer_with_transfer(
      peer: &Peer, connected: bool, transfer: TransferMetrics,
   ) -> Self {
      Self {
         address: Some(peer.socket_addr()),
         client: peer.id.map(|id| id.client_name().to_string()),
         connected,
         peer_choking: peer.am_choked(),
         peer_interested: peer.interested(),
         client_choking: peer.choked(),
         client_interested: peer.am_interested(),
         available_pieces: u64::try_from(peer.pieces.count_ones()).unwrap_or(u64::MAX),
         transfer,
      }
   }
}

impl HasTransferMetrics for PeerView {
   fn transfer_metrics(&self) -> &TransferMetrics {
      &self.transfer
   }
}

/// Frontend-safe live tracker identity and latest announce outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerView {
   /// Credential-free tracker endpoint label.
   pub endpoint: String,
   /// Current actor and announce lifecycle.
   pub status: TrackerStatus,
   /// Number of peers returned by the latest successful announce.
   pub peers_returned: Option<u64>,
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
   use super::*;
   use crate::metrics::BytesPerSecond;

   #[test]
   fn peer_view_uses_canonical_byte_units() {
      let peer = PeerView {
         address: None,
         client: None,
         connected: true,
         peer_choking: false,
         peer_interested: true,
         client_choking: false,
         client_interested: true,
         available_pieces: 1,
         transfer: TransferMetrics {
            totals: TrafficTotals::default(),
            rates: Some(TransferRates {
               download: BytesPerSecond(3),
               upload: BytesPerSecond(2),
            }),
         },
      };

      let peers = [peer.clone(), peer];
      let rates = TransferRates::aggregate(&peers).unwrap();

      assert_eq!(rates.download, BytesPerSecond(6));
      assert_eq!(rates.upload, BytesPerSecond(4));
   }
}
