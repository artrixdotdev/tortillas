use std::sync::{
   Arc,
   atomic::{AtomicBool, AtomicU8, Ordering},
};

use tracing::trace;

use super::Peer;

/// A helper struct to determine what BEPs a given peer supports.
///
/// BEP support that is derived from the m dictionary in [BEP 0010](https://www.bittorrent.org/beps/bep_0010.html) is denoted by a u8, and
/// BEP support that is derived from the handshake is denoted by a boolean.
///
/// When initalized with new, every field is initialized as unsupported: a 0 for
/// u8s, and a false for booleans.
#[derive(Clone, Default)]
pub struct PeerSupports {
   pub bep_0009: Arc<AtomicU8>,
   pub bep_0010: Arc<AtomicBool>,
}

impl PeerSupports {
   pub fn new() -> Self {
      PeerSupports {
         bep_0009: Arc::new(AtomicU8::new(0)),
         bep_0010: Arc::new(AtomicBool::new(false)),
      }
   }
}

impl Peer {
   pub(crate) fn supports_bep_0009(&self) -> bool {
      self.bep_0009_id() > 0
   }

   pub(crate) fn bep_0009_id(&self) -> u8 {
      self.peer_supports.bep_0009.load(Ordering::Acquire)
   }

   pub(crate) fn supports_bep_0010(&self) -> bool {
      self.peer_supports.bep_0010.load(Ordering::Acquire)
   }

   pub(crate) fn set_bep_0009(&mut self, id: u8) {
      self.peer_supports.bep_0009.store(id, Ordering::Release);
   }

   pub(crate) fn set_bep_0010(&mut self, status: bool) {
      self.peer_supports.bep_0010.store(status, Ordering::Release);
   }

   /// Determines and updates what BEPs a given peer supports based off of the
   /// reserved bytes from the peers handshake.
   ///
   /// This function does not inherently have to be async due to the extremely
   /// light workload it takes on, but there's no reason for it not to be.
   pub(crate) async fn determine_supported(&mut self) {
      if self.reserved[5] == 0x10 {
         self.set_bep_0010(true);
         trace!("Peer supports BEP 0010 (extended messages)");
      }
   }
}
