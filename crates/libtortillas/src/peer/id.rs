use std::{fmt, str::FromStr};

use rand::{Rng, distr::Alphanumeric};

use crate::hashes::Hash;

pub type Id = [u8; 20];
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Client information structure
#[derive(Debug, Clone)]
pub struct ClientInfo {
   pub prefix: &'static [u8],
   pub name: &'static str,
   pub format: PeerIdFormat,
}

/// Different peer ID formats as defined in BEP 20
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerIdFormat {
   /// Azureus-style: -XX####-
   Azureus,
   /// Shadow/BitTornado-style: Single char + version + dashes
   Shadow,
   /// Mainline-style: M + version + dashes
   Mainline,
   /// BitComet legacy: exbc + version bytes
   BitCometLegacy,
   /// XBT Client: XBT + version digits
   Xbt,
   /// Opera: OP + build number
   Opera,
   /// MLdonkey: -ML + dotted version
   MlDonkey,
   /// Bits on Wheels: -BOW + version
   BitsOnWheels,
   /// Queen Bee: Q1- + version
   QueenBee,
   /// BitTyrant: AZ2500BT + random
   BitTyrant,
   /// TorrenTopia: 346- (pretending to be Mainline)
   TorrenTopia,
   /// BitSpirit: null bytes + BS
   BitSpirit,
   /// Rufus: version + RS
   Rufus,
   /// G3 Torrent: -G3 + nickname
   G3Torrent,
   /// FlashGet: FG + version
   FlashGet,
   /// AllPeers: AP + version + hash
   AllPeers,
}

