use bitvec::vec::BitVec;
use dashmap::DashMap;

pub const BLOCK_SIZE: usize = 16 * 1024;

pub type BlockMap = DashMap<usize, BitVec<usize>>;
