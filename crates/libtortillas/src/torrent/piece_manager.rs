use std::{
   os::unix::io::AsRawFd,
   path::{Path, PathBuf},
   str::FromStr,
};

use async_trait::async_trait;
use bytes::Bytes;
use tokio::{
   fs::{self, File},
   io::{AsyncReadExt, AsyncWriteExt, copy},
};

use crate::prelude::{Info, InfoKeys};

#[async_trait]
pub trait PieceManager: Clone + Send + Sync {
   async fn pre_start(&self, info: Info) -> anyhow::Result<()>;
   async fn recv(&self, filename: String, index: usize, offset: usize, data: Bytes);
}

#[derive(Clone)]
pub struct FilePieceManager {
   piece_length: usize,
   total_length: usize,
   base_path: PathBuf,
}

impl FilePieceManager {
   async fn create_empty_file(&self, path: impl AsRef<Path>, length: usize) -> anyhow::Result<()> {
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
}

#[async_trait]
impl PieceManager for FilePieceManager {
   async fn pre_start(&self, info_dict: Info) -> anyhow::Result<()> {
      match info_dict.file {
         InfoKeys::Single { length, .. } => {
            self
               .create_empty_file(&info_dict.name, length as usize)
               .await?;
         }

         InfoKeys::Multi { files } => {
            for file in files {
               self
                  .create_empty_file(&file.path.concat(), file.length)
                  .await?;
            }
         }
      }
      Ok(())
   }

   async fn recv(&self, filename: String, index: usize, offset: usize, data: Bytes) {}
}

#[cfg(test)]
mod tests {
   use super::*;

   #[tokio::test]
   async fn test_create_empty_file() {
      let temp_dir = std::env::temp_dir();
      let path = temp_dir.join("tortillas-test");

      let manager = FilePieceManager {
         piece_length: 1024,
         total_length: 1024,
         base_path: path.clone(),
      };

      manager
         .create_empty_file(&path, 1_000_000_000)
         .await
         .unwrap();

      assert!(path.exists());
   }
}
