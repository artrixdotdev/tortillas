use super::Metainfo;
use anyhow::Result;
use serde_bencode as bencode;
use std::path::PathBuf;

use tokio::fs;

/// Parse torrent file into [`Metainfo`](super::Metainfo).
pub async fn parse_file(path: PathBuf) -> Result<Metainfo> {
   let file = fs::read(path).await?;
   let metainfo: Metainfo = bencode::from_bytes(&file)?;
   Ok(metainfo)
}

#[cfg(test)]
mod tests {
   use super::*;

   #[tokio::test]
   async fn test_parse_file() {
      println!("{}", std::env::current_dir().unwrap().display());
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/torrents/big-buck-bunny.torrent");
      let metainfo = parse_file(path).await.unwrap();

      assert_eq!(metainfo.info.name, "Big Buck Bunny");
   }
}
