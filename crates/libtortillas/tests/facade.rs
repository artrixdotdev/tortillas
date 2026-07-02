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

   let command = CoreCommand::PauseTorrent { torrent };
   assert_eq!(command, CoreCommand::PauseTorrent { torrent });

   let command = CoreCommand::ResumeTorrent { torrent };
   assert_eq!(command, CoreCommand::ResumeTorrent { torrent });

   let command = CoreCommand::StopTorrent { torrent };
   assert_eq!(command, CoreCommand::StopTorrent { torrent });
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

#[test]
fn facade_torrent_snapshot_counts_the_short_final_piece_exactly() {
   let mut export = torrent_export();
   let last_piece = export.bitfield.len() - 1;
   export.bitfield.fill(false);
   export.bitfield.set_aliased(last_piece, true);

   let snapshot = TorrentSnapshot::from_export(export);

   assert_eq!(snapshot.progress.verified_bytes, 145_691);
}

#[test]
fn facade_torrent_snapshot_deduplicates_tracker_urls() {
   let mut export = torrent_export();
   let MetaInfo::Torrent(torrent_file) = &mut export.metainfo else {
      panic!("expected torrent fixture");
   };
   torrent_file.announce_list = Some(vec![vec![
      torrent_file.announce.clone(),
      torrent_file.announce.clone(),
   ]]);

   let snapshot = TorrentSnapshot::from_export(export);

   assert_eq!(snapshot.trackers.len(), 1);
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
