use std::{
   env,
   path::PathBuf,
   process,
   time::{Duration, SystemTime, UNIX_EPOCH},
};

use libtortillas::{
   engine::Engine,
   errors::EngineError,
   hashes::{Hash, HashVec, InfoHash},
   metainfo::{Info, InfoKeys, TorrentFile},
   settings::Settings,
   torrent::TorrentState,
   tracker::Tracker,
};
use tokio::fs;

#[tokio::test(flavor = "multi_thread")]
async fn engine_remove_torrent_drops_it_from_exports() {
   let (path, info_hash) = write_http_torrent_fixture().await;
   let engine = Engine::builder()
      .settings(test_settings())
      .autostart(false)
      .sufficient_peers(usize::MAX)
      .build();

   let torrent = engine.add_torrent(path.to_string_lossy()).await.unwrap();
   assert_eq!(torrent.info_hash(), info_hash);
   assert_eq!(engine.export().await.unwrap().torrents.len(), 1);

   engine.remove_torrent(info_hash).await.unwrap();
   assert!(engine.export().await.unwrap().torrents.is_empty());
   assert!(torrent.state().await.is_err());

   let err = engine.remove_torrent(info_hash).await.unwrap_err();
   assert!(matches!(
      err,
      EngineError::TorrentNotFound(missing_hash) if missing_hash == info_hash
   ));

   engine.shutdown().await.unwrap();
   let _ = fs::remove_file(path).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn engine_shutdown_stops_managed_torrents() {
   let (path, _) = write_http_torrent_fixture().await;
   let engine = Engine::builder()
      .settings(test_settings())
      .autostart(false)
      .sufficient_peers(usize::MAX)
      .build();

   let torrent = engine.add_torrent(path.to_string_lossy()).await.unwrap();

   engine.shutdown().await.unwrap();
   assert!(torrent.state().await.is_err());

   let _ = fs::remove_file(path).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn torrent_controls_complete_their_state_transitions() {
   let (path, _) = write_http_torrent_fixture().await;
   let engine = Engine::builder()
      .settings(test_settings())
      .autostart(false)
      .sufficient_peers(usize::MAX)
      .build();
   let torrent = engine.add_torrent(path.to_string_lossy()).await.unwrap();

   torrent.resume().await.unwrap();
   assert_eq!(torrent.state().await.unwrap(), TorrentState::Downloading);

   torrent.pause().await.unwrap();
   assert_eq!(torrent.state().await.unwrap(), TorrentState::Inactive);
   torrent.pause().await.unwrap();

   torrent.resume().await.unwrap();
   torrent.stop().await.unwrap();
   assert_eq!(torrent.state().await.unwrap(), TorrentState::Inactive);
   torrent.stop().await.unwrap();

   engine.shutdown().await.unwrap();
   let _ = fs::remove_file(path).await;
}

fn test_settings() -> Settings {
   let mut settings = Settings::default();
   settings.tracker.stop_timeout = Duration::from_millis(20);
   settings.tracker.http_stop_timeout = Duration::from_millis(20);
   settings
}

async fn write_http_torrent_fixture() -> (PathBuf, InfoHash) {
   let mut pieces = HashVec::new();
   pieces.push(Hash::from_bytes([1; 20]));
   let info = Info {
      name: "engine-lifecycle.bin".to_string(),
      piece_length: 16,
      pieces,
      file: InfoKeys::Single {
         length: 16,
         md5sum: None,
      },
      is_private: None,
      publisher: None,
      publisher_url: None,
      source: None,
   };
   let torrent = TorrentFile {
      announce: Tracker::Http("http://127.0.0.1:9/announce".to_string()),
      announce_list: None,
      comment: None,
      created_by: Some("libtortillas-test".to_string()),
      creation_date: None,
      encoding: None,
      info,
      url_list: None,
   };
   let info_hash = torrent.info.hash().unwrap();
   let bytes = serde_bencode::to_bytes(&torrent).unwrap();
   let path = env::temp_dir().join(format!(
      "tortillas-engine-lifecycle-{}-{}.torrent",
      process::id(),
      SystemTime::now()
         .duration_since(UNIX_EPOCH)
         .unwrap()
         .as_nanos()
   ));

   fs::write(&path, bytes).await.unwrap();
   (path, info_hash)
}
