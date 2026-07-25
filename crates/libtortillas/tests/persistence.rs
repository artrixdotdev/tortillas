use async_trait::async_trait;
use bytes::Bytes;
use libtortillas::{
   engine::Engine,
   errors::{EngineError, SnapshotUnsupportedReason, TorrentError},
   metainfo::Info,
   pieces::PieceManager,
   prelude::{Settings, TorrentSource, TorrentState},
   torrent::{PieceStorageStrategy, RestoreVerification, TorrentSnapshot},
};

const BIG_BUCK_BUNNY: &[u8] = include_bytes!("torrents/big-buck-bunny.torrent");
const WIRED_CD: &[u8] = include_bytes!("torrents/wired-cd.torrent");
const SNAPSHOT_V1: &str = include_str!("fixtures/torrent-snapshot-v1.json");
const SNAPSHOT_V2: &str = include_str!("fixtures/torrent-snapshot-v2.json");
const ENGINE_SNAPSHOT_V1: &str = include_str!("fixtures/engine-snapshot-v1.json");
const ENGINE_SNAPSHOT_V2: &str = include_str!("fixtures/engine-snapshot-v2.json");

fn deterministic_engine() -> Engine {
   let mut settings = Settings::default();
   settings.dht.enabled = false;
   Engine::builder()
      .settings(settings)
      .autostart(false)
      .build()
}

#[derive(Default)]
struct CustomPieceManager {
   info: Option<Info>,
}

#[async_trait]
impl PieceManager for CustomPieceManager {
   fn info(&self) -> Option<&Info> {
      self.info.as_ref()
   }

   async fn pre_start(&mut self, info: Info) -> anyhow::Result<()> {
      self.info = Some(info);
      Ok(())
   }

   async fn recv(&self, _index: usize, _data: Bytes) -> anyhow::Result<()> {
      Ok(())
   }
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
   let view = restored.view().unwrap();

   assert_eq!(restored.info_hash(), torrent.info_hash());
   assert_eq!(view.state, TorrentState::Paused);
   assert!(!view.auto_start);
   assert_eq!(view.sufficient_peers, 4);

