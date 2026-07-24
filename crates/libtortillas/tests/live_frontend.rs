use std::time::Duration;

use libtortillas::{
   engine::EngineStatus,
   errors::EngineError,
   frontend::{CoreCommand, CoreCommandResult, CoreEventKind, TorrentCommand},
   prelude::{Engine, Settings, TorrentSource, TorrentState},
};
use tokio::time::timeout;

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
   let result = engine
      .send(CoreCommand::AddTorrent {
         source: TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY),
      })
      .await
      .unwrap();
   let CoreCommandResult::TorrentAdded(torrent) = result else {
      panic!("add command should return a torrent handle");
   };

   let added = timeout(Duration::from_secs(2), async {
      loop {
         let event = engine_listener.recv().await.unwrap();
         if matches!(event.kind, CoreEventKind::TorrentAdded(_)) {
            break event;
         }
      }
   })
   .await
   .unwrap();
   assert_eq!(added.torrent(), Some(torrent.info_hash()));
   assert_eq!(engine_listener.view().torrent_count, 1);

   let mut torrent_listener = torrent.listener();
   torrent.send(TorrentCommand::Pause).await.unwrap();
   let paused = timeout(Duration::from_secs(2), async {
      loop {
         let event = torrent_listener.recv().await.unwrap();
         if matches!(
            event.kind,
            CoreEventKind::TorrentStateChanged {
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
      CoreEventKind::TorrentStateChanged {
         current: TorrentState::Paused,
         ..
      }
   ));
   assert_eq!(torrent_listener.view().unwrap().state, TorrentState::Paused);

   let _ = engine
      .send(CoreCommand::RemoveTorrent {
         torrent: torrent.info_hash(),
      })
      .await
      .unwrap();
   assert!(torrent_listener.view().is_none());
   assert_eq!(engine_listener.view().torrent_count, 0);

   engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn engine_listener_receives_graceful_shutdown() {
   let engine = deterministic_engine();
   let mut listener = engine.listener();

   let _ = engine.send(CoreCommand::Shutdown).await.unwrap();
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
async fn engine_commands_return_typed_unknown_torrent_errors() {
   let engine = deterministic_engine();
   let unknown = libtortillas::hashes::InfoHash::from_bytes([42; 20]);

   let error = engine
      .send(CoreCommand::PauseTorrent { torrent: unknown })
      .await
      .unwrap_err();

   assert!(matches!(error, EngineError::TorrentNotFound(torrent) if torrent == unknown));
   engine.shutdown().await.unwrap();
}
