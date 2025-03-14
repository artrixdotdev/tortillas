use serde::Deserialize;

mod file;
mod magnet;

pub use file::*;
pub use magnet::*;

/// An Announce URI from a torrent file or magnet URI.
/// https://www.bittorrent.org/beps/bep_0012.html
/// Example: udp://tracker.opentrackr.org:1337/announce
#[derive(Debug, Deserialize)]
pub struct AnnounceUri(pub String);

/// Always utilize MetaInfo instead of directly using TorrentFile or MagnetUri
#[derive(Debug, Deserialize)]
pub enum MetaInfo {
   Torrent(TorrentFile),
   MagnetUri(MagnetUri),
}

impl MetaInfo {
   /// Returns the info hash for the given MetaInfo enum. If the enum is a [Torrent](TorrentFile), then this
   /// function will calculate and return the hash. If the enum is a [MagnetUri](MagnetUri), then this
   /// function will grab the existing hash and return it, as the MagnetUri spec already contains
   /// the hash.
   pub fn info_hash(&self) -> String {
      match &self {
         MetaInfo::Torrent(torrent) => torrent.info.hash().unwrap(),
         MetaInfo::MagnetUri(magnet_uri) => {
            String::from(magnet_uri.info_hash.split(":").last().unwrap())
         }
      }
   }
}

#[cfg(test)]
mod tests {

   use super::*;

   #[tokio::test]
   async fn test_info_hash_with_magneturi() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MagnetUri::parse(contents).await.unwrap();
      assert_eq!(
         metainfo.info_hash(),
         "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c"
      );
   }

   #[tokio::test]
   async fn test_info_hash_with_torrent() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/torrents/big-buck-bunny.torrent");
      let file = TorrentFile::parse(path).await.unwrap();
      assert_eq!(file.info_hash(), "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c");
   }
}
