pub mod engine;
pub mod errors;
pub mod hashes;
pub mod metainfo;
pub mod peer;
pub mod protocol;
pub mod torrent;
pub mod tracker;
pub(crate) mod util;

#[cfg(test)]
pub(crate) mod test_support {
   use std::path::PathBuf;

   use crate::metainfo::{MagnetUri, MetaInfo, TorrentFile};

   pub(crate) const BIG_BUCK_BUNNY_MAGNET: &str =
      include_str!("../tests/magneturis/big-buck-bunny.txt");
   pub(crate) const BIG_BUCK_BUNNY_MAGNET_FILE: &str = "big-buck-bunny.txt";
   pub(crate) const BIG_BUCK_BUNNY_TORRENT_FILE: &str = "big-buck-bunny.torrent";
   pub(crate) const KNOPPIX_TORRENT_FILE: &str = "KNOPPIX_V9.1DVD-2021-01-25-EN.torrent";

   pub(crate) fn fixture_path(relative_path: &str) -> PathBuf {
      PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path)
   }

   pub(crate) fn torrent_fixture_path(file_name: &str) -> PathBuf {
      fixture_path(&format!("tests/torrents/{file_name}"))
   }

   pub(crate) fn magnet_fixture_path(file_name: &str) -> PathBuf {
      fixture_path(&format!("tests/magneturis/{file_name}"))
   }

   pub(crate) async fn read_torrent_fixture(file_name: &str) -> MetaInfo {
      TorrentFile::read(torrent_fixture_path(file_name))
         .await
         .unwrap()
   }

   pub(crate) async fn read_magnet_fixture(file_name: &str) -> MetaInfo {
      let contents = tokio::fs::read_to_string(magnet_fixture_path(file_name))
         .await
         .unwrap();
      MagnetUri::parse(contents).unwrap()
   }

   pub(crate) fn init_tracing() {
      let _ = tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .try_init();
   }
}

/// The prelude for this crate.
///
/// This module re-exports the most commonly used types, traits, and functions
/// so that you can conveniently import them all at once:
///
/// ```
/// use libtortillas::prelude::*;
/// ```
pub mod prelude {
   pub use crate::{
      engine::*,
      errors::*,
      hashes::InfoHash,
      metainfo::*,
      peer::{Peer, PeerId},
      torrent::*,
      tracker::Tracker,
   };
}
