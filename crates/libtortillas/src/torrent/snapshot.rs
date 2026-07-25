use std::{collections::BTreeMap, path::PathBuf, sync::atomic::AtomicU8};

use bitvec::vec::BitVec;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use tokio::fs;

use super::{BLOCK_SIZE, PieceStorageStrategy, TorrentState, util};
use crate::{
   errors::TorrentError,
   hashes::InfoHash,
   metainfo::{Info, MetaInfo},
   pieces::FilePieceManager,
};

/// Current persistence schema version for [`TorrentSnapshot`].
pub const TORRENT_SNAPSHOT_VERSION: u32 = 2;

/// Amount of durable storage reconciliation performed before actor state is
/// installed. Full verification is the safe default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RestoreVerification {
   #[default]
   Full,
   FileMetadata,
   /// Trusts bitfields and block maps without checking referenced payload.
   /// Missing or corrupt data may not be detected until it is served or used.
   TrustSnapshot,
}

/// Portable partial-piece scheduler state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PieceBlockSnapshot {
   pub piece_index: u64,
   pub blocks: Vec<bool>,
}

/// Serializable state required to restore a torrent session.
#[derive(Debug, Clone, Serialize)]
pub struct TorrentSnapshot {
   pub version: u32,
   pub info_hash: InfoHash,
   pub state: TorrentState,
   pub auto_start: bool,
   pub sufficient_peers: u64,
   pub output_path: Option<PathBuf>,
   pub metainfo: MetaInfo,
   pub piece_storage: PieceStorageStrategy,
   /// Resolved metadata exists here only for magnet sources. `.torrent`
   /// sources already store the same `Info` in `metainfo`.
   #[serde(default, alias = "info_dict")]
   pub resolved_magnet_info: Option<Info>,
   pub bitfield: Vec<bool>,
   pub block_map: Vec<PieceBlockSnapshot>,
}

