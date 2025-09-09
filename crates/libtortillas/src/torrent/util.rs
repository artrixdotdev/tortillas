use std::{
   io::SeekFrom,
   path::{Path, PathBuf},
};

use anyhow::ensure;
use bytes::Bytes;
use sha1::{Digest, Sha1};
use tokio::{
   fs::{self, File, OpenOptions},
   io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, copy},
   task::spawn_blocking,
};

use crate::hashes::Hash;

/// Creates a blank file padded with 0's.
///
/// # Examples
///
/// ```no_run
/// let path = "/tmp/my-file";
/// util::create_empty_file(path, 100).await;
///
/// assert!(path.exists());
/// ```
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

/// Writes a single block from a
/// [Piece](crate::protocol::messages::PeerMessages::Piece) message to a file
/// already padded with 0's. See [self::create_empty_file] for more.
///
/// # Examples
/// ```no_run
/// let message = PeerMessages::Piece(0, 0, Bytes::new());
/// let path = "/tmp/my-file"
///
/// if let PeerMessages::Piece(index, begin, block) = message {
///   util::write_block_to_file(path, begin, block);
/// }
/// ```
pub async fn write_block_to_file(
   path: impl AsRef<Path>, offset: usize, block: Bytes,
) -> anyhow::Result<()> {
   let mut file = OpenOptions::new()
      .create(true) // create if it doesn't exist
      .write(true) // open for writing
      .truncate(false) // don't clear file if it already exists
      .open(path)
      .await?;

   file.seek(SeekFrom::Start(offset as u64)).await?;
   file.write_all(&block).await?;
   file.flush().await?;

   Ok(())
}

/// Validates a piece file given the path and the pieces hash from our info
/// dictionary (the torrent's metadata).
///
/// This function should only be called when we have the info dictionary from
/// either the orginal `.torrent` file or from retrieving the info dict via [BEP 0010](https://www.bittorrent.org/beps/bep_0010.html) or [BEP 0009](https://www.bittorrent.org/beps/bep_0009.html),
/// and when we know that we've acquired every block for a piece.
///
/// # Examples
/// ```no_run
/// let info_dict = Info { ... };
/// let path = "/tmp/my-file";
/// let current_piece_hash = info_dict.pieces[0];
///
/// util::validate_piece_file(path, current_piece_hash);
/// ```
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

   ensure!(
      piece_file_hash == hash,
      "Hashed file was not equal to hash from info dict"
   );

   Ok(())
}

/// Creates a dir if it doesn't exist. This function can be called even if the
/// directory exists -- if it does, nothing will happen.
pub async fn create_dir(path: &PathBuf) -> Result<(), tokio::io::Error> {
   match path.try_exists() {
      // Path doesn't exist
      Ok(false) => fs::create_dir_all(path).await,
      Err(e) => Err(e),
      _ => Ok(()),
   }
}
