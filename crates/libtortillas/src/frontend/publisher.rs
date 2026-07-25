use std::sync::{Arc, Weak, atomic::AtomicU64};

use super::{
   LivePublisher,
   hub::{EngineScope, FrontendHub},
   registry::ScopeRegistry,
};
use crate::{engine::EngineStatus, settings::FrontendSettings};

#[derive(Debug, Clone)]
enum HubReference {
   Strong(Arc<FrontendHub>),
   Weak(Weak<FrontendHub>),
}

/// Cloneable access to one frontend hub.
#[derive(Debug, Clone)]
pub(crate) struct FrontendPublisher {
   hub: HubReference,
}

impl FrontendPublisher {
   pub(crate) fn new() -> Self {
      Self::with_settings(FrontendSettings::default())
   }

   pub(crate) fn with_settings(settings: FrontendSettings) -> Self {
      Self {
         hub: HubReference::Strong(Arc::new(FrontendHub {
            engine: EngineScope {
               live: LivePublisher::new(EngineStatus::Starting, settings.engine_event_capacity),
            },
            torrents: ScopeRegistry::new(),
            handles: ScopeRegistry::new(),
            settings,
            next_tracker_id: AtomicU64::new(1),
         })),
      }
   }

   pub(crate) fn from_hub(hub: Arc<FrontendHub>) -> Self {
      Self {
         hub: HubReference::Strong(hub),
      }
   }

   pub(crate) fn weak(&self) -> Self {
      Self {
         hub: HubReference::Weak(self.downgrade()),
      }
   }

   pub(crate) fn downgrade(&self) -> Weak<FrontendHub> {
      match &self.hub {
         HubReference::Strong(hub) => Arc::downgrade(hub),
         HubReference::Weak(hub) => hub.clone(),
      }
   }

   pub(crate) fn hub(&self) -> Arc<FrontendHub> {
      match &self.hub {
         HubReference::Strong(hub) => Arc::clone(hub),
         HubReference::Weak(hub) => hub
            .upgrade()
            .expect("frontend hub outlived by its actor hierarchy"),
      }
   }
}

impl Default for FrontendPublisher {
   fn default() -> Self {
      Self::new()
   }
}

#[cfg(test)]
mod tests {
   use std::{
      net::{Ipv4Addr, SocketAddr},
      time::Duration,
   };

   use super::*;
   use crate::{
      frontend::{
         ByteCount, ContentProgress, EventStreamError, PeerEventKind, PeerScope, PeerView,
         TorrentMetrics, TorrentView, TrackerEventKind, TrackerView, TrafficTotals,
         TransferMetrics,
      },
      hashes::InfoHash,
      peer::PeerId,
      torrent::TorrentState,
      tracker::Tracker,
   };