macro_rules! define_clients {
   ($(
      $variant:ident => {
         prefix: $prefix:expr,
         name: $name:expr,
         format: $format:expr
      }
   ),* $(,)?) => {
      #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
      pub enum PeerId {
         $($variant(Id),)*
         Unknown(Id),
      }

      impl PeerId {
         const CLIENT_INFO: &'static [(&'static str, ClientInfo)] = &[
            $(
               (stringify!($variant), ClientInfo {
                  prefix: $prefix,
                  name: $name,
                  format: $format,
               }),
            )*
         ];

         pub fn client_info(&self) -> &'static ClientInfo {
            match self {
               $(Self::$variant(_) => &Self::CLIENT_INFO.iter()
                  .find(|(name, _)| *name == stringify!($variant))
                  .unwrap().1,)*
               Self::Unknown(_) => &ClientInfo {
                  prefix: b"??",
                  name: "Unknown",
                  format: PeerIdFormat::Azureus,
               },
            }
         }

         pub fn client_name(&self) -> &'static str {
            self.client_info().name
         }

         pub fn prefix(&self) -> &'static [u8] {
            self.client_info().prefix
         }

         pub fn format(&self) -> PeerIdFormat {
            self.client_info().format
         }

         pub fn id(&self) -> &Id {
            match self {
               $(Self::$variant(id) |)* Self::Unknown(id) => id,
            }
         }

         pub fn version(&self) -> Option<String> {
            let id = self.id();

            match self.format() {
               PeerIdFormat::Azureus => {
                  if id.len() >= 8 && id[0] == b'-' && id[7] == b'-' {
                     Some(format!("{}.{}.{}{}",
                        id[3] as char, id[4] as char, id[5] as char, id[6] as char))
                  } else {
                     None
                  }
               },
               PeerIdFormat::Shadow => {
                  if id.len() >= 6 {
                     let version_bytes = &id[1..6];
                     let version_str = String::from_utf8_lossy(version_bytes)
                        .trim_end_matches('-').to_string();
                     if !version_str.is_empty() {
                        Some(version_str)
                     } else {
                        None
                     }
                  } else {
                     None
                  }
               },
               PeerIdFormat::Mainline => {
                  if id.len() > 1 && id[0] == b'M' {
                     let version_part = String::from_utf8_lossy(&id[1..])
                        .trim_end_matches('-').to_string();
                     if !version_part.is_empty() {
                        let version_part = version_part.split('-').collect::<Vec<&str>>();
                        let major = version_part[0];
                        let minor = version_part[1];
                        let patch = version_part[2];
                        Some(format!("{}.{}.{}", major, minor, patch))
                     } else {
                        None
                     }
                  } else {
                     None
                  }
               },
               PeerIdFormat::BitCometLegacy => {
                  if id.len() >= 6 && &id[0..4] == b"exbc" {
                     Some(format!("{}.{:02}", id[4], id[5]))
                  } else {
                     None
                  }
               },
               PeerIdFormat::Xbt => {
                  if id.len() >= 6 && &id[0..3] == b"XBT" {
                     Some(format!("{}.{}.{}",
                        id[3] as char, id[4] as char, id[5] as char))
                  } else {
                     None
                  }
               },
               PeerIdFormat::Opera => {
                  if id.len() >= 6 && &id[0..2] == b"OP" {
                     let build = String::from_utf8_lossy(&id[2..6]);
                     Some(format!("build {}", build))
                  } else {
                     None
                  }
               },
               _ => None,
            }
         }
      }

      impl From<Id> for PeerId {
         fn from(id: Id) -> Self {
            // Check Azureus-style first (most common)
            if id.len() >= 8 && id[0] == b'-' && id[7] == b'-' {
               let prefix = &id[1..3];
               $(
                  if $format == PeerIdFormat::Azureus && prefix == $prefix {
                     return Self::$variant(id);
                  }
               )*
            }

            // Check other formats
            $(
               if Self::matches_format(&id, $prefix, $format) {
                  return Self::$variant(id);
               }
            )*

            Self::Unknown(id)
         }
      }

      impl PeerId {
         fn matches_format(id: &Id, prefix: &[u8], format: PeerIdFormat) -> bool {
            match format {
               PeerIdFormat::Azureus => {
                  id.len() >= 8 && id[0] == b'-' && id[7] == b'-' && &id[1..3] == prefix
               },
               PeerIdFormat::Shadow => {
                  id.len() >= 6 && id.len() >= prefix.len() && &id[0..prefix.len()] == prefix
               },
               PeerIdFormat::Mainline => {
                  id.len() > 0 && &id[0..prefix.len()] == prefix
               },
               PeerIdFormat::BitCometLegacy => {
                  id.len() >= 4 && &id[0..prefix.len()] == prefix
               },
               PeerIdFormat::Xbt => {
                  id.len() >= 6 && &id[0..prefix.len()] == prefix
               },
               PeerIdFormat::Opera => {
                  id.len() >= 6 && &id[0..prefix.len()] == prefix
               },
               PeerIdFormat::MlDonkey => {
                  id.len() >= 3 && &id[0..prefix.len()] == prefix
               },
               PeerIdFormat::BitsOnWheels => {
                  id.len() >= 4 && &id[0..prefix.len()] == prefix
               },
               PeerIdFormat::QueenBee => {
                  id.len() >= 3 && &id[0..prefix.len()] == prefix
               },
               PeerIdFormat::BitTyrant => {
                  id.len() >= 9 && &id[0..prefix.len()] == prefix
               },
               PeerIdFormat::TorrenTopia => {
                  id.len() >= 4 && &id[0..prefix.len()] == prefix
               },
               PeerIdFormat::BitSpirit => {
                  id.len() >= 4 && id[0] == 0 && id[1] == 3 && id[2] == b'B' && id[3] == b'S'
               },
               PeerIdFormat::Rufus => {
                  id.len() >= 4 && &id[2..4] == b"RS"
               },
               PeerIdFormat::G3Torrent => {
                  id.len() >= 3 && &id[0..prefix.len()] == prefix
               },
               PeerIdFormat::FlashGet | PeerIdFormat::AllPeers => {
                  id.len() >= 2 && &id[0..prefix.len()] == prefix
               },

            }
         }
      }
   };
}

