use anyhow::ensure;
use bytes::{Bytes, BytesMut};

/// Metadata assembly state owned by a connected peer actor.
///
/// If you're unfamiliar, you can get metadata from a peer using the protocol
/// described in [BEP 0009](https://www.bittorrent.org/beps/bep_0009.html) and [BEP 0010](https://www.bittorrent.org/beps/bep_0010.html)
#[derive(Default)]
pub(crate) struct PeerInfo {
   info_size: usize,
   info_bytes: BytesMut,
}

impl PeerInfo {
   pub(crate) fn set_info_size(&mut self, info_size: usize) {
      self.info_size = info_size;
   }

   /// A helper function for handling any issues with appending the new bytes to
   /// the current info_bytes
   pub(crate) fn append_to_bytes(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
      let bytes_len = bytes.len();
      let current_len = self.info_bytes.len();
      let total_len = current_len + bytes_len;

      ensure!(
         total_len <= self.info_size,
         "The inputted bytes + pre-existing bytes were longer than the metadata size"
      );

      self.info_bytes.extend_from_slice(bytes);
      Ok(())
   }

   /// Helper for checking if we have all required bytes
   ///
   /// If [info_bytes](Self::info_bytes) is 0, this function automatically
   /// returns false due to the redundancy (and incorrectness) of comparing
   /// the length of [info_bytes](Self::info_bytes) to
   /// [info_size](Self::info_size).
   pub(crate) fn have_all_bytes(&self) -> bool {
      let current_size = self.info_bytes.len();

      if self.info_size == 0 {
         return false;
      }
      current_size >= self.info_size
   }

   pub(crate) fn info_size(&self) -> usize {
      self.info_size
   }

   pub(crate) fn info_bytes(&self) -> Bytes {
      self.info_bytes.clone().freeze()
   }
}