   let round_trip = restored.snapshot().await.unwrap();
   assert_eq!(round_trip.info_hash, snapshot.info_hash);
   assert_eq!(round_trip.output_path, snapshot.output_path);
   assert_eq!(round_trip.piece_storage, snapshot.piece_storage);
   assert_eq!(round_trip.bitfield, snapshot.bitfield);
   assert_eq!(round_trip.block_map.len(), snapshot.block_map.len());
   restored_engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn version_one_torrent_metainfo_with_null_info_dict_restores_metadata() {
   let source = deterministic_engine();
   let torrent = source
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   let snapshot = torrent.snapshot().await.unwrap();
   let mut wire = serde_json::to_value(snapshot).unwrap();
   wire["version"] = serde_json::json!(1);
   wire["info_dict"] = serde_json::Value::Null;
   wire.as_object_mut().unwrap().remove("resolved_magnet_info");
   wire["block_map"] = serde_json::json!({});
   source.shutdown().await.unwrap();

   let migrated: TorrentSnapshot = serde_json::from_value(wire).unwrap();
   let target = deterministic_engine();
   let restored = target.restore_torrent(migrated).await.unwrap();

   assert!(restored.view().unwrap().has_metadata());
   target.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_completed_payload_never_restores_as_seeding() {
   let source = deterministic_engine();
   let torrent = source
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   let mut snapshot = torrent.snapshot().await.unwrap();
   snapshot.state = libtortillas::torrent::TorrentState::Seeding;
   snapshot.bitfield.fill(true);
   let output_path = std::env::temp_dir().join(format!(
      "libtortillas-missing-restore-{}",
      std::process::id()
   ));
   tokio::fs::create_dir_all(&output_path).await.unwrap();
   snapshot.output_path = Some(output_path.clone());
   source.shutdown().await.unwrap();

   let target = deterministic_engine();
   let restored = target.restore_torrent(snapshot).await.unwrap();

   assert_ne!(
      restored.state().await.unwrap(),
      libtortillas::torrent::TorrentState::Seeding
   );
   assert_eq!(
      restored
         .snapshot()
         .await
         .unwrap()
         .bitfield
         .iter()
         .filter(|complete| **complete)
         .count(),
      0
   );
   target.shutdown().await.unwrap();
   tokio::fs::remove_dir_all(output_path).await.unwrap();
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
   let expected_output_path = snapshot.output_path.clone();
   let expected_piece_storage = snapshot.piece_storage.clone();
   engine.shutdown().await.unwrap();

   let restored_engine = deterministic_engine();
   let restored = restored_engine.restore_torrent(snapshot).await.unwrap();

   assert_eq!(restored.state().await.unwrap(), TorrentState::Downloading);
   let round_trip = restored.snapshot().await.unwrap();
   assert_eq!(round_trip.output_path, expected_output_path);
   assert_eq!(round_trip.piece_storage, expected_piece_storage);
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
   let engine_snapshot = engine.snapshot().await.unwrap();
   assert!(
      engine_snapshot
         .torrents
         .windows(2)
         .all(|pair| { pair[0].info_hash.as_bytes() <= pair[1].info_hash.as_bytes() })
   );
   let snapshot_bytes = serde_json::to_vec(&engine_snapshot).unwrap();
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
   assert_eq!(restored_engine.view().torrent_count(), 2);
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
   assert_eq!(target_engine.view().torrent_count(), 0);
   target_engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn engine_restore_checks_authoritative_actor_state_before_mutating() {
   let source_engine = deterministic_engine();
   source_engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   let snapshot = source_engine.snapshot().await.unwrap();
   source_engine.shutdown().await.unwrap();

   let target_engine = deterministic_engine();
   let existing = target_engine
      .add_torrent(TorrentSource::torrent_file_bytes(WIRED_CD))
      .await
      .unwrap();

   let error = target_engine.restore(snapshot).await.unwrap_err();

   assert!(matches!(error, EngineError::InvalidSnapshot { .. }));
   assert_eq!(target_engine.view().torrent_count(), 1);
   assert!(target_engine.torrent(existing.info_hash()).await.is_ok());
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
   assert_eq!(target_engine.view().torrent_count(), 0);
   target_engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn torrent_snapshot_without_output_path_returns_typed_error() {
   let source = deterministic_engine();
   let torrent = source
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   let mut snapshot = torrent.snapshot().await.unwrap();
   snapshot.output_path = None;
   source.shutdown().await.unwrap();

   let target = deterministic_engine();
   let error = target.restore_torrent(snapshot).await.unwrap_err();

   assert!(matches!(
      error,
      EngineError::Torrent(TorrentError::InvalidSnapshot { .. })
   ));
   target.shutdown().await.unwrap();
}

#[tokio::test]
async fn duplicate_add_preserves_typed_domain_error() {
   let engine = deterministic_engine();
   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();

   let error = engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap_err();

   assert!(matches!(
      error,
      EngineError::TorrentAlreadyExists(info_hash) if info_hash == torrent.info_hash()
   ));
   engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn custom_piece_manager_snapshot_returns_typed_unsupported_error() {
   let engine = deterministic_engine();
   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   let storage = std::env::temp_dir().join(format!(
      "libtortillas-custom-manager-{}",
      std::process::id()
   ));
   torrent
      .set_piece_storage(PieceStorageStrategy::Disk(storage.clone()))
      .await
      .unwrap();
   torrent
      .set_piece_manager(CustomPieceManager::default())
      .await
      .unwrap();

   let error = torrent.snapshot().await.unwrap_err();

   assert!(matches!(
      error,
      TorrentError::SnapshotUnsupported {
         reason: SnapshotUnsupportedReason::CustomPieceManager
      }
   ));
   engine.shutdown().await.unwrap();
   let _ = tokio::fs::remove_dir_all(storage).await;
}

#[tokio::test]
async fn invalid_output_folder_returns_typed_filesystem_error() {
   let engine = deterministic_engine();
   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   let fixture =
      std::env::temp_dir().join(format!("libtortillas-output-file-{}", std::process::id()));
   tokio::fs::write(&fixture, b"not a directory")
      .await
      .unwrap();

   let error = torrent
      .set_output_folder(fixture.join("child"))
      .await
      .unwrap_err();

   assert!(matches!(error, TorrentError::FileIoError { .. }));
   tokio::fs::remove_file(fixture).await.unwrap();
   engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn piece_storage_change_after_restored_data_returns_invalid_operation() {
   let source = deterministic_engine();
   let torrent = source
      .add_torrent(TorrentSource::torrent_file_bytes(BIG_BUCK_BUNNY))
      .await
      .unwrap();
   let mut snapshot = torrent.snapshot().await.unwrap();
   snapshot.bitfield[0] = true;
   source.shutdown().await.unwrap();

   let target = deterministic_engine();
   let restored = target
      .restore_torrent_with_verification(snapshot, RestoreVerification::TrustSnapshot)
      .await
      .unwrap();
   let error = restored
      .set_piece_storage(PieceStorageStrategy::Disk(
         std::env::temp_dir().join("libtortillas-rejected-storage-change"),
      ))
      .await
      .unwrap_err();

   assert!(matches!(
      error,
      TorrentError::InvalidOperation {
         operation: "set piece storage",
         ..
      }
   ));
   target.shutdown().await.unwrap();
}

#[test]
fn torrent_snapshot_v2_golden_fixture_round_trips_every_field() {
   let snapshot: TorrentSnapshot = serde_json::from_str(SNAPSHOT_V2).unwrap();
   snapshot.validate().unwrap();

   let expected: serde_json::Value = serde_json::from_str(SNAPSHOT_V2).unwrap();
   let actual = serde_json::to_value(snapshot).unwrap();

   assert_eq!(actual, expected);
}

#[test]
fn torrent_snapshot_v1_golden_fixture_migrates_to_canonical_v2() {
   let snapshot: TorrentSnapshot = serde_json::from_str(SNAPSHOT_V1).unwrap();

   assert_eq!(
      snapshot.version,
      libtortillas::torrent::TORRENT_SNAPSHOT_VERSION
   );
   assert!(snapshot.resolved_magnet_info.is_none());
   assert!(snapshot.block_map.is_empty());
   snapshot.validate().unwrap();
}

#[test]
fn engine_snapshot_golden_fixtures_migrate_and_round_trip() {
   let migrated: libtortillas::engine::EngineSnapshot =
      serde_json::from_str(ENGINE_SNAPSHOT_V1).unwrap();
   assert_eq!(
      migrated.version,
      libtortillas::engine::ENGINE_SNAPSHOT_VERSION
   );

   let current: libtortillas::engine::EngineSnapshot =
      serde_json::from_str(ENGINE_SNAPSHOT_V2).unwrap();
   let expected: serde_json::Value = serde_json::from_str(ENGINE_SNAPSHOT_V2).unwrap();
   assert_eq!(serde_json::to_value(current).unwrap(), expected);
}
