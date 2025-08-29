use std::{io::SeekFrom, path::PathBuf};

use tokio::{
   fs::OpenOptions,
   io::{AsyncSeekExt, AsyncWriteExt},
   sync::mpsc,
};
use tracing::{error, info};

use crate::torrent::StreamedPiece;

pub struct PieceManager {
   receiver: mpsc::Receiver<StreamedPiece>,
   pub output_path: Option<PathBuf>,
   pub piece_length: usize,
}

impl PieceManager {
   pub fn new(
      receiver: mpsc::Receiver<StreamedPiece>, output_path: Option<PathBuf>, piece_length: usize,
   ) -> Self {
      Self {
         receiver,
         output_path,
         piece_length,
      }
   }

   pub async fn process_piece(&mut self) {
      let Some(path) = &self.output_path else {
         error!("Output path not set");
         return;
      };

      // Open file in append/update mode (create if missing)
      let mut file = match OpenOptions::new()
         .create(true)
         .write(true)
         .read(true)
         .open(path)
         .await
      {
         Ok(f) => f,
         Err(e) => {
            error!("Failed to open file {}: {}", path.display(), e);
            return;
         }
      };

      while let Some(piece) = self.receiver.recv().await {
         let offset = piece.index * self.piece_length + piece.offset;

         if let Err(e) = file.seek(SeekFrom::Start(offset as u64)).await {
            error!("Failed to seek in file {}: {}", path.display(), e);
            continue;
         }

         if let Err(e) = file.write_all(&piece.data).await {
            error!(
               "Failed to write piece {} at offset {}: {}",
               piece.index, offset, e
            );
            continue;
         }

         info!(
            "Wrote piece {} ({} bytes) at offset {} for file {}",
            piece.index,
            piece.data.len(),
            offset,
            piece.name
         );
      }
   }
}
