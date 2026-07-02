use std::path::PathBuf;

use libtortillas::{
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
}