// Define all clients in one place
define_clients! {
   // Azureus-style clients
   Ares => { prefix: b"AG", name: "Ares", format: PeerIdFormat::Azureus },
   AresV2 => { prefix: b"A~", name: "AresV2", format: PeerIdFormat::Azureus },
   Arctic => { prefix: b"AR", name: "Arctic", format: PeerIdFormat::Azureus },
   Avicora => { prefix: b"AV", name: "Avicora", format: PeerIdFormat::Azureus },
   BitPump => { prefix: b"AX", name: "BitPump", format: PeerIdFormat::Azureus },
   Azureus => { prefix: b"AZ", name: "Azureus", format: PeerIdFormat::Azureus },
   BitBuddy => { prefix: b"BB", name: "BitBuddy", format: PeerIdFormat::Azureus },
   BitComet => { prefix: b"BC", name: "BitComet", format: PeerIdFormat::Azureus },
   Bitflu => { prefix: b"BF", name: "Bitflu", format: PeerIdFormat::Azureus },
   Btg => { prefix: b"BG", name: "BTG", format: PeerIdFormat::Azureus },
   BitRocket => { prefix: b"BR", name: "BitRocket", format: PeerIdFormat::Azureus },
   BtSlave => { prefix: b"BS", name: "BTSlave", format: PeerIdFormat::Azureus },
   BittorrentX => { prefix: b"BX", name: "BitTorrent X", format: PeerIdFormat::Azureus },
   EnhancedCTorrent => { prefix: b"CD", name: "Enhanced CTorrent", format: PeerIdFormat::Azureus },
   Ctorrent => { prefix: b"CT", name: "CTorrent", format: PeerIdFormat::Azureus },
   Deluge => { prefix: b"DE", name: "Deluge", format: PeerIdFormat::Azureus },
   PropagateData => { prefix: b"DP", name: "Propagate Data Client", format: PeerIdFormat::Azureus },
   EBit => { prefix: b"EB", name: "EBit", format: PeerIdFormat::Azureus },
   ElectricSheep => { prefix: b"ES", name: "Electric Sheep", format: PeerIdFormat::Azureus },
   FoxTorrent => { prefix: b"FT", name: "FoxTorrent", format: PeerIdFormat::Azureus },
   FrostWire => { prefix: b"FW", name: "FrostWire", format: PeerIdFormat::Azureus },
   Freebox => { prefix: b"FX", name: "Freebox BitTorrent", format: PeerIdFormat::Azureus },
   GsTorrent => { prefix: b"GS", name: "GSTorrent", format: PeerIdFormat::Azureus },
   Halite => { prefix: b"HL", name: "Halite", format: PeerIdFormat::Azureus },
   Hydranode => { prefix: b"HN", name: "Hydranode", format: PeerIdFormat::Azureus },
   KGet => { prefix: b"KG", name: "KGet", format: PeerIdFormat::Azureus },
   KTorrent => { prefix: b"KT", name: "KTorrent", format: PeerIdFormat::Azureus },
   LhAbc => { prefix: b"LH", name: "LH-ABC", format: PeerIdFormat::Azureus },
   Lphant => { prefix: b"LP", name: "Lphant", format: PeerIdFormat::Azureus },
   Libtorrent => { prefix: b"LT", name: "libtorrent", format: PeerIdFormat::Azureus },
   LibTorrent => { prefix: b"lt", name: "libTorrent", format: PeerIdFormat::Azureus },
   LimeWire => { prefix: b"LW", name: "LimeWire", format: PeerIdFormat::Azureus },
   MonoTorrent => { prefix: b"MO", name: "MonoTorrent", format: PeerIdFormat::Azureus },
   MooPolice => { prefix: b"MP", name: "MooPolice", format: PeerIdFormat::Azureus },
   Miro => { prefix: b"MR", name: "Miro", format: PeerIdFormat::Azureus },
   MoonlightTorrent => { prefix: b"MT", name: "MoonlightTorrent", format: PeerIdFormat::Azureus },
   NetTransport => { prefix: b"NX", name: "Net Transport", format: PeerIdFormat::Azureus },
   Pando => { prefix: b"PD", name: "Pando", format: PeerIdFormat::Azureus },
   Qbittorrent => { prefix: b"qB", name: "qBittorrent", format: PeerIdFormat::Azureus },
   QqDownload => { prefix: b"QD", name: "QQDownload", format: PeerIdFormat::Azureus },
   QtExample => { prefix: b"QT", name: "Qt 4 Torrent example", format: PeerIdFormat::Azureus },
   Retriever => { prefix: b"RT", name: "Retriever", format: PeerIdFormat::Azureus },
   ShareazaAlpha => { prefix: b"S~", name: "Shareaza (alpha/beta)", format: PeerIdFormat::Azureus },
   Swiftbit => { prefix: b"SB", name: "Swiftbit", format: PeerIdFormat::Azureus },
   SwarmScope => { prefix: b"SS", name: "SwarmScope", format: PeerIdFormat::Azureus },
   SymTorrent => { prefix: b"ST", name: "SymTorrent", format: PeerIdFormat::Azureus },
   Sharktorrent => { prefix: b"st", name: "sharktorrent", format: PeerIdFormat::Azureus },
   Shareaza => { prefix: b"SZ", name: "Shareaza", format: PeerIdFormat::Azureus },
   Tortillas => { prefix: b"TO", name: "Tortillas", format: PeerIdFormat::Azureus },
   TorrentDotNet => { prefix: b"TN", name: "TorrentDotNET", format: PeerIdFormat::Azureus },
   Transmission => { prefix: b"TR", name: "Transmission", format: PeerIdFormat::Azureus },
   Torrentstorm => { prefix: b"TS", name: "Torrentstorm", format: PeerIdFormat::Azureus },
   TuoTu => { prefix: b"TT", name: "TuoTu", format: PeerIdFormat::Azureus },
   ULeecher => { prefix: b"UL", name: "uLeecher!", format: PeerIdFormat::Azureus },
   Utorrent => { prefix: b"UT", name: "µTorrent", format: PeerIdFormat::Azureus },
   UtorrentWeb => { prefix: b"UW", name: "µTorrent Web", format: PeerIdFormat::Azureus },
   Vagaa => { prefix: b"VG", name: "Vagaa", format: PeerIdFormat::Azureus },
   WebTorrentDesktop => { prefix: b"WD", name: "WebTorrent Desktop", format: PeerIdFormat::Azureus },
   BitLet => { prefix: b"WT", name: "BitLet", format: PeerIdFormat::Azureus },
   WebTorrent => { prefix: b"WW", name: "WebTorrent", format: PeerIdFormat::Azureus },
   FireTorrent => { prefix: b"WY", name: "FireTorrent", format: PeerIdFormat::Azureus },
   Xunlei => { prefix: b"XL", name: "Xunlei", format: PeerIdFormat::Azureus },
   XanTorrent => { prefix: b"XT", name: "XanTorrent", format: PeerIdFormat::Azureus },
   Xtorrent => { prefix: b"XX", name: "Xtorrent", format: PeerIdFormat::Azureus },
   ZipTorrent => { prefix: b"ZT", name: "ZipTorrent", format: PeerIdFormat::Azureus },

   // Shadow/BitTornado-style clients
   Abc => { prefix: b"A", name: "ABC", format: PeerIdFormat::Shadow },
   OspreyPermaseed => { prefix: b"O", name: "Osprey Permaseed", format: PeerIdFormat::Shadow },
   BtQueue => { prefix: b"Q", name: "BTQueue", format: PeerIdFormat::Shadow },
   Tribler => { prefix: b"R", name: "Tribler", format: PeerIdFormat::Shadow },
   ShadowClient => { prefix: b"S", name: "Shadow's client", format: PeerIdFormat::Shadow },
   BitTornado => { prefix: b"T", name: "BitTornado", format: PeerIdFormat::Shadow },
   UpnpNatBitTorrent => { prefix: b"U", name: "UPnP NAT Bit Torrent", format: PeerIdFormat::Shadow },

   // Mainline-style
   MainlineBitTorrent => { prefix: b"M", name: "Mainline BitTorrent", format: PeerIdFormat::Mainline },

   // Other formats
   BitCometLegacy => { prefix: b"exbc", name: "BitComet (legacy)", format: PeerIdFormat::BitCometLegacy },
   XbtClient => { prefix: b"XBT", name: "XBT Client", format: PeerIdFormat::Xbt },
   Opera => { prefix: b"OP", name: "Opera", format: PeerIdFormat::Opera },
   MlDonkey => { prefix: b"-ML", name: "MLdonkey", format: PeerIdFormat::MlDonkey },
   BitsOnWheels => { prefix: b"-BOW", name: "Bits on Wheels", format: PeerIdFormat::BitsOnWheels },
   QueenBee => { prefix: b"Q1-", name: "Queen Bee", format: PeerIdFormat::QueenBee },
   BitTyrant => { prefix: b"AZ2500BT", name: "BitTyrant", format: PeerIdFormat::BitTyrant },
   TorrenTopia => { prefix: b"346-", name: "TorrenTopia", format: PeerIdFormat::TorrenTopia },
   BitSpirit => { prefix: b"", name: "BitSpirit", format: PeerIdFormat::BitSpirit },
   Rufus => { prefix: b"RS", name: "Rufus", format: PeerIdFormat::Rufus },
   G3Torrent => { prefix: b"-G3", name: "G3 Torrent", format: PeerIdFormat::G3Torrent },
   FlashGet => { prefix: b"FG", name: "FlashGet", format: PeerIdFormat::FlashGet },
   AllPeers => { prefix: b"AP", name: "AllPeers", format: PeerIdFormat::AllPeers },
}

