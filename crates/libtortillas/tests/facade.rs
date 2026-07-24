use std::path::PathBuf;

use libtortillas::{
   facade::{EngineSnapshot, TorrentSnapshot},
   hashes::InfoHash,
   prelude::{CoreCommand, EngineHandle, TorrentSource},
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

   let command = CoreCommand::RemoveTorrent { torrent };
   assert_eq!(command, CoreCommand::RemoveTorrent { torrent });

   let command = CoreCommand::Shutdown;
   assert_eq!(command, CoreCommand::Shutdown);
}

#[test]
fn facade_reexports_canonical_snapshot_types() {
   fn accepts_engine_snapshot(_: Option<libtortillas::engine::EngineSnapshot>) {}
   fn accepts_torrent_snapshot(_: Option<libtortillas::torrent::TorrentSnapshot>) {}

   let engine_snapshot: Option<EngineSnapshot> = None;
   let torrent_snapshot: Option<TorrentSnapshot> = None;

   accepts_engine_snapshot(engine_snapshot);
   accepts_torrent_snapshot(torrent_snapshot);
}
