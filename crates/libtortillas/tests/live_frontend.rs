use std::time::Duration;

use futures::StreamExt;
use libtortillas::{
   engine::EngineStatus,
   errors::EngineError,
   frontend::{
      CoreEventKind, EventStreamError, LivePublisher, TorrentEventKind, TrackerEventKind,
      TrackerStatus,
   },
   prelude::{Engine, Settings, TorrentSource, TorrentState},
};
use tokio::time::{sleep, timeout};

const BIG_BUCK_BUNNY: &[u8] = include_bytes!("torrents/big-buck-bunny.torrent");

fn deterministic_engine() -> Engine {
   let mut settings = Settings::default();
   settings.dht.enabled = false;
   Engine::builder()
      .settings(settings)
      .autostart(false)
      .build()
}

#[tokio::test]
async fn engine_listener_receives_live_torrent_lifecycle() {
   let engine = deterministic_engine();
   let mut engine_listener = engine.listener();
   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();

   let added = timeout(Duration::from_secs(2), async {
      loop {
         let event = engine_listener.next().await.unwrap().unwrap();
         if matches!(
            event.kind,
            CoreEventKind::Torrent {
               event: TorrentEventKind::Added,
               ..
            }
         ) {
            break event;
         }
      }
   })
   .await
   .unwrap();
   assert_eq!(added.torrent(), Some(torrent.info_hash()));
   let CoreEventKind::Torrent {
      torrent: added_torrent,
      event: TorrentEventKind::Added,
   } = added.kind
   else {
      unreachable!();
   };
   assert_eq!(added_torrent.info_hash(), torrent.info_hash());
   assert_eq!(added_torrent.live_view(), torrent.live_view());
   assert_eq!(engine_listener.view().torrent_count, 1);

   let mut torrent_listener = torrent.listener();
   torrent.pause().await.unwrap();
   let paused = timeout(Duration::from_secs(2), async {
      loop {
         let event = torrent_listener.recv().await.unwrap();
         if matches!(
            event.kind,
            TorrentEventKind::StateChanged {
               current: TorrentState::Paused,
               ..
            }
         ) {
            break event;
         }
      }
   })
   .await
   .unwrap();
   assert!(matches!(
      paused.kind,
      TorrentEventKind::StateChanged {
         current: TorrentState::Paused,
         ..
      }
   ));
   assert_eq!(torrent_listener.view().unwrap().state, TorrentState::Paused);

   engine.remove_torrent(torrent.info_hash()).await.unwrap();
   assert!(torrent_listener.view().is_none());
   assert_eq!(engine_listener.view().torrent_count, 0);

   engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn generic_live_publisher_implements_async_stream() {
   let publisher = LivePublisher::new(0_u8, 4);
   let mut listener = publisher.listener();

   publisher.update(1, "changed");

   let event = listener.next().await.unwrap().unwrap();
   assert_eq!(event.sequence, 1);
   assert_eq!(event.kind, "changed");
   assert_eq!(listener.view(), 1);
}

#[tokio::test]
async fn live_listener_closes_when_its_publisher_is_dropped() {
   let publisher = LivePublisher::<_, &'static str>::new(0_u8, 4);
   let mut listener = publisher.listener();

   drop(publisher);

   assert_eq!(listener.recv().await, Err(EventStreamError::Closed));
   assert_eq!(listener.view(), 0);
}

#[tokio::test]
async fn closed_live_publisher_rejects_late_updates() {
   let publisher = LivePublisher::new(0_u8, 4);
   let mut listener = publisher.listener();

   assert!(publisher.close(1, "closed"));
   assert!(!publisher.update(2, "late"));

   assert_eq!(listener.recv().await.unwrap().kind, "closed");
   assert_eq!(listener.recv().await, Err(EventStreamError::Closed));
   assert_eq!(listener.view(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_live_updates_are_delivered_in_sequence_order() {
   const UPDATE_COUNT: u64 = 64;
   let publisher = LivePublisher::new(0_u64, UPDATE_COUNT as usize);
   let mut events = publisher.subscribe();
   let updates = (1..=UPDATE_COUNT)
      .map(|view| {
         let publisher = publisher.clone();
         tokio::spawn(async move { publisher.update(view, view) })
      })
      .collect::<Vec<_>>();

   for update in updates {
      update.await.unwrap();
   }
   for sequence in 1..=UPDATE_COUNT {
      assert_eq!(events.recv().await.unwrap().sequence, sequence);
   }
}

#[tokio::test]
async fn tracker_handle_exposes_its_own_live_listener() {
   let engine = deterministic_engine();
   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   let tracker = torrent.trackers().into_iter().next().unwrap();
   let mut listener = tracker.listener();

   assert!(tracker.live_view().status.is_active());
   engine.shutdown().await.unwrap();

   let stopped = timeout(Duration::from_secs(2), async {
      loop {
         let event = listener.recv().await.unwrap();
         if matches!(event.kind, TrackerEventKind::Stopped) {
            break event;
         }
      }
   })
   .await
   .unwrap();
   assert!(stopped.sequence > 0);
   assert_eq!(listener.view().status, TrackerStatus::Stopped);
}

#[tokio::test]
async fn engine_listener_receives_graceful_shutdown() {
   let engine = deterministic_engine();
   let mut listener = engine.listener();

   engine.shutdown().await.unwrap();
   let shutdown = timeout(Duration::from_secs(2), async {
      loop {
         let event = listener.recv().await.unwrap();
         if matches!(event.kind, CoreEventKind::Shutdown(_)) {
            break event;
         }
      }
   })
   .await
   .unwrap();

   let CoreEventKind::Shutdown(view) = shutdown.kind else {
      unreachable!();
   };
   assert_eq!(view.status, EngineStatus::Stopped);
   assert_eq!(listener.view().status, EngineStatus::Stopped);
}

#[tokio::test]
async fn engine_methods_return_typed_unknown_torrent_errors() {
   let engine = deterministic_engine();
   let unknown = libtortillas::hashes::InfoHash::from_bytes([42; 20]);

   let error = engine.torrent(unknown).await.unwrap_err();

   assert!(matches!(error, EngineError::TorrentNotFound(torrent) if torrent == unknown));
   engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn lagging_listener_recovers_from_current_live_view() {
   let engine = deterministic_engine();
   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   let mut listener = torrent.listener();

   for peers in 1..=300 {
      torrent.set_sufficient_peers(peers).await.unwrap();
   }
   timeout(Duration::from_secs(2), async {
      loop {
         if listener
            .view()
            .is_some_and(|view| view.sufficient_peers == 300)
         {
            break;
         }
         sleep(Duration::from_millis(5)).await;
      }
   })
   .await
   .unwrap();

   let error = listener.recv().await.unwrap_err();
   assert!(matches!(error, EventStreamError::Lagged(events) if events > 0));
   assert_eq!(listener.view().unwrap().sufficient_peers, 300);

   engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn live_views_are_serde_compatible() {
   let engine = deterministic_engine();
   let mut listener = engine.listener();
   let _ = engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   timeout(Duration::from_secs(2), async {
      loop {
         let event = listener.recv().await.unwrap();
         if matches!(
            event.kind,
            CoreEventKind::Torrent {
               event: TorrentEventKind::Added,
               ..
            }
         ) {
            break;
         }
      }
   })
   .await
   .unwrap();

   let view = listener.view();
   let encoded_view = serde_json::to_string(&view).unwrap();
   let decoded_view = serde_json::from_str(&encoded_view).unwrap();
   assert_eq!(view, decoded_view);

   engine.shutdown().await.unwrap();
}
