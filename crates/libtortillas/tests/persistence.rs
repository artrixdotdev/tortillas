use libtortillas::{
   engine::Engine,
   errors::{EngineError, TorrentError},
   prelude::{Settings, TorrentSource, TorrentState},
   torrent::TorrentSnapshot,
};

const BIG_BUCK_BUNNY: &[u8] = include_bytes!("torrents/big-buck-bunny.torrent");
const WIRED_CD: &[u8] = include_bytes!("torrents/wired-cd.torrent");

fn deterministic_engine() -> Engine {
   let mut settings = Settings::default();
   settings.dht.enabled = false;
   Engine::builder()
      .settings(settings)
      .autostart(false)
      .build()
}

#[tokio::test]
async fn torrent_snapshot_when_serialized_then_restores_session_state() {
   let engine = deterministic_engine();
   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   torrent.pause().await.unwrap();
   torrent.set_auto_start(false).await.unwrap();
   torrent.set_sufficient_peers(4).await.unwrap();
   let snapshot = torrent.snapshot().await.unwrap();
   let bytes = serde_json::to_vec(&snapshot).unwrap();
   engine.shutdown().await.unwrap();

   let restored_snapshot: TorrentSnapshot = serde_json::from_slice(&bytes).unwrap();
   let restored_engine = deterministic_engine();
   let restored = restored_engine
      .restore_torrent(restored_snapshot)
      .await
      .unwrap();
   let view = restored.live_view().unwrap();

   assert_eq!(restored.info_hash(), torrent.info_hash());
   assert_eq!(view.state, TorrentState::Paused);
   assert!(!view.auto_start);
   assert_eq!(view.sufficient_peers, 4);

   let round_trip = restored.snapshot().await.unwrap();
   assert_eq!(round_trip.info_hash, snapshot.info_hash);
   assert_eq!(round_trip.bitfield, snapshot.bitfield);
   assert_eq!(round_trip.block_map.len(), snapshot.block_map.len());
   restored_engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn active_torrent_snapshot_when_restored_then_resumes_transfer_state() {
   let engine = deterministic_engine();
   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   torrent.set_sufficient_peers(0).await.unwrap();
   torrent.start().await.unwrap();
   assert_eq!(torrent.state().await.unwrap(), TorrentState::Downloading);
   let snapshot = torrent.snapshot().await.unwrap();
   engine.shutdown().await.unwrap();

   let restored_engine = deterministic_engine();
   let restored = restored_engine.restore_torrent(snapshot).await.unwrap();

   assert_eq!(restored.state().await.unwrap(), TorrentState::Downloading);
   restored_engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn torrent_snapshot_when_version_is_unknown_then_returns_typed_error() {
   let engine = deterministic_engine();
   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   let mut snapshot = torrent.snapshot().await.unwrap();
   engine.remove_torrent(torrent.info_hash()).await.unwrap();
   snapshot.version += 1;

   let error = engine.restore_torrent(snapshot).await.unwrap_err();

   assert!(matches!(
      error,
      EngineError::Torrent(TorrentError::InvalidSnapshot { .. })
   ));
   engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn engine_snapshot_when_serialized_then_restores_all_torrents() {
   let engine = deterministic_engine();
   let first = engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   let second = engine
      .add_torrent(TorrentSource::torrent_file_bytes(WIRED_CD))
      .await
      .unwrap();
   let expected_hashes = [first.info_hash(), second.info_hash()];
   let snapshot_bytes = serde_json::to_vec(&engine.snapshot().await.unwrap()).unwrap();
   engine.shutdown().await.unwrap();

   let snapshot = serde_json::from_slice(&snapshot_bytes).unwrap();
   let restored_engine = deterministic_engine();
   let restored = restored_engine.restore(snapshot).await.unwrap();
   let restored_hashes = restored
      .iter()
      .map(libtortillas::torrent::Torrent::info_hash)
      .collect::<Vec<_>>();

   assert_eq!(restored.len(), 2);
   assert!(
      expected_hashes
         .iter()
         .all(|hash| restored_hashes.contains(hash))
   );
   assert_eq!(restored_engine.live_view().torrent_count, 2);
   restored_engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn engine_snapshot_when_version_is_unknown_then_restores_nothing() {
   let source_engine = deterministic_engine();
   let mut snapshot = source_engine.snapshot().await.unwrap();
   source_engine.shutdown().await.unwrap();
   snapshot.version += 1;
   let target_engine = deterministic_engine();

   let error = target_engine.restore(snapshot).await.unwrap_err();

   assert!(matches!(error, EngineError::InvalidSnapshot { .. }));
   assert_eq!(target_engine.live_view().torrent_count, 0);
   target_engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn torrent_snapshot_when_piece_state_is_inconsistent_then_is_rejected_cleanly() {
   let source_engine = deterministic_engine();
   let torrent = source_engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   let mut snapshot = torrent.snapshot().await.unwrap();
   source_engine.shutdown().await.unwrap();
   snapshot.bitfield.pop();
   let target_engine = deterministic_engine();

   let error = target_engine.restore_torrent(snapshot).await.unwrap_err();

   assert!(matches!(
      error,
      EngineError::Torrent(TorrentError::InvalidSnapshot { .. })
   ));
   assert_eq!(target_engine.live_view().torrent_count, 0);
   target_engine.shutdown().await.unwrap();
}
