use serde::Deserialize;
mod file;
mod magnet;

pub use file::*;
pub use magnet::*;

use crate::{
   hashes::{Hash, HashVec, InfoHash},
   tracker::Tracker,
};

/// Always utilize MetaInfo instead of directly using TorrentFile or MagnetUri
#[derive(Debug, Deserialize)]
pub enum MetaInfo {
   Torrent(TorrentFile),
   MagnetUri(MagnetUri),
}

impl MetaInfo {
   pub async fn new(path_or_url: String) -> Result<Self, anyhow::Error> {
      Ok(if path_or_url.starts_with("magnet:") {
         MagnetUri::parse(path_or_url.into())?
      } else {
         TorrentFile::read(path_or_url.into()).await?
      })
   }

   /// Returns the info hash for the given MetaInfo enum. If the enum is a [Torrent](TorrentFile), then this
   /// function will calculate and return the hash. If the enum is a [MagnetUri], then this
   /// function will grab the existing hash and return it, as the MagnetUri spec already contains
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
}

#[cfg(test)]
mod tests {
   use crate::tracker::Tracker;
   use tracing_test::traced_test;

   use super::*;

   #[tokio::test]
   #[traced_test]
   async fn test_info_hash_with_magneturi() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MagnetUri::parse(contents).unwrap();

      let info_hash = metainfo.info_hash().unwrap();
      assert_eq!(
         info_hash.to_hex(),
         "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c"
      );
   }

   #[tokio::test]
   #[traced_test]
   async fn test_info_hash_with_torrent() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/torrents/big-buck-bunny.torrent");
      let file = TorrentFile::read(path).await.unwrap();

      let info_hash = file.info_hash().unwrap();
      assert_eq!(
         info_hash.to_hex(),
         "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c"
      );
   }

   #[tokio::test]
   #[traced_test]
   async fn test_announce_uri() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MagnetUri::parse(contents).unwrap();
      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            matches!(magnet.announce_list.unwrap()[0], Tracker::Udp(_))
         }
         _ => panic!("Expected MagnetUri"),
      };
   }

   #[tokio::test]
   #[traced_test]
   async fn test_compare_magnet_and_torrent_info_hash() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MagnetUri::parse(contents).unwrap();
      let info_hash = metainfo.info_hash().unwrap();

      let path = std::env::current_dir()
         .unwrap()
         .join("tests/torrents/big-buck-bunny.torrent");
      let file = TorrentFile::read(path).await.unwrap();

      let torrent_info_hash = file.info_hash().unwrap();

      assert_eq!(info_hash, torrent_info_hash);
   }
}
