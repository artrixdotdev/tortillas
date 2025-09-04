use std::{io::SeekFrom, path::Path};

use anyhow::anyhow;
use bytes::Bytes;
use sha1::{Digest, Sha1};
use tokio::{
   fs::{File, OpenOptions},
   io::{self, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, copy},
   task::spawn_blocking,
};

use crate::hashes::{Hash, HashVec};

pub async fn create_empty_file(path: impl AsRef<Path>, length: usize) -> anyhow::Result<()> {
   let mut out = File::create(path).await?;
   if cfg!(target_family = "unix") {
      let zero = File::open("/dev/zero").await?;
      let mut limited = zero.take(length as u64);

      if copy(&mut limited, &mut out).await? != length as u64 {
         return Err(anyhow::anyhow!("Failed to copy exact number of bytes"));
      }
   } else {
      let chunk = [0u8; 8192]; // 8 KB zero buffer

      let mut written: usize = 0;
      while written < length {
         let to_write = std::cmp::min(chunk.len(), length - written);
         out.write_all(&chunk[..to_write]).await?;
         written += to_write;
      }
   }
   Ok(())
}

pub async fn write_block_to_file(
   path: impl AsRef<Path>, offset: usize, block: Bytes,
) -> anyhow::Result<()> {
   let mut file = OpenOptions::new()
      .read(false)
      .write(true)
      .create(false)
      .open(path)
      .await?;

   file.seek(SeekFrom::Start(offset as u64)).await?;

   file.write_all(&block).await?;

   Ok(())
}

/// Validates a piece file given the path and the pieces hash from our info
/// dictionary (the torrent's metadata). This function should only be called
/// when we have the info dictionary from either the orginal `.torrent` file or
/// from retrieving the info dict via BEP 0010/BEP 0009.
pub async fn validate_piece_file(
   path: impl AsRef<Path> + Send + 'static, hash: Hash<20>,
) -> anyhow::Result<()> {
   let piece_file_hash = spawn_blocking(move || {
      let mut hasher = Sha1::new();
      let mut file = std::fs::File::open(&path).unwrap();
      std::io::copy(&mut file, &mut hasher).unwrap();
      let hash = hasher.finalize();
      Hash::from_bytes(hash.into())
   })
   .await?;

   if piece_file_hash != hash {
      return Err(anyhow!("Hashed file was not equal to hash from info dict"));
   }

   Ok(())
}