   fn connected_peer_view() -> PeerView {
      PeerView {
         address: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 6881))),
         client: Some("Unknown".to_string()),
         connected: true,
         peer_choking: true,
         peer_interested: false,
         client_choking: true,
         client_interested: false,
         available_pieces: 0,
         transfer: TransferMetrics::default(),
      }
   }

   fn pending_tracker_view() -> TrackerView {
      TrackerView {
         endpoint: "https://tracker.example".to_string(),
         status: super::super::TrackerStatus::Pending,
         peers_returned: None,
      }
   }

   #[tokio::test]
   async fn peer_handle_when_updated_then_only_its_listener_receives_event() {
      let frontend = FrontendPublisher::new();
      let scope = PeerScope {
         torrent: InfoHash::from_bytes([1; 20]),
         peer: PeerId::Unknown([2; 20]),
      };
      let view = connected_peer_view();
      let peer = frontend.register_peer_scope(scope, view.clone());
      let mut listener = peer.listener();
      let mut updated = view;
      updated.transfer.totals = TrafficTotals {
         downloaded: ByteCount(16),
         uploaded: ByteCount::ZERO,
      };

      peer.publish_metrics(updated);

      let event = listener.recv().await.unwrap();
      assert!(matches!(event.kind, PeerEventKind::MetricsChanged(_)));
      assert_eq!(listener.view().transfer.totals.downloaded, ByteCount(16));
   }

   #[tokio::test]
   async fn peer_metrics_do_not_republish_the_torrent_projection() {
      let frontend = FrontendPublisher::new();
      let info_hash = InfoHash::from_bytes([1; 20]);
      let torrent = benchmark_torrent_view(info_hash, "isolated");
      frontend.initialize_torrent_projection(torrent.clone());
      let scope = frontend.ensure_torrent_scope(info_hash);
      let mut torrent_events = scope.live.subscribe();
      let peer = frontend.register_peer_scope(
         PeerScope {
            torrent: info_hash,
            peer: PeerId::Unknown([2; 20]),
         },
         connected_peer_view(),
      );
      let mut peer_view = peer.view();
      peer_view.transfer.rates = Some(Default::default());

      peer.publish_metrics(peer_view);

      assert_eq!(scope.live.view(), Some(torrent));
      assert!(
         tokio::time::timeout(Duration::from_millis(20), torrent_events.recv())
            .await
            .is_err()
      );
   }

   #[tokio::test]
   async fn disconnected_peer_rejects_late_actor_updates() {
      let frontend = FrontendPublisher::new();
      let scope = PeerScope {
         torrent: InfoHash::from_bytes([1; 20]),
         peer: PeerId::Unknown([2; 20]),
      };
      let view = connected_peer_view();
      let peer = frontend.register_peer_scope(scope, view.clone());
      let mut listener = peer.listener();

      peer.disconnected();
      let mut late = view;
      late.transfer.totals.downloaded = ByteCount(32);
      peer.publish_metrics(late);

      assert_eq!(
         listener.recv().await.unwrap().kind,
         PeerEventKind::Disconnected
      );
      assert_eq!(
         listener.recv().await,
         Err(super::super::EventStreamError::Closed)
      );
      assert!(!listener.view().connected);
      assert_eq!(listener.view().transfer.totals.downloaded, ByteCount::ZERO);
   }

   #[test]
   fn live_handles_do_not_keep_their_frontend_hub_alive() {
      let frontend = FrontendPublisher::new();
      let hub = frontend.downgrade();
      let scope = PeerScope {
         torrent: InfoHash::from_bytes([1; 20]),
         peer: PeerId::Unknown([2; 20]),
      };
      let peer = frontend.register_peer_scope(scope, connected_peer_view());

      drop(frontend);

      assert!(hub.upgrade().is_none());
      assert!(peer.view().connected);
   }

   #[test]
   fn publishers_without_listeners_do_not_allocate_event_channels() {
      let frontend = FrontendPublisher::new();
      let peer = frontend.register_peer_scope(
         PeerScope {
            torrent: InfoHash::from_bytes([1; 20]),
            peer: PeerId::Unknown([2; 20]),
         },
         connected_peer_view(),
      );

      assert!(!peer.inner.live.has_event_channel());
      let _listener = peer.listener();
      assert!(peer.inner.live.has_event_channel());
   }

   #[tokio::test]
   async fn tracker_restart_keeps_listener_open_until_final_stop() {
      let frontend = FrontendPublisher::new();
      let source = Tracker::Http("https://tracker.example/announce".to_string());
      let tracker = frontend.register_tracker_scope(
         InfoHash::from_bytes([3; 20]),
         &source,
         pending_tracker_view(),
      );
      let mut listener = tracker.listener();

      tracker.restarting();
      assert_eq!(
         listener.recv().await.unwrap().kind,
         TrackerEventKind::Restarting
      );
      assert_eq!(
         listener.view().status,
         super::super::TrackerStatus::Restarting
      );

      let restarted = frontend.register_tracker_scope(
         InfoHash::from_bytes([3; 20]),
         &source,
         pending_tracker_view(),
      );
      assert_eq!(restarted.id(), tracker.id());
      restarted.announce_succeeded(2);
      assert_eq!(
         listener.recv().await.unwrap().kind,
         TrackerEventKind::AnnounceSucceeded { peers_returned: 2 }
      );

      tracker.stopped();
      tracker.stopped();
      tracker.announce_failed();
      assert_eq!(
         listener.recv().await.unwrap().kind,
         TrackerEventKind::Stopped
      );
      assert_eq!(
         listener.recv().await,
         Err(super::super::EventStreamError::Closed)
      );
   }

   #[tokio::test]
   async fn torrent_removal_closes_every_child_scope_exactly_once() {
      let frontend = FrontendPublisher::new();
      let info_hash = InfoHash::from_bytes([4; 20]);
      frontend.initialize_torrent_projection(benchmark_torrent_view(info_hash, "removed"));
      let peer = frontend.register_peer_scope(
         PeerScope {
            torrent: info_hash,
            peer: PeerId::Unknown([5; 20]),
         },
         connected_peer_view(),
      );
      let source = Tracker::Http("https://tracker.example/announce".to_string());
      let tracker = frontend.register_tracker_scope(info_hash, &source, pending_tracker_view());
      let mut peer_events = peer.subscribe();
      let mut tracker_events = tracker.subscribe();

      frontend.remove_torrent_scope(info_hash);
      frontend.remove_torrent_scope(info_hash);

      assert_eq!(
         peer_events.recv().await.unwrap().kind,
         PeerEventKind::Disconnected
      );
      assert_eq!(peer_events.recv().await, Err(EventStreamError::Closed));
      assert_eq!(
         tracker_events.recv().await.unwrap().kind,
         TrackerEventKind::Stopped
      );
      assert_eq!(tracker_events.recv().await, Err(EventStreamError::Closed));
   }

   fn benchmark_torrent_view(info_hash: InfoHash, name: &str) -> TorrentView {
      TorrentView {
         info_hash,
         name: name.to_string(),
         state: TorrentState::Downloading,
         auto_start: true,
         sufficient_peers: 1,
         peer_count: 0,
         tracker_count: 0,
         output_path: None,
         metrics: TorrentMetrics::new(
            TransferMetrics::default(),
            ContentProgress {
               total_bytes: Some(ByteCount(1_000)),
               verified_bytes: ByteCount::ZERO,
               remaining_bytes: Some(ByteCount(1_000)),
               progress_fraction: Some(0.0),
               completed_pieces: 0,
               partial_pieces: 0,
               total_pieces: 1,
            },
         ),
      }
   }

   #[test]
   #[ignore = "performance benchmark; run explicitly with --ignored --nocapture"]
   fn large_scope_tree_benchmark() {
      use std::time::Instant;

      let frontend = FrontendPublisher::new();
      let started = Instant::now();
      for torrent_index in 0_u16..100 {
         let bytes = torrent_index.to_be_bytes();
         let mut hash = [0_u8; 20];
         hash[..2].copy_from_slice(&bytes);
         frontend.initialize_torrent_projection(benchmark_torrent_view(
            InfoHash::from_bytes(hash),
            &format!("torrent-{torrent_index}"),
         ));
         frontend
            .ensure_torrent_scope(InfoHash::from_bytes(hash))
            .register();
         for peer_index in 0_u8..10 {
            frontend.register_peer_scope(
               PeerScope {
                  torrent: InfoHash::from_bytes(hash),
                  peer: PeerId::Unknown([peer_index; 20]),
               },
               connected_peer_view(),
            );
         }
      }
      let construction = started.elapsed();

      let started = Instant::now();
      for _ in 0..10 {
         for torrent_index in 0_u16..100 {
            let bytes = torrent_index.to_be_bytes();
            let mut hash = [0_u8; 20];
            hash[..2].copy_from_slice(&bytes);
            for peer in frontend.peer_handles(InfoHash::from_bytes(hash)) {
               peer.publish_metrics(peer.view());
            }
         }
      }
      let updates = started.elapsed();

      let started = Instant::now();
      let view = frontend.view();
      let view_construction = started.elapsed();
      assert_eq!(view.torrent_count(), 100);
      assert!(
         view
            .torrents
            .windows(2)
            .all(|pair| { pair[0].info_hash.as_bytes() <= pair[1].info_hash.as_bytes() })
      );

      let removal_hash = InfoHash::from_bytes([255; 20]);
      frontend.initialize_torrent_projection(benchmark_torrent_view(removal_hash, "removal"));
      let removal_peers = (0_u16..1_000)
         .map(|peer_index| {
            let bytes = peer_index.to_be_bytes();
            let mut id = [0_u8; 20];
            id[..2].copy_from_slice(&bytes);
            frontend.register_peer_scope(
               PeerScope {
                  torrent: removal_hash,
                  peer: PeerId::Unknown(id),
               },
               connected_peer_view(),
            )
         })
         .collect::<Vec<_>>();
      let zero_listener_slots = removal_peers
         .iter()
         .map(|peer| peer.inner.live.allocated_event_slots())
         .sum::<usize>();
      let zero_listener_memory_lower_bound = removal_peers
         .iter()
         .map(|peer| peer.inner.live.allocation_lower_bound_bytes())
         .sum::<usize>();
      let started = Instant::now();
      frontend.remove_torrent_scope(removal_hash);
      let removal = started.elapsed();

      let burst = LivePublisher::new(0_u64, 8);
      let mut lagging = burst.subscribe();
      let started = Instant::now();
      for value in 1..=10_000 {
         burst.update(value, value);
      }
      let burst_publication = started.elapsed();
      let lagged_by = match futures::executor::block_on(lagging.recv()) {
         Err(EventStreamError::Lagged(skipped)) => skipped,
         result => panic!("expected a lagged subscription, got {result:?}"),
      };

      assert_eq!(zero_listener_slots, 0);
      assert!(lagged_by > 0);
      eprintln!(
         "100 torrents / 1,000 peers: {construction:?}; 10,000 peer updates: \
          {updates:?}; engine view: {view_construction:?}; remove 1,000 children: \
          {removal:?}; zero-listener allocated event slots: {zero_listener_slots}; \
          zero-listener publisher memory lower bound: {zero_listener_memory_lower_bound} bytes; \
          10,000-event burst: {burst_publication:?}; lagged by: {lagged_by}"
      );
   }
}