impl Default for PeerId {
   /// Creates a new [PeerId] for the tortillas client using the
   /// [Azureus](PeerIdFormat::Azureus) format
   fn default() -> Self {
      // Fill entire array with random alphanumeric bytes
      let mut id = [0u8; 20];
      rand::rng()
         .sample_iter(&Alphanumeric)
         .take(20)
         .enumerate()
         .for_each(|(i, byte)| id[i] = byte);

      // Overwrite the beginning with our identifier
      id[0] = b'-';
      id[1..3].copy_from_slice(b"TO");

      let version = String::from(VERSION);
      let version = version.replace(".", "");
      let version = if version.len() < 4 {
         version + "0"
      } else {
         version
      };
      let version = version.as_bytes();

      let version_end = std::cmp::min(3 + version.len(), 20);
      id[3..version_end].copy_from_slice(&version[..version_end - 3]);
      id[version_end] = b'-';

      Self::Tortillas(id)
   }
}

impl PeerId {
   /// See [PeerId::default]
   pub fn new() -> Self {
      Self::default()
   }

   pub fn as_bytes(&self) -> &Id {
      self.id()
   }
}

impl From<Hash<20>> for PeerId {
   fn from(hash: Hash<20>) -> Self {
      Self::from(*hash.as_bytes())
   }
}