#[derive(Deserialize)]
struct TorrentSnapshotWire {
   version: u32,
   info_hash: InfoHash,
   state: TorrentState,
   auto_start: bool,
   sufficient_peers: u64,
   output_path: Option<PathBuf>,
   metainfo: MetaInfo,
   piece_storage: PieceStorageStrategy,
   #[serde(default, alias = "info_dict")]
   resolved_magnet_info: Option<Info>,
   bitfield: SnapshotBitfieldWire,
   block_map: PieceBlockMapWire,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PieceBlockMapWire {
   Portable(Vec<PieceBlockSnapshot>),
   VersionOne(BTreeMap<u64, BitVec<usize>>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SnapshotBitfieldWire {
   Portable(Vec<bool>),
   VersionOne(BitVec<AtomicU8>),
}

impl<'de> Deserialize<'de> for TorrentSnapshot {
   fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
      let wire = TorrentSnapshotWire::deserialize(deserializer)?;
      let (version, block_map) = match (wire.version, wire.block_map) {
         (1, PieceBlockMapWire::VersionOne(entries)) => (
            TORRENT_SNAPSHOT_VERSION,
            entries
               .into_iter()
               .map(|(piece_index, blocks)| PieceBlockSnapshot {
                  piece_index,
                  blocks: blocks.iter().by_vals().collect(),
               })
               .collect(),
         ),
         (1, PieceBlockMapWire::Portable(entries))
         | (TORRENT_SNAPSHOT_VERSION, PieceBlockMapWire::Portable(entries)) => {
            (TORRENT_SNAPSHOT_VERSION, entries)
         }
         (_, PieceBlockMapWire::VersionOne(_)) => {
            return Err(D::Error::custom(
               "map-shaped block state is only supported by snapshot version 1",
            ));
         }
         (version, PieceBlockMapWire::Portable(entries)) => (version, entries),
      };
      let resolved_magnet_info = match &wire.metainfo {
         MetaInfo::Torrent(_) if wire.version == 1 => None,
         _ => wire.resolved_magnet_info,
      };
      let bitfield = match (wire.version, wire.bitfield) {
         (1, SnapshotBitfieldWire::VersionOne(bits)) => bits.iter().by_vals().collect(),
         (1, SnapshotBitfieldWire::Portable(bits))
         | (TORRENT_SNAPSHOT_VERSION, SnapshotBitfieldWire::Portable(bits)) => bits,
         (_, SnapshotBitfieldWire::VersionOne(_)) => {
            return Err(D::Error::custom(
               "bitvec-shaped bitfields are only supported by snapshot version 1",
            ));
         }
         (_, SnapshotBitfieldWire::Portable(bits)) => bits,
      };

      Ok(Self {
         version,
         info_hash: wire.info_hash,
         state: wire.state,
         auto_start: wire.auto_start,
         sufficient_peers: wire.sufficient_peers,
         output_path: wire.output_path,
         metainfo: wire.metainfo,
         piece_storage: wire.piece_storage,
         resolved_magnet_info,
         bitfield,
         block_map,
      })
   }
}

impl TorrentSnapshot {
   /// Validates schema compatibility and structural integrity.
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
      if matches!(self.metainfo, MetaInfo::Torrent(_)) && self.resolved_magnet_info.is_some() {
         return Err(self.invalid("torrent metainfo must not duplicate its info dictionary"));
      }
      if self.output_path.is_none() {
         return Err(self.invalid("snapshot does not contain an output path"));
      }
      if let Some(info) = &self.resolved_magnet_info {
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
         let index = usize::try_from(entry.piece_index)
            .map_err(|_| self.invalid("partial piece index cannot be represented"))?;
         if index >= piece_count {
            return Err(self.invalid("partial piece index is outside the metadata piece range"));
         }
         if self.bitfield[index] {
            return Err(self.invalid("completed piece also contains partial block state"));
         }

         let Some(info) = info else {
            return Err(self.invalid("partial block state requires resolved metadata"));
         };
         let expected_blocks = piece_length(info, index)?.div_ceil(BLOCK_SIZE);
         if entry.blocks.len() != expected_blocks {
            return Err(self.invalid(format!(
               "partial piece {index} has {} blocks; expected {expected_blocks}",
               entry.blocks.len()
            )));
         }
      }

      Ok(())
   }

   #[must_use]
   pub fn resolved_info(&self) -> Option<&Info> {
      match &self.metainfo {
         MetaInfo::Torrent(torrent) => Some(&torrent.info),
         MetaInfo::MagnetUri(_) => self.resolved_magnet_info.as_ref(),
      }
   }

   fn invalid(&self, reason: impl Into<String>) -> TorrentError {
      TorrentError::InvalidSnapshot {
         reason: reason.into(),
      }
   }
}

/// Structurally validated persistence input. Only the engine restore boundary
/// can construct this wrapper.
#[derive(Debug)]
pub(crate) struct ValidatedTorrentSnapshot(TorrentSnapshot);

/// Validated actor state after the durable metainfo has been moved into the
/// actor constructor. This prevents restoration from cloning `MetaInfo` merely
/// to keep using the original snapshot container.
#[derive(Debug)]
pub(crate) struct ValidatedTorrentState {
   pub(crate) state: TorrentState,
   pub(crate) auto_start: bool,
   pub(crate) sufficient_peers: u64,
   pub(crate) resolved_magnet_info: Option<Info>,
   pub(crate) bitfield: Vec<bool>,
   pub(crate) block_map: Vec<PieceBlockSnapshot>,
}

impl TryFrom<TorrentSnapshot> for ValidatedTorrentSnapshot {
   type Error = TorrentError;

   fn try_from(snapshot: TorrentSnapshot) -> Result<Self, Self::Error> {
      snapshot.validate()?;
      Ok(Self(snapshot))
   }
}

impl ValidatedTorrentSnapshot {
   pub(crate) fn new_validated(snapshot: TorrentSnapshot) -> Self {
      Self(snapshot)
   }

   pub(crate) fn snapshot(&self) -> &TorrentSnapshot {
      &self.0
   }

