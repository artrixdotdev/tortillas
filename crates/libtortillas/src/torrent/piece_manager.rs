use std::path::PathBuf;

use async_trait::async_trait;
use bytes::Bytes;

use super::util;
use crate::prelude::{Info, InfoKeys};
#[allow(unused)]
#[async_trait]
pub trait PieceManager: Clone + Send + Sync {
   async fn pre_start(&self, info: Info) -> anyhow::Result<()>;
   async fn recv(&self, filename: String, index: usize, offset: usize, data: Bytes);
}

#[allow(unused)]
#[derive(Clone)]
pub struct FilePieceManager {
   piece_length: usize,
   total_length: usize,
   base_path: PathBuf,
}

impl FilePieceManager {}

#[async_trait]
impl PieceManager for FilePieceManager {
   async fn pre_start(&self, info_dict: Info) -> anyhow::Result<()> {
      match info_dict.file {
         InfoKeys::Single { length, .. } => {
            util::create_empty_file(&info_dict.name, length as usize).await?;
         }

         InfoKeys::Multi { files } => {
            for file in files {
               util::create_empty_file(&file.path.concat(), file.length).await?;
            }
         }
      }
      Ok(())
   }

   #[allow(unused)]
   async fn recv(&self, filename: String, index: usize, offset: usize, data: Bytes) {}
}

#[cfg(test)]
mod tests {
   use tokio::fs;

   use super::*;

   #[tokio::test]
   async fn test_create_empty_file() {
      let temp_dir = std::env::temp_dir();
      let path = temp_dir.join("tortillas-test");

      let _manager = FilePieceManager {
         piece_length: 1024,
         total_length: 1024,
         base_path: path.clone(),
      };

      util::create_empty_file(&path, 1_000).await.unwrap();

      assert!(path.exists());

      // This is not absolutely necessary, but it is the appropriate thing to do.
      fs::remove_file(path.clone()).await.unwrap();
   }
}
