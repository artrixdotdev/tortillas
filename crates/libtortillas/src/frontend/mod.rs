//! Transport-agnostic live application API.
//!
//! Terminal interfaces, HTTP or WebSocket servers, websites, and desktop
//! applications all consume this same API. Rendering, transport, input, and
//! application routing policy remain outside `libtortillas`.
//!
//! # Public model
//!
//! The module is organized by the way an application reads it:
//!
//! - [`EngineView`], [`TorrentView`], [`PeerView`], and [`TrackerView`] are
//!   current presentation state.
//! - Shared measurements live in [`crate::metrics`] and are re-exported here.
//! - Event enums describe discrete changes.
//! - [`EventSubscription`] is events only; [`EventListener`] pairs events with
//!   a coherent current view.
//! - [`PeerHandle`] and [`TrackerHandle`] provide scoped identity and access.
//! - The private hub owns the complete live projection tree and coordinates
//!   publication.
//!
//! [`crate::engine::Engine`] and [`crate::torrent::Torrent`] remain the sole
//! public command API. There is no parallel command enum or generic `send`
//! method for application operations.
//!
//! # Listening to an engine
//!
//! Create a listener before starting operations when the application must not
//! miss their events. Use [`EventListener::view`] for initial rendering and
//! lag recovery, and [`EventListener::recv`] for future changes.
//!
//! ```no_run
//! use libtortillas::prelude::{Engine, EngineEventKind, EventStreamError};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let engine = Engine::default();
//! let mut listener = engine.listener();
//! let initial_view = listener.view();
//!
//! loop {
//!    match listener.recv().await {
//!       Ok(event) => {
//!          let current_view = listener.view();
//!          // Render, serialize, or forward `current_view` and `event`.
//!          let _ = current_view;
//!          if matches!(event.kind, EngineEventKind::Shutdown(_)) {
//!             break;
//!          }
//!       }
//!       Err(EventStreamError::Lagged(_)) => {
//!          // Discard adapter-local assumptions and redraw from current state.
//!          let current_view = listener.view();
//!          let _ = current_view;
//!       }
//!       Err(EventStreamError::Closed) => break,
//!    }
//! }
//! # let _ = initial_view;
//! # Ok(())
//! # }
//! ```
//!
//! Every [`crate::torrent::Torrent`] has its own `listener()` and
//! `subscribe()` methods. Peers and trackers returned by `Torrent::peers()` and
//! `Torrent::trackers()` follow the same pattern. A scoped listener receives
//! only that scope's events; it does not filter the engine stream.
//!
//! Engine listeners receive [`EngineEventKind::Torrent`], whose nested event
//! uses the same [`TorrentEventKind`] vocabulary as the torrent listener.
//! Peer and tracker lifecycle events carry public handles, allowing an
//! application to descend into detailed streams only when needed.
//!
//! Use `subscribe()` when only discrete events are needed. Use `listener()`
//! when initial rendering or recovery requires a current view as well.
//!
//! # Ownership and source of truth
//!
//! ```text
//! EngineActor ── owns operational engine state
//! Hub
//! ├── engine lifecycle and event publisher
//! └── keyed torrent scopes
//!     └── torrent view and event publisher
//!         ├── keyed peer scopes
//!         └── keyed tracker scopes
//!
//! EngineView = engine lifecycle + views derived from current torrent scopes
//! ```
//!
//! The engine never caches a second `Vec<TorrentView>`. [`EngineListener`]
//! derives [`EngineView`] on read from current torrent scopes and sorts them by
//! info hash. Peer-only changes therefore touch one peer scope and cannot make
//! a copied engine projection drift from the torrent projection.
//!
//! The private scope registry is a policy wrapper around `DashMap`, not a
//! replacement concurrent map. It prevents shard guards from escaping by
//! returning cloned `Arc` values or owned vectors. Peer and tracker registries
//! are nested under their torrent, making lookup and removal proportional to
//! that torrent's children.
//!
//! # Architectural invariants
//!
//! These rules define the live API's source of truth:
//!
//! 1. Actors own operational domain state.
//! 2. A live scope owns only its frontend projection.
//! 3. Parent views are derived from child scopes; they do not maintain manually
//!    synchronized child-view copies.
//! 4. Every scope has one view-and-event publication entry point.
//! 5. Peer and tracker events do not implicitly rebuild torrent or engine
//!    state.
//! 6. A scope closes exactly once, only when it cannot restart.
//! 7. Snapshot schema validation runs once at the authoritative engine restore
//!    boundary.
//! 8. Actor and hub back-references are weak; the ownership graph contains no
//!    strong cycle.
//! 9. Synchronous lock order is registry shard, scope publication/state, then
//!    event sender.
//! 10. Actor communication, filesystem work, arbitrary callbacks, and `.await`
//!     never occur while a synchronous lock is held.
//!
//! # Event delivery and lifecycle
//!
//! Channels are allocated lazily on first subscription. Defaults retain 256
//! engine or torrent events and 64 peer or tracker events; all capacities are
//! configurable with [`crate::settings::FrontendSettings`]. A slow consumer
//! receives [`EventStreamError::Lagged`] instead of causing unbounded memory
//! growth. Sequence numbers increase monotonically within each scope.
//!
//! [`LivePublisher`] mutation names describe their full effect:
//! [`LivePublisher::replace_view`] changes only the projection,
//! [`LivePublisher::replace_view_and_emit`] performs a coherent view/event
//! transition, [`LivePublisher::emit_without_view_change`] emits a discrete
//! event, and [`LivePublisher::close_with_terminal_event`] performs the one
//! irreversible close transition.
//!
//! Supervised torrent and tracker actors publish a restarting state after
//! abnormal termination and keep their streams open. Final ownership teardown
//! publishes the terminal state once, closes the scope tree, and rejects late
//! actor updates.
//!
//! # Locking and publication
//!
//! Registry methods release their `DashMap` shard guard before acquiring a
//! scope lock. Scope construction occurs before shard entry acquisition, so
//! callbacks never execute under a registry lock. A scope publication lock
//! serializes its view transition, scoped event, and corresponding root event.
//! [`LivePublisher`] then acquires its state lock before its optional sender
//! lock. No path acquires a registry guard while holding a child scope lock,
//! and no synchronous lock crosses an `.await`.
//!
//! # Views and persistence
//!
//! Views are presentation contracts suitable for rendering, API responses,
//! and transport serialization. [`crate::engine::EngineSnapshot`] and
//! [`crate::torrent::TorrentSnapshot`] are durable persistence contracts.
//! Applications must not poll snapshots to refresh a frontend. See
//! [`crate::torrent`] for restore validation and storage reconciliation rules.
//!
//! Application-specific action routing can use an adapter-owned Tokio channel
//! whose consumer invokes methods on `Engine` and `Torrent`. That keeps UI or
//! server commands outside the library without duplicating its public API.

