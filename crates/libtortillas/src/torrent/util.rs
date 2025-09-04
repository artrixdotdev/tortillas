use std::{io::SeekFrom, path::Path};

use bytes::Bytes;
use tokio::{
   fs::{File, OpenOptions},
   io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, copy},
};

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
