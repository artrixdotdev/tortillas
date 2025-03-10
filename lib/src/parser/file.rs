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