mod event;
mod handle;
mod hub;
mod stream;
mod view;

pub use event::{
   EngineEvent, EngineEventKind, FrontendHealth, FrontendHealthLevel, PeerEvent, PeerEventKind,
   SequencedEvent, TorrentEvent, TorrentEventKind, TrackerEvent, TrackerEventKind,
};
pub(crate) use handle::PeerIdentity;
pub use handle::{PeerHandle, PeerListener, TrackerHandle, TrackerId, TrackerListener};
pub(crate) use hub::{Hub, HubInner};
pub use stream::{
   EngineListener, EventListener, EventStreamError, EventSubscription, LivePublisher,
   TorrentListener,
};
pub use view::{EngineView, PeerView, TorrentView, TrackerStatus, TrackerView};

pub use crate::metrics::{
   ByteCount, BytesPerSecond, ContentProgress, HasTransferMetrics, PeerMetrics, Seconds,
   TorrentMetrics, TrackerMetrics, TrafficTotals, TransferMetrics, TransferRates,
};

#[cfg(test)]
mod tests {
   use std::{
      net::{Ipv4Addr, SocketAddr},
      time::Duration,
   };

   use super::*;
   use crate::{hashes::InfoHash, peer::PeerId, torrent::TorrentState, tracker::Tracker};

   fn connected_peer_view() -> PeerView {
      PeerView {
         address: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 6881))),
         client: Some("Unknown".to_string()),
         connected: true,
         metrics: PeerMetrics {
            peer_choking: true,
            client_choking: true,
            ..Default::default()
         },
      }
   }

   fn pending_tracker_view() -> TrackerView {
      TrackerView {
         endpoint: "https://tracker.example".to_string(),
         status: TrackerStatus::Pending,
         metrics: TrackerMetrics::default(),
      }
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

   #[tokio::test]
   async fn peer_metrics_do_not_republish_the_torrent_projection() {
      let frontend = Hub::new();
      let info_hash = InfoHash::from_bytes([1; 20]);
      let torrent = benchmark_torrent_view(info_hash, "isolated");
      frontend.initialize_torrent_projection(torrent.clone());
      let scope = frontend.ensure_torrent_scope(info_hash);
      let mut torrent_events = scope.live.subscribe();
      let peer = frontend.register_peer_scope(
         PeerIdentity {
            torrent: info_hash,
            peer: PeerId::Unknown([2; 20]),
         },
         connected_peer_view(),
      );
      let mut peer_view = peer.view();
      peer_view.metrics.transfer.rates = Some(Default::default());

      peer.publish_metrics(peer_view);

      assert_eq!(scope.live.view(), Some(torrent));
      assert!(
         tokio::time::timeout(Duration::from_millis(20), torrent_events.recv())
            .await
            .is_err()
      );
   }

   #[tokio::test]
   async fn tracker_restart_keeps_listener_open_until_final_stop() {
      let frontend = Hub::new();
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
      assert_eq!(listener.view().status, TrackerStatus::Restarting);

      let restarted = frontend.register_tracker_scope(
         InfoHash::from_bytes([3; 20]),
         &source,
         pending_tracker_view(),
      );
      assert_eq!(restarted.id(), tracker.id());
      restarted.announce_succeeded(TrackerMetrics {
         latest_peers_returned: Some(2),
         ..Default::default()
      });
      assert_eq!(
         listener.recv().await.unwrap().kind,
         TrackerEventKind::AnnounceSucceeded { peers_returned: 2 }
      );

      tracker.stopped();
      tracker.stopped();
      tracker.announce_failed(TrackerMetrics::default());
      assert_eq!(
         listener.recv().await.unwrap().kind,
         TrackerEventKind::Stopped
      );
      assert_eq!(listener.recv().await, Err(EventStreamError::Closed));
   }

   #[test]
   #[ignore = "performance benchmark; run explicitly with --ignored --nocapture"]
   fn large_scope_tree_benchmark() {
      use std::time::Instant;

      let frontend = Hub::new();
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
            .mark_registered_for_benchmark();
         for peer_index in 0_u8..10 {
            frontend.register_peer_scope(
               PeerIdentity {
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
               PeerIdentity {
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
         burst.replace_view_and_emit(value, value);
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
