use std::{path::PathBuf, sync::atomic::AtomicU8};

use bitvec::vec::BitVec;
use serde::{Deserialize, Serialize};

use super::{BLOCK_SIZE, BlockMap, PieceStorageStrategy, TorrentState};
use crate::{
   errors::TorrentError,
   hashes::InfoHash,
   metainfo::{Info, MetaInfo},
};

/// Current persistence schema version for [`TorrentSnapshot`].
pub const TORRENT_SNAPSHOT_VERSION: u32 = 1;

/// Serializable state required to restore a torrent session.
///
/// Frontends choose the Serde format and storage location. Live UI rendering
/// should use [`Torrent::listener`](super::Torrent::listener), not this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentSnapshot {
   pub version: u32,
   pub info_hash: InfoHash,
   pub state: TorrentState,
   pub auto_start: bool,
   pub sufficient_peers: usize,
   pub output_path: Option<PathBuf>,
   pub metainfo: MetaInfo,
   pub piece_storage: PieceStorageStrategy,
   pub info_dict: Option<Info>,
   pub bitfield: BitVec<AtomicU8>,
   pub block_map: BlockMap,
}

impl TorrentSnapshot {
   /// Validates schema compatibility and all redundant integrity fields.
   pub fn validate(&self) -> Result<(), TorrentError> {
      if self.version != TORRENT_SNAPSHOT_VERSION {
         return Err(self.invalid(format!(
            "unsupported version {}; expected {}",
            self.version, TORRENT_SNAPSHOT_VERSION
         )));
      }
      let metainfo_hash = self
         .metainfo
         .info_hash()
         .map_err(|error| self.invalid(format!("failed to hash metainfo: {error}")))?;
      if metainfo_hash != self.info_hash {
         return Err(self.invalid("info hash does not match metainfo"));
      }
      if let Some(info) = &self.info_dict {
         let restored_hash = info
            .hash()
            .map_err(|error| self.invalid(format!("failed to hash info dictionary: {error}")))?;
         if restored_hash != self.info_hash {
            return Err(self.invalid("restored info dictionary does not match the info hash"));
         }
      }

      let info = self.resolved_info();
      let piece_count = info.map_or(0, Info::piece_count);
      if self.bitfield.len() != piece_count {
         return Err(self.invalid(format!(
            "bitfield has {} pieces but metadata declares {piece_count}",
            self.bitfield.len()
         )));
      }
      for entry in &self.block_map {
         let index = *entry.key();
         if index >= piece_count {
            return Err(self.invalid("partial piece index is outside the metadata piece range"));
         }
         if self.bitfield[index] {
            return Err(self.invalid("completed piece also contains partial block state"));
         }

         let Some(info) = info else {
            return Err(self.invalid("partial block state requires resolved metadata"));
         };
         let piece_length = usize::try_from(info.piece_length)
            .map_err(|_| self.invalid("piece length cannot be represented on this platform"))?;
         if piece_length == 0 {
            return Err(self.invalid("piece length must be greater than zero"));
         }
         let last_piece = piece_count.saturating_sub(1);
         let concrete_length = if index == last_piece {
            let remainder = info.total_length() % piece_length;
            if remainder == 0 {
               piece_length
            } else {
               remainder
            }
         } else {
            piece_length
         };
         let expected_blocks = concrete_length.div_ceil(BLOCK_SIZE);
         if entry.value().len() != expected_blocks {
            return Err(self.invalid(format!(
               "partial piece {index} has {} blocks; expected {expected_blocks}",
               entry.value().len()
            )));
         }
      }

      Ok(())
   }

   pub(crate) fn resolved_info(&self) -> Option<&Info> {
      self.info_dict.as_ref().or(match &self.metainfo {
         MetaInfo::Torrent(torrent) => Some(&torrent.info),
         MetaInfo::MagnetUri(_) => None,
      })
   }

   fn invalid(&self, reason: impl Into<String>) -> TorrentError {
      TorrentError::InvalidSnapshot {
         reason: reason.into(),
      }
   }
}
