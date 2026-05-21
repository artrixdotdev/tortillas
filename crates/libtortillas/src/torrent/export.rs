use std::{path::PathBuf, sync::atomic::AtomicU8};

use bitvec::vec::BitVec;
use serde::{Deserialize, Serialize};

use super::{BlockMap, PieceStorageStrategy, TorrentState};
use crate::{
   hashes::InfoHash,
   metainfo::{Info, MetaInfo},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentExport {
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
