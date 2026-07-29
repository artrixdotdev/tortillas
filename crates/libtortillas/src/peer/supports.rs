/// A helper struct to determine what BEPs a given peer supports.
///
/// BEP support that is derived from the m dictionary in [BEP 0010](https://www.bittorrent.org/beps/bep_0010.html) is denoted by a u8, and
/// BEP support that is derived from the handshake is denoted by a boolean.
///
/// When initialized with new, every field is initialized as unsupported: a 0
/// for u8s, and a false for booleans.
#[derive(Default)]
pub(crate) struct PeerSupports {
   bep_0009: u8,
   bep_0010: bool,
}

impl PeerSupports {
   pub(crate) fn from_reserved(reserved: [u8; 8]) -> Self {
      Self {
         bep_0010: reserved[5] & 0x10 != 0,
         ..Self::default()
      }
   }
}

use super::PeerActor;

impl PeerActor {
   pub(crate) fn bep_0009_id(&self) -> u8 {
      self.supports.bep_0009
   }

   pub(crate) fn set_bep_0009(&mut self, id: u8) {
      self.supports.bep_0009 = id;
   }

   #[allow(dead_code)]
   pub(crate) fn supports_bep_0009(&self) -> bool {
      self.bep_0009_id() > 0
   }

   #[allow(dead_code)]
   pub(crate) fn supports_bep_0010(&self) -> bool {
      self.supports.bep_0010
   }
}