impl fmt::Display for PeerId {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      if let Some(version) = self.version() {
         write!(
            f,
            "{} {} ({})",
            self.client_name(),
            version,
            hex::encode(self.id())
         )
      } else {
         write!(f, "{} ({})", self.client_name(), hex::encode(self.id()))
      }
   }
}

impl FromStr for PeerId {
   type Err = std::array::TryFromSliceError;

   fn from_str(s: &str) -> Result<Self, Self::Err> {
      let id: Id = s.as_bytes().try_into()?;
      Ok(Self::from(id))
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn test_parse_azureus_style_peer_id() {
      let id = *b"-UT1234-abcdefghijkn";
      let peer = PeerId::from(id);
      assert_eq!(peer.client_name(), "µTorrent");
      assert_eq!(peer.version(), Some("1.2.34".to_string()));
   }

   #[test]
   fn test_parse_tortillas_peer_id() {
      let peer = PeerId::new();
      let correct_version = if VERSION.len() <= 5 {
         VERSION.to_owned() + "0"
      } else {
         VERSION.to_owned()
      };

      assert_eq!(peer.client_name(), "Tortillas");
      assert_eq!(peer.version(), Some(correct_version));
   }
   #[test]
   fn test_parse_webtorrent_peer_id() {
      let id = *b"-WW1234-abcdefghijkn";
      let peer = PeerId::from(id);
      assert_eq!(peer.client_name(), "WebTorrent");
      assert_eq!(peer.version(), Some("1.2.34".to_string()));
   }

   #[test]
   fn test_parse_shadow_style_peer_id() {
      let id = *b"T58B-----abcdefghijk";
      let peer = PeerId::from(id);
      assert_eq!(peer.client_name(), "BitTornado");
      assert_eq!(peer.version(), Some("58B".to_string()));
   }

   #[test]
   fn test_parse_mainline_peer_id() {
      let id = *b"M4-3-6--abcdefghijkl";
      let peer = PeerId::from(id);
      assert_eq!(peer.client_name(), "Mainline BitTorrent");
      assert_eq!(peer.version(), Some("4.3.6".to_string()));
   }

   #[test]
   fn test_parse_bitcomet_legacy_peer_id() {
      let id = *b"exbc\x01\x02abcdefghijklmn";
      let peer = PeerId::from(id);
      assert_eq!(peer.client_name(), "BitComet (legacy)");
      assert_eq!(peer.version(), Some("1.02".to_string()));
   }

   #[test]
   fn test_parse_unknown_peer_id() {
      let id = *b"sdfsfdsfabcdefghijkl";
      let peer = PeerId::from(id);
      assert_eq!(peer.client_name(), "Unknown");
      assert_eq!(peer.version(), None);
   }

   #[test]
   fn test_client_info_access() {
      let id = *b"-UT1234-abcdefghijkn";
      let peer = PeerId::from(id);
      let info = peer.client_info();
      assert_eq!(info.name, "µTorrent");
      assert_eq!(info.prefix, b"UT");
      assert_eq!(info.format, PeerIdFormat::Azureus);
   }
}
