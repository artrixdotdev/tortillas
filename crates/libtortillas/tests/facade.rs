use std::{path::PathBuf, sync::atomic::AtomicU8};

use bitvec::vec::BitVec;
use libtortillas::{
   engine::EngineExport,
   facade::{EngineSnapshot, TorrentSnapshot, TrackerStatus},
   hashes::InfoHash,
   metainfo::{MetaInfo, TorrentFile},
   prelude::{CoreCommand, EngineHandle, TorrentSource},
   torrent::{BlockMap, PieceStorageStrategy, TorrentExport, TorrentState},
};

#[test]
fn prelude_exposes_frontend_facade_types() {
   let command = CoreCommand::AddTorrent {
      source: TorrentSource::TorrentFilePath(PathBuf::from("ubuntu.torrent")),
   };

   match command {
      CoreCommand::AddTorrent {
         source: TorrentSource::TorrentFilePath(path),
      } => assert_eq!(path, PathBuf::from("ubuntu.torrent")),
      other => panic!("unexpected command: {other:?}"),
   }
}

#[test]
fn facade_engine_handle_matches_existing_engine_type() {
   fn accepts_engine_handle(_: Option<EngineHandle>) {}

   accepts_engine_handle(None);
}

#[test]
fn command_variants_identify_torrents_by_info_hash() {
   let torrent = InfoHash::from_bytes([7; 20]);
   let command = CoreCommand::StartTorrent { torrent };

   assert_eq!(command, CoreCommand::StartTorrent { torrent });
}

#[test]
fn facade_torrent_snapshot_maps_current_export() {
   let export = torrent_export();
   let snapshot = TorrentSnapshot::from_export(export);

   assert_eq!(snapshot.name.as_deref(), Some("Big Buck Bunny"));
   assert_eq!(snapshot.state, TorrentState::Downloading);
   assert_eq!(snapshot.progress.total_bytes, 276_445_467);
   assert_eq!(snapshot.progress.verified_bytes, 262_144);
   assert!(snapshot.peers.is_empty());
   assert!(snapshot.trackers.iter().all(|tracker| {
      !tracker.announce_url.is_empty() && tracker.status == TrackerStatus::Pending
   }));
}

#[test]
fn facade_engine_snapshot_maps_torrent_exports() {
   let export = EngineExport {
      torrents: vec![torrent_export()],
   };

   let snapshot = EngineSnapshot::from_export(export);

   assert_eq!(snapshot.torrents.len(), 1);
   assert_eq!(snapshot.torrents[0].name.as_deref(), Some("Big Buck Bunny"));
}

fn torrent_export() -> TorrentExport {
   let metainfo = TorrentFile::parse(include_bytes!("torrents/big-buck-bunny.torrent")).unwrap();
   let info_hash = metainfo.info_hash().unwrap();
   let info = match &metainfo {
      MetaInfo::Torrent(torrent_file) => &torrent_file.info,
      MetaInfo::MagnetUri(_) => panic!("expected torrent fixture"),
   };
   let bitfield = BitVec::<AtomicU8>::repeat(false, info.piece_count());
   bitfield.set_aliased(0, true);

   TorrentExport {
      info_hash,
      state: TorrentState::Downloading,
      auto_start: true,
      sufficient_peers: 6,
      output_path: Some(PathBuf::from("/tmp/tortillas")),
      metainfo,
      piece_storage: PieceStorageStrategy::InFile,
      info_dict: None,
      bitfield,
      block_map: BlockMap::new(),
   }
}
