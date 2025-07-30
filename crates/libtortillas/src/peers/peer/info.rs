use anyhow::{Error, bail};
use tracing::{trace, warn};

use crate::{hashes::InfoHash, parser::Info};

/// A helper struct for Peer. Manages and handles any metadata (informally
/// called an Info dict, as is the case here) from a Peer.
///
/// If you're unfamiliar, you can get metadata from a peer using the protocol
/// described in [BEP 0009](https://www.bittorrent.org/beps/bep_0009.html) and [BEP 0010](https://www.bittorrent.org/beps/bep_0010.html)
#[derive(Clone)]
pub struct PeerInfo {
   info_size: u64,
   info_bytes: Vec<u8>,
}

#[allow(dead_code)]
impl PeerInfo {
   pub fn new(info_size: u64, info_bytes: Vec<u8>) -> Self {
      PeerInfo {
         info_size,
         info_bytes,
      }
   }

   pub(crate) fn set_info_size(&mut self, info_size: u64) {
      self.info_size = info_size;
   }

   pub(crate) fn set_info_bytes(&mut self, info_bytes: Vec<u8>) {
      self.info_bytes = info_bytes;
   }

   /// Generates an Info dict from the current bytes in info_bytes. If the hash
   /// of the created Info dict is not the same as the inputted info hash, an
   /// error will be returned. If the hash is the same, the newly created
   /// Info will be returned.
   pub(crate) async fn generate_info_from_bytes(&self, info_hash: InfoHash) -> Result<Info, Error> {
      // We have to do this because sometimes info dicts have non-standard properties
      // that get discared by serde automatically, causing the hash to be
      // different.
      //
      // The solution? Hash the raw bytes of it instead of parsing it first.
      let real_info_hash: InfoHash = {
         use sha1::{Digest, Sha1};
         let mut hasher = Sha1::new();

         hasher.update(&self.info_bytes);
         let hash = hasher.finalize();
         hash.to_vec().try_into()?
      };

      // Put bytes into Info struct
      // The metadata should be bencoded bytes.
      trace!("Generating info dict from metadata bytes");
      let info_dict: Info = serde_bencode::from_bytes(&self.info_bytes).unwrap();

      // Validate hash of struct with given info hash
      assert_eq!(
         real_info_hash, info_hash,
         "Inputted info_hash was not the same as generated info_hash"
      );

      trace!("Info hash validation successful");

      Ok(info_dict)
   }

   /// A helper function for handling any issues with appending the new bytes to
   /// the current info_bytes
   pub(crate) fn append_to_bytes(&mut self, bytes: Vec<u8>) -> Result<(), Error> {
      let bytes_len = bytes.len();
      let current_len = self.info_bytes.len();
      let total_len = current_len + bytes_len;

      if total_len > self.info_size as usize {
         warn!(
            bytes_len,
            current_len,
            info_size = self.info_size,
            total_len,
            "Metadata bytes exceed expected size"
         );
         bail!("The inputted bytes + pre-existing bytes were longer than the metadata size")
      }
      self.info_bytes.extend_from_slice(&bytes);
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
      trace!(
         info_size = self.info_size,
         current_size, "Checking metadata completeness"
      );
      if self.info_size == 0 {
         return false;
      }
      current_size >= self.info_size as usize
   }

   pub(crate) fn info_size(&self) -> u64 {
      self.info_size
   }

   pub(crate) fn info_bytes(&self) -> Vec<u8> {
      self.info_bytes.clone()
   }

   /// Resets the PeerInfo struct.
   pub(crate) fn reset(&mut self) {
      trace!("Resetting peer info metadata");
      self.info_bytes = vec![];
      self.info_size = 0;
   }
}
