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
