use std::path::PathBuf;

use libtortillas::{
   facade::{EngineSnapshot, TorrentSnapshot},
   hashes::InfoHash,
   prelude::{CoreCommand, EngineHandle, TorrentCommand, TorrentSource},
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
fn engine_command_routes_the_shared_torrent_command_type() {
   let torrent = InfoHash::from_bytes([7; 20]);
   let command = CoreCommand::Torrent {
      torrent,
      command: TorrentCommand::Pause,
   };

   assert_eq!(
      command,
      CoreCommand::Torrent {
         torrent,
         command: TorrentCommand::Pause,
      }
   );

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
