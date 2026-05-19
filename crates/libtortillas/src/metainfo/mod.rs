use serde::{Deserialize, Serialize};
mod file;
mod magnet;

pub use file::*;
pub use magnet::*;

use crate::{hashes::InfoHash, tracker::Tracker};

/// Always utilize MetaInfo instead of directly using TorrentFile or MagnetUri
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum MetaInfo {
   Torrent(TorrentFile),
   MagnetUri(#[serde(deserialize_with = "MagnetUri::deserialize")] MagnetUri),
}

impl MetaInfo {
   pub async fn new(path_or_url: String) -> Result<Self, anyhow::Error> {
      Ok(if path_or_url.starts_with("magnet:") {
         MagnetUri::parse(path_or_url)?
      } else {
         TorrentFile::read(path_or_url.into()).await?
      })
   }

   /// Returns the info hash for the given MetaInfo enum. If the enum is a
   /// [Torrent](TorrentFile), then this function will calculate and return
   /// the hash. If the enum is a [MagnetUri], then this function will grab
   /// the existing hash and return it, as the MagnetUri spec already contains
   /// the hash.
   pub fn info_hash(&self) -> Result<InfoHash, anyhow::Error> {
      match &self {
         MetaInfo::Torrent(torrent) => torrent
            .info
            .hash()
            .map_err(|e| anyhow::anyhow!("Failed to compute torrent info hash: {}", e)),
         MetaInfo::MagnetUri(magnet_uri) => magnet_uri
            .info_hash()
            .map_err(|e| anyhow::anyhow!("Failed to extract magnet URI info hash: {}", e)),
      }
   }

   pub fn announce_list(&self) -> Vec<Tracker> {
      match &self {
         MetaInfo::Torrent(file) => file.announce_list(),
         MetaInfo::MagnetUri(magnet) => magnet.announce_list(),
      }
   }

   pub fn clear_announce_list(&mut self) {
      match self {
         MetaInfo::Torrent(file) => file.announce_list = None,
         MetaInfo::MagnetUri(magnet) => magnet.announce_list = None,
      };
   }
}

#[cfg(test)]
mod tests {
   use tracing_test::traced_test;

   use super::*;
   use crate::{
      test_support::{
         BIG_BUCK_BUNNY_INFO_HASH, BIG_BUCK_BUNNY_TORRENT_FILE, big_buck_bunny_magnet,
         read_torrent_fixture,
      },
      tracker::Tracker,
   };

   #[tokio::test]
   #[traced_test]
   async fn metainfo_when_source_is_magnet_uri_then_returns_expected_info_hash() {
      let metainfo = big_buck_bunny_magnet();

      let info_hash = metainfo.info_hash().unwrap();
      assert_eq!(info_hash.to_hex(), BIG_BUCK_BUNNY_INFO_HASH);
   }

   #[tokio::test]
   #[traced_test]
   async fn metainfo_when_source_is_torrent_file_then_returns_expected_info_hash() {
      let file = read_torrent_fixture(BIG_BUCK_BUNNY_TORRENT_FILE).await;

      let info_hash = file.info_hash().unwrap();
      assert_eq!(info_hash.to_hex(), BIG_BUCK_BUNNY_INFO_HASH);
   }

   #[tokio::test]
   #[traced_test]
   async fn metainfo_when_source_is_magnet_uri_then_parses_udp_announce() {
      let metainfo = big_buck_bunny_magnet();
      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            assert!(matches!(magnet.announce_list.unwrap()[0], Tracker::Udp(_)))
         }
         _ => panic!("Expected MagnetUri"),
      };
   }

   #[tokio::test]
   #[traced_test]
   async fn metainfo_when_magnet_and_torrent_match_then_share_info_hash() {
      let metainfo = big_buck_bunny_magnet();
      let info_hash = metainfo.info_hash().unwrap();

      let file = read_torrent_fixture(BIG_BUCK_BUNNY_TORRENT_FILE).await;

      let torrent_info_hash = file.info_hash().unwrap();

      assert_eq!(info_hash, torrent_info_hash);
   }
}
