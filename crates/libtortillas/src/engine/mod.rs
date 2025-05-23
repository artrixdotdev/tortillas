use std::sync::Arc;

use anyhow::{Ok, Result};
use rand::random_range;
use tokio::sync::Mutex;

use crate::{
   hashes::InfoHash,
   parser::{MagnetUri, MetaInfo, TorrentFile},
   peers::{tcp::TcpProtocol, utp::UtpProtocol, Peer, TransportHandler},
   tracker::Tracker,
};

pub enum TransportProtocolS {
   Tcp(TcpProtocol),
   Utp(UtpProtocol),
}

/// Helper enum for managing the input to the [torrent()] function.
pub enum TorrentInput {
   MagnetUri(String),
   File(String),
}

/// The main engine that any outside libraries/programs should be interacting with. Automatically handles all supported protocols.
pub struct TorrentEngine {
   transport: TransportProtocolS,
   metainfo: MetaInfo,
   peers: Arc<Mutex<Vec<Peer>>>,
}

impl TorrentEngine {
   /// Parses the input to the torrent function. Can either be a Magnet URI or a valid file path a
   /// torrent file. The definition of a "valid file path" depends on where you're running the
   /// program from.
   async fn parse_input(&self, input: String) -> MetaInfo {
      match &input[0..8] {
         // Is a Magnet URI
         "magnet:" => MagnetUri::parse(input).await.unwrap(),

         // Is a torrent file
         _ => {
            let path = std::env::current_dir().unwrap().join(input);

            TorrentFile::parse(path).await.unwrap()
         }
      }
   }

   /// Returns the announce list. Flattens vec of vecs if the initial input was a torrent file (or
   /// more specifically, a path to a torrent file)
   fn get_announce_list(self) -> Vec<Tracker> {
      match self.metainfo {
         // MetaInfo::Torrent(file) => file.announce_list.unwrap(),
         MetaInfo::MagnetUri(magnet) => magnet.announce_list.unwrap(),
         MetaInfo::Torrent(file) => {
            // Vec of vecs
            let flattened = file.announce_list.unwrap().into_iter().flatten();

            // No longer a vec of vecs
            flattened.collect()
         }
      }
   }

   /// Returns the info hash
   fn get_info_hash(&self) -> InfoHash {
      match &self.metainfo {
         MetaInfo::MagnetUri(magnet) => magnet.info_hash().unwrap(),
         MetaInfo::Torrent(file) => file.info.hash().unwrap(),
      }
   }

   // TODO: Implement stream_peers and concurrently update self.peers
   /// Contacts all given trackers for a list of peers
   async fn get_all_peers(&self, announce_list: Vec<Tracker>) {
      for tracker in announce_list.iter() {
         tracker.get_peers(self.get_info_hash()).await.unwrap();
      }
   }

   /// The full torrenting process, summarized in a single function. As of 5/23/25, the return
   /// value of this function is temporary.
   pub async fn torrent(mut self, input: String) -> Result<()> {
      self.metainfo = self.parse_input(input).await;

      let info_hash = self.get_info_hash();

      let announce_list: Vec<Tracker> = self.get_announce_list();
      let port: u16 = random_range(1024..65535);

      // TODO
      // Call get_all_peers

      // Handle edge cases (ex. no peers)

      // Create single instantce of transport

      // Handshake with all given peers

      // Wait for bitfield from each peer

      // TEMPORARY
      Ok(())
   }
}
