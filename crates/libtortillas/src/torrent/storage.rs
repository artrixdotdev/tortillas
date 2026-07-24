use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Defines how torrent pieces are stored and accessed.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "strategy", content = "piece_output_path")]
pub enum PieceStorageStrategy {
   /// Reference pieces directly from the downloaded files themselves.
   #[default]
   InFile,
   /// Write each piece to disk separately in the given cache directory.
   Disk(PathBuf),
}