   pub(crate) fn into_restore_parts(self) -> (MetaInfo, ValidatedTorrentState) {
      let TorrentSnapshot {
         state,
         auto_start,
         sufficient_peers,
         metainfo,
         resolved_magnet_info,
         bitfield,
         block_map,
         ..
      } = self.0;
      (
         metainfo,
         ValidatedTorrentState {
            state,
            auto_start,
            sufficient_peers,
            resolved_magnet_info,
            bitfield,
            block_map,
         },
      )
   }

   pub(crate) async fn reconcile_storage(
      mut self, verification: RestoreVerification,
   ) -> Result<Self, TorrentError> {
      if verification == RestoreVerification::TrustSnapshot {
         return Ok(self);
      }
      let output_path = self
         .0
         .output_path
         .as_ref()
         .ok_or_else(|| self.0.invalid("snapshot does not contain an output path"))?;
      let output_metadata =
         fs::metadata(output_path)
            .await
            .map_err(|error| TorrentError::FileIoError {
               operation: "reconcile restored output folder".to_string(),
               reason: error.to_string(),
            })?;
      if !output_metadata.is_dir() {
         return Err(TorrentError::InvalidSnapshot {
            reason: "restored output path is not a directory".to_string(),
         });
      }
      let Some(info) = self.0.resolved_info().cloned() else {
         return Ok(self);
      };
      let output_manager = FilePieceManager(self.0.output_path.clone(), Some(info.clone()));

      for index in self
         .0
         .bitfield
         .iter()
         .enumerate()
         .filter_map(|(index, complete)| complete.then_some(index))
         .collect::<Vec<_>>()
      {
         let valid = match &self.0.piece_storage {
            PieceStorageStrategy::Disk(directory) => {
               let path = directory.join(format!("{}.piece", info.pieces[index]));
               match verification {
                  RestoreVerification::Full => util::validate_piece_file(path, info.pieces[index])
                     .await
                     .is_ok(),
                  RestoreVerification::FileMetadata => {
                     fs::metadata(path).await.is_ok_and(|metadata| {
                        metadata.len()
                           >= u64::try_from(piece_length(&info, index).unwrap_or(usize::MAX))
                              .unwrap_or(u64::MAX)
                     })
                  }
                  RestoreVerification::TrustSnapshot => true,
               }
            }
            PieceStorageStrategy::InFile => match output_manager.read_piece(index).await {
               Ok(bytes) if verification == RestoreVerification::FileMetadata => {
                  bytes.len() == piece_length(&info, index)?
               }
               Ok(bytes) => util::validate_piece_bytes(&bytes, info.pieces[index]).is_ok(),
               Err(_) => false,
            },
         };
         if !valid {
            self.0.bitfield[index] = false;
         }
      }

      for entry in &mut self.0.block_map {
         let index =
            usize::try_from(entry.piece_index).map_err(|_| TorrentError::InvalidSnapshot {
               reason: "partial piece index cannot be represented".to_string(),
            })?;
         let piece_len = piece_length(&info, index)?;
         for block_index in entry
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(index, complete)| complete.then_some(index))
            .collect::<Vec<_>>()
         {
            let offset = block_index.saturating_mul(BLOCK_SIZE);
            let length = piece_len.saturating_sub(offset).min(BLOCK_SIZE);
            let exists = match &self.0.piece_storage {
               PieceStorageStrategy::Disk(directory) => {
                  let path = directory.join(format!("{}.piece", info.pieces[index]));
                  fs::metadata(path).await.is_ok_and(|metadata| {
                     metadata.len()
                        >= u64::try_from(offset.saturating_add(length)).unwrap_or(u64::MAX)
                  })
               }
               PieceStorageStrategy::InFile => output_manager
                  .read_piece_block(index, offset, length)
                  .await
                  .is_ok_and(|bytes| bytes.len() == length),
            };
            if !exists {
               entry.blocks[block_index] = false;
            }
         }
      }
      self
         .0
         .block_map
         .retain(|entry| entry.blocks.iter().any(|block| *block));

      Ok(self)
   }
}

