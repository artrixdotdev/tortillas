use std::{io::SeekFrom, path::PathBuf};

use anyhow::ensure;
use async_trait::async_trait;
use bytes::Bytes;
use tokio::{
   fs::OpenOptions,
   io::{AsyncSeekExt, AsyncWriteExt},
};
use tracing::debug;

use crate::metainfo::{Info, InfoKeys};

#[allow(unused)]
#[async_trait]
pub trait PieceManager: Send + Sync {
   fn info(&self) -> Option<&Info>;
   async fn pre_start(&mut self, info: Info) -> anyhow::Result<()>;
   async fn recv(&self, index: usize, data: Bytes) -> anyhow::Result<()>;

   /// Maps a torrent piece index to its corresponding file segments.
   ///
   /// Each piece is a contiguous chunk of the torrent's data, but in multi-file
   /// torrents, a piece may span across multiple files. This function
   /// determines which file(s) contain the given piece and at what offsets.
   ///
   /// # Returns
   /// A [`Vec<(PathBuf, usize, usize)>`] where each tuple contains:
   /// - [`PathBuf`] -> the file’s path within the torrent
   /// - [`usize`]   -> the byte offset inside that file where the piece data
   ///   starts
   /// - [`usize`]   -> the number of bytes from that file that belong to the
   ///   piece
   ///
   /// # Errors
   /// - Returns an error if the torrent metadata (`info`) is missing.
   /// - Returns an error if the piece index extends beyond the total torrent
   ///   length.
   ///
   /// This ensures that every piece index is mapped precisely onto the
   /// underlying file storage layout.
   fn piece_to_paths(&self, index: usize) -> anyhow::Result<Vec<(PathBuf, usize, usize)>> {
      let info = self.info().ok_or_else(|| anyhow::anyhow!("info not set"))?;
      let piece_len = info.piece_length as usize;
      let total_len = info.total_length();

      let piece_start = index
         .checked_mul(piece_len)
         .ok_or_else(|| anyhow::anyhow!("piece index {index} overflows usize math"))?;
      let piece_end = (index
         .checked_add(1)
         .and_then(|i| i.checked_mul(piece_len))
         .ok_or_else(|| anyhow::anyhow!("piece index {index} overflows usize math"))?)
      .min(total_len);
      ensure!(
         piece_start < total_len,
         "piece index {index} exceeds file lengths"
      );
      let mut remaining = piece_end - piece_start;
      let mut acc = 0;
      let mut results = Vec::new();

      match &info.file {
         InfoKeys::Single { length, .. } => {
            // Single-file torrents just map to a single path = `name`
            let file_len = *length as usize;

            if piece_start < file_len {
               let offset_in_file = piece_start;
               let available_in_file = file_len - offset_in_file;
               let take_len = remaining.min(available_in_file);

               results.push((PathBuf::from(&info.name), offset_in_file, take_len));

               remaining -= take_len;
            }
         }
         InfoKeys::Multi { files } => {
            for file in files {
               let file_len = file.length;

               // Skip files before the piece
               if piece_start >= acc + file_len {
                  acc += file_len;
                  continue;
               }

               // Overlap with this file
               let offset_in_file = piece_start.saturating_sub(acc);
               let available_in_file = file_len - offset_in_file;
               let take_len = remaining.min(available_in_file);

               results.push((
                  PathBuf::from_iter(file.path.clone()),
                  offset_in_file,
                  take_len,
               ));

               remaining -= take_len;
               acc += file_len;

               if remaining == 0 {
                  break;
               }
            }
         }
      }

      ensure!(remaining == 0, "piece index {index} exceeds file lengths");
      Ok(results)
   }
}
#[derive(Debug, Clone)]
pub(crate) struct FilePieceManager(pub Option<PathBuf>, pub Option<Info>);

impl FilePieceManager {
   pub fn set_path(&mut self, path: PathBuf) {
      self.0 = Some(path);
   }
   pub fn path(&self) -> Option<&PathBuf> {
      self.0.as_ref()
   }
}

#[async_trait]
impl PieceManager for FilePieceManager {
   fn info(&self) -> Option<&Info> {
      self.1.as_ref()
   }

   async fn pre_start(&mut self, info_dict: Info) -> anyhow::Result<()> {
      // Intentional panic because this is unintended behavior
      assert!(self.0.is_some(), "Path must be set before pre_start");
      self.1 = Some(info_dict);
      Ok(())
   }

   #[allow(unused)]
   async fn recv(&self, index: usize, data: Bytes) -> anyhow::Result<()> {
      let base_path = &self.path().unwrap();
      let info = self.info().ok_or_else(|| anyhow::anyhow!("info not set"))?;

      let piece_bounds = self.piece_to_paths(index)?;
      let mut data_offset = 0; // Track how much of `data` we've consumed

      for (path, file_offset, len) in piece_bounds {
         // Ensure parent directories exist
         let full_path = base_path.join(&path);
         if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
         }

         let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&full_path)
            .await?;

         // Position file correctly
         file.seek(SeekFrom::Start(file_offset as u64)).await?;

         // Write correct slice from the piece buffer
         file
            .write_all(&data[data_offset..data_offset + len])
            .await?;

         data_offset += len;

         debug!(index, offset = file_offset, len, path = %path.display(), "Wrote piece to file");
      }

      Ok(())
   }
}

#[cfg(test)]
mod tests {
   use super::*;
   use crate::prelude::{MetaInfo, TorrentFile};
   #[tokio::test]
   async fn test_piece_paths() {
      tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .init();

      let path = std::env::current_dir().unwrap();

      // Parse real torrent
      let MetaInfo::Torrent(torrent) = TorrentFile::parse(include_bytes!(
         "../../tests/torrents/big-buck-bunny.torrent"
      ))
      .unwrap() else {
         panic!("failed to parse torrent file");
      };
      let manager = FilePieceManager(Some(path), Some(torrent.info));

      // Pick a few representative pieces
      assert!(manager.piece_to_paths(0).is_ok());

      let (file_path, _, _) = manager.piece_to_paths(0).unwrap()[0].clone();

      // The first file should always be "Big Buck Bunny.en.srt"
      assert_eq!(file_path.extension().unwrap(), "srt");
   }
}
