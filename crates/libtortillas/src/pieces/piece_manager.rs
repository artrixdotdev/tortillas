use std::{io::SeekFrom, path::PathBuf};

use anyhow::ensure;
use async_trait::async_trait;
use bytes::Bytes;
use tokio::{
   fs::{OpenOptions, create_dir_all},
   io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};
use tracing::trace;

use crate::metainfo::{Info, InfoKeys};

#[allow(unused)]
#[async_trait]
pub trait PieceManager: Send + Sync {
   fn info(&self) -> Option<&Info>;
   async fn pre_start(&mut self, info: Info) -> anyhow::Result<()>;
   /// Receives a piece from the torrent
   ///
   /// To figure out which file(s) to write the piece to, use the
   /// [`PieceManager::piece_to_paths`] function.
   ///
   /// # Arguments
   /// - `index`: The index of the piece to receive
   /// - `data`: The piece data
   ///
   /// # Errors
   /// If this function return errors, the piece will be re-requested.
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

               let mut relative_path = PathBuf::new();

               if file.path.first().map(|component| component.as_str()) != Some(info.name.as_str())
               {
                  relative_path.push(&info.name);
               }

               for component in &file.path {
                  relative_path.push(component);
               }

               results.push((relative_path, offset_in_file, take_len));

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

   pub(crate) async fn write_block(
      &self, index: usize, offset: usize, block: Bytes,
   ) -> anyhow::Result<()> {
      self.write_piece_range(index, offset, &block).await
   }

   pub(crate) async fn read_piece(&self, index: usize) -> anyhow::Result<Bytes> {
      let piece_bounds = self.piece_to_paths(index)?;
      let piece_len = piece_bounds.iter().map(|(_, _, len)| len).sum();
      self.read_piece_range(index, 0, piece_len).await
   }

   pub(crate) async fn read_piece_block(
      &self, index: usize, offset: usize, length: usize,
   ) -> anyhow::Result<Bytes> {
      self.read_piece_range(index, offset, length).await
   }

   async fn write_piece_range(
      &self, index: usize, offset: usize, data: &[u8],
   ) -> anyhow::Result<()> {
      let base_path = self.path().ok_or_else(|| anyhow::anyhow!("path not set"))?;
      let mut remaining = data.len();
      let mut skipped = offset;
      let mut data_offset = 0;

      for (path, file_offset, len) in self.piece_to_paths(index)? {
         if skipped >= len {
            skipped -= len;
            continue;
         }

         let write_len = remaining.min(len - skipped);
         let full_path = base_path.join(&path);
         if let Some(parent) = full_path.parent() {
            create_dir_all(parent).await?;
         }

         let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&full_path)
            .await?;
         file
            .seek(SeekFrom::Start((file_offset + skipped) as u64))
            .await?;
         file
            .write_all(&data[data_offset..data_offset + write_len])
            .await?;

         remaining -= write_len;
         data_offset += write_len;
         skipped = 0;

         if remaining == 0 {
            return Ok(());
         }
      }

      anyhow::bail!("piece range exceeds file lengths")
   }

   async fn read_piece_range(
      &self, index: usize, offset: usize, length: usize,
   ) -> anyhow::Result<Bytes> {
      let base_path = self.path().ok_or_else(|| anyhow::anyhow!("path not set"))?;
      let mut remaining = length;
      let mut skipped = offset;
      let mut data = Vec::with_capacity(length);

      for (path, file_offset, len) in self.piece_to_paths(index)? {
         if skipped >= len {
            skipped -= len;
            continue;
         }

         let read_len = remaining.min(len - skipped);
         let full_path = base_path.join(&path);
         let mut file = OpenOptions::new().read(true).open(&full_path).await?;
         file
            .seek(SeekFrom::Start((file_offset + skipped) as u64))
            .await?;

         let start = data.len();
         data.resize(start + read_len, 0);
         file.read_exact(&mut data[start..]).await?;

         remaining -= read_len;
         skipped = 0;

         if remaining == 0 {
            return Ok(data.into());
         }
      }

      anyhow::bail!("piece range exceeds file lengths")
   }
}

#[async_trait]
impl PieceManager for FilePieceManager {
   fn info(&self) -> Option<&Info> {
      self.1.as_ref()
   }

   async fn pre_start(&mut self, info_dict: Info) -> anyhow::Result<()> {
      let base_path = self
         .0
         .as_ref()
         .ok_or_else(|| anyhow::anyhow!("path must be set before pre_start"))?;

      let info_hash = info_dict.hash()?;

      trace!(torrent_id = %info_hash, path = %base_path.display(), "Pre-starting piece manager");

      self.1 = Some(info_dict);
      Ok(())
   }

   #[allow(unused)]
   async fn recv(&self, index: usize, data: Bytes) -> anyhow::Result<()> {
      let _info = self.info().ok_or_else(|| anyhow::anyhow!("info not set"))?;
      self.write_piece_range(index, 0, &data).await
   }
}

#[cfg(test)]
mod tests {
   use super::*;
   use crate::{
      hashes::{Hash, HashVec},
      prelude::MetaInfo,
      testing,
   };

   fn piece_hash(data: &[u8]) -> Hash<20> {
      use sha1::{Digest, Sha1};

      let mut hasher = Sha1::new();
      hasher.update(data);
      Hash::from_bytes(hasher.finalize().into())
   }

   fn single_file_info() -> Info {
      Info {
         name: "data.bin".to_string(),
         piece_length: 4,
         pieces: HashVec::from(vec![piece_hash(b"abcd"), piece_hash(b"ef")]),
         file: InfoKeys::Single {
            length: 6,
            md5sum: None,
         },
         is_private: None,
         publisher: None,
         publisher_url: None,
         source: None,
      }
   }

   #[tokio::test]
   async fn file_piece_manager_when_mapping_first_piece_then_returns_expected_file() {
      testing::init_tracing();

      let MetaInfo::Torrent(torrent) =
         testing::read_torrent_fixture(testing::BIG_BUCK_BUNNY_TORRENT_FILE).await
      else {
         panic!("failed to parse torrent file");
      };
      let manager = FilePieceManager(Some(testing::fixture_path("")), Some(torrent.info));

      // Pick a few representative pieces
      assert!(manager.piece_to_paths(0).is_ok());

      let (file_path, _, _) = manager.piece_to_paths(0).unwrap()[0].clone();

      // The first file should always be "Big Buck Bunny.en.srt"
      assert_eq!(file_path.extension().unwrap(), "srt");
   }

   #[tokio::test]
   async fn file_piece_manager_when_writing_blocks_then_reads_piece_back() {
      let base_path = testing::torrent_temp_path();
      let manager = FilePieceManager(Some(base_path.clone()), Some(single_file_info()));

      manager
         .write_block(0, 0, Bytes::from_static(b"ab"))
         .await
         .unwrap();
      manager
         .write_block(0, 2, Bytes::from_static(b"cd"))
         .await
         .unwrap();

      assert_eq!(
         manager.read_piece(0).await.unwrap(),
         Bytes::from_static(b"abcd")
      );
      assert_eq!(
         manager.read_piece_block(0, 1, 2).await.unwrap(),
         Bytes::from_static(b"bc")
      );

      tokio::fs::remove_dir_all(base_path).await.unwrap();
   }
}
