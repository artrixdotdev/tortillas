use super::MetaInfo;
use anyhow::Result;
use serde_bencode as bencode;
use std::path::PathBuf;

use tokio::fs;

/// Parse torrent file into [`Metainfo`](super::MetaInfo).
pub async fn parse_file(path: PathBuf) -> Result<MetaInfo> {
   let file = fs::read(path).await?;
   let metainfo: MetaInfo = MetaInfo::Torrent(bencode::from_bytes(&file)?);
   Ok(metainfo)
}

#[cfg(test)]
mod tests {
   use super::*;

   #[tokio::test]
   async fn test_parse_file() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/torrents/big-buck-bunny.torrent");

      let metainfo = parse_file(path).await.unwrap();

      match metainfo {
         MetaInfo::Torrent(torrent) => {
            assert_eq!(torrent.info.name, "Big Buck Bunny");
         }
         _ => panic!("Expected Torrent"),
      }
   }
}