fn piece_length(info: &Info, index: usize) -> Result<usize, TorrentError> {
   let standard =
      usize::try_from(info.piece_length).map_err(|_| TorrentError::InvalidSnapshot {
         reason: "piece length cannot be represented on this platform".to_string(),
      })?;
   if standard == 0 {
      return Err(TorrentError::InvalidSnapshot {
         reason: "piece length must be greater than zero".to_string(),
      });
   }
   let last_piece = info.piece_count().saturating_sub(1);
   if index == last_piece {
      Ok(info
         .total_length()
         .saturating_sub(standard.saturating_mul(last_piece)))
   } else {
      Ok(standard)
   }
}

#[cfg(test)]
mod tests {
   use super::*;
   use crate::{testing, torrent::TorrentState};

   async fn snapshot_with_storage(piece_storage: PieceStorageStrategy) -> TorrentSnapshot {
      let metainfo = testing::read_torrent_fixture(testing::BIG_BUCK_BUNNY_TORRENT_FILE).await;
      let info_hash = metainfo.info_hash().unwrap();
      let piece_count = match &metainfo {
         MetaInfo::Torrent(torrent) => torrent.info.piece_count(),
         MetaInfo::MagnetUri(_) => unreachable!(),
      };
      TorrentSnapshot {
         version: TORRENT_SNAPSHOT_VERSION,
         info_hash,
         state: TorrentState::Seeding,
         auto_start: false,
         sufficient_peers: 0,
         output_path: Some(std::env::temp_dir()),
         metainfo,
         piece_storage,
         resolved_magnet_info: None,
         bitfield: vec![true; piece_count],
         block_map: Vec::new(),
      }
   }

   #[tokio::test]
   async fn missing_completed_piece_storage_is_demoted_before_restore() {
      let snapshot = snapshot_with_storage(PieceStorageStrategy::InFile).await;
      let validated = ValidatedTorrentSnapshot::try_from(snapshot)
         .unwrap()
         .reconcile_storage(RestoreVerification::Full)
         .await
         .unwrap();

      assert!(
         validated
            .snapshot()
            .bitfield
            .iter()
            .all(|complete| !complete)
      );
   }

   #[tokio::test]
   async fn missing_partial_piece_data_clears_block_bits() {
      let fixture = testing::storage_fixture("snapshot-missing-partial")
         .await
         .unwrap();
      let mut snapshot =
         snapshot_with_storage(PieceStorageStrategy::Disk(fixture.path().to_path_buf())).await;
      snapshot.bitfield.fill(false);
      let info = snapshot.resolved_info().unwrap();
      let block_count = piece_length(info, 0).unwrap().div_ceil(BLOCK_SIZE);
      let mut blocks = bitvec::vec::BitVec::<usize>::repeat(false, block_count);
      blocks.set(0, true);
      snapshot.block_map.push(PieceBlockSnapshot {
         piece_index: 0,
         blocks: blocks.iter().by_vals().collect(),
      });

      let validated = ValidatedTorrentSnapshot::try_from(snapshot)
         .unwrap()
         .reconcile_storage(RestoreVerification::Full)
         .await
         .unwrap();

      assert!(validated.snapshot().block_map.is_empty());
   }

   #[tokio::test]
   async fn corrupted_completed_piece_is_demoted_before_restore() {
      let fixture = testing::storage_fixture("snapshot-corrupt-piece")
         .await
         .unwrap();
      let mut snapshot =
         snapshot_with_storage(PieceStorageStrategy::Disk(fixture.path().to_path_buf())).await;
      snapshot.bitfield.fill(false);
      snapshot.bitfield[0] = true;
      let info = snapshot.resolved_info().unwrap();
      let path = fixture.path().join(format!("{}.piece", info.pieces[0]));
      tokio::fs::write(path, vec![0_u8; piece_length(info, 0).unwrap()])
         .await
         .unwrap();

      let validated = ValidatedTorrentSnapshot::try_from(snapshot)
         .unwrap()
         .reconcile_storage(RestoreVerification::Full)
         .await
         .unwrap();

      assert!(!validated.snapshot().bitfield[0]);
   }
}
