use std::{
   env,
   path::PathBuf,
   process,
   time::{Duration, SystemTime, UNIX_EPOCH},
};

use libtortillas::{
   engine::{Engine, TorrentSource},
   errors::EngineError,
   hashes::{Hash, HashVec, InfoHash},
   metainfo::{Info, InfoKeys, TorrentFile},
   settings::Settings,
   torrent::TorrentState,
   tracker::Tracker,
};
use tokio::fs;

#[tokio::test(flavor = "multi_thread")]
async fn engine_remove_torrent_drops_it_from_snapshot_and_live_view() {
   let (path, info_hash) = write_http_torrent_fixture().await;
   let engine = Engine::builder()
      .settings(test_settings())
      .autostart(false)
      .sufficient_peers(usize::MAX)
      .build();

   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_path(&path))
      .await
      .unwrap();
   assert_eq!(torrent.info_hash(), info_hash);
   assert_eq!(engine.snapshot().await.unwrap().torrents.len(), 1);
   assert_eq!(engine.view().torrent_count(), 1);

   engine.remove_torrent(info_hash).await.unwrap();
   assert!(engine.snapshot().await.unwrap().torrents.is_empty());
   assert_eq!(engine.view().torrent_count(), 0);
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

   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_path(&path))
      .await
      .unwrap();

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
   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_path(&path))
      .await
      .unwrap();

   torrent.resume().await.unwrap();
   assert_eq!(torrent.state().await.unwrap(), TorrentState::Downloading);

   torrent.pause().await.unwrap();
   assert_eq!(torrent.state().await.unwrap(), TorrentState::Paused);
   torrent.pause().await.unwrap();

   torrent.resume().await.unwrap();
   torrent.stop().await.unwrap();
   assert_eq!(torrent.state().await.unwrap(), TorrentState::Paused);
   torrent.stop().await.unwrap();

   engine.shutdown().await.unwrap();
   let _ = fs::remove_file(path).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn engine_manages_multiple_distinct_torrents_through_the_public_api() {
   let (first_path, first_hash) = write_http_torrent_fixture_named("first-download.bin").await;
   let (second_path, second_hash) = write_http_torrent_fixture_named("second-download.bin").await;
   let output_path = unique_temp_path("documented-downloads");
   let engine = Engine::builder()
      .settings(test_settings())
      .autostart(false)
      .sufficient_peers(usize::MAX)
      .output_path(&output_path)
      .build();

   let first = engine
      .add_torrent(TorrentSource::torrent_file_path(&first_path))
      .await
      .unwrap();
   let second = engine
      .add_torrent(TorrentSource::torrent_file_path(&second_path))
      .await
      .unwrap();

   assert_eq!(first.info_hash(), first_hash);
   assert_eq!(second.info_hash(), second_hash);
   assert_ne!(first.info_hash(), second.info_hash());
   assert_eq!(engine.view().torrent_count(), 2);
   assert_eq!(engine.snapshot().await.unwrap().torrents.len(), 2);
   assert!(
      engine
         .view()
         .torrents
         .iter()
         .all(|view| view.output_path.as_ref() == Some(&output_path))
   );

   engine.shutdown().await.unwrap();
   let _ = fs::remove_file(first_path).await;
   let _ = fs::remove_file(second_path).await;
   let _ = fs::remove_dir_all(output_path).await;
}

fn test_settings() -> Settings {
   let mut settings = Settings::default();
   settings.dht.enabled = false;
   settings.tracker.stop_timeout = Duration::from_millis(20);
   settings.tracker.http_stop_timeout = Duration::from_millis(20);
   settings
}

async fn write_http_torrent_fixture() -> (PathBuf, InfoHash) {
   write_http_torrent_fixture_named("engine-lifecycle.bin").await
}

async fn write_http_torrent_fixture_named(name: &str) -> (PathBuf, InfoHash) {
   let mut pieces = HashVec::new();
   pieces.push(Hash::from_bytes([1; 20]));
   let info = Info {
      name: name.to_string(),
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
      announce: Some(Tracker::Http("http://127.0.0.1:9/announce".to_string())),
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
   let path = unique_temp_path(name).with_extension("torrent");

   fs::write(&path, bytes).await.unwrap();
   (path, info_hash)
}

fn unique_temp_path(name: &str) -> PathBuf {
   env::temp_dir().join(format!(
      "tortillas-engine-lifecycle-{}-{}-{}",
      process::id(),
      name,
      SystemTime::now()
         .duration_since(UNIX_EPOCH)
         .unwrap()
         .as_nanos()
   ))
}
