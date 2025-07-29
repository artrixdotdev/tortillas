use tracing::trace;

use crate::peers::Peer;

/// A helper struct to determine what BEPs a given peer supports.
///
/// BEP support that is derived from the m dictionary in [BEP 0010](https://www.bittorrent.org/beps/bep_0010.html) is denoted by a u8, and
/// BEP support that is derived from the handshake is denoted by a boolean.
///
/// When initalized with new, every field is initialized as unsupported: a 0 for
/// u8s, and a false for booleans.
#[derive(Clone, Default)]
pub struct PeerSupports {
   pub bep_0009: u8,
   pub bep_0010: bool,
}

impl PeerSupports {
   pub fn new() -> Self {
      PeerSupports {
         bep_0009: 0,
         bep_0010: false,
      }
   }
}

impl Peer {
   pub fn supports_bep_0009(&self) -> bool {
      self.peer_supports.bep_0009 > 0
   }

   pub fn bep_0009_id(&self) -> u8 {
      self.peer_supports.bep_0009
   }

   pub fn supports_bep_0010(&self) -> bool {
      self.peer_supports.bep_0010
   }

   pub fn set_bep_0009(&mut self, id: u8) {
      self.peer_supports.bep_0009 = id;
   }

   pub fn set_bep_0010(&mut self, status: bool) {
      self.peer_supports.bep_0010 = status;
   }

   /// Determines and updates what BEPs a given peer supports based off of the
   /// reserved bytes from the peers handshake.
   ///
   /// This function does not inherently have to be async due to the extremely
   /// light workload it takes on, but there's no reason for it not to be.
   pub async fn determine_supported(&mut self) {
      if self.reserved[5] == 0x10 {
         self.set_bep_0010(true);
         trace!("Peer supports BEP 0010 (extended messages)");
      }
   }
}
