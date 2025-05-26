use std::sync::Arc;

use anyhow::{Ok, Result};
use rand::random_range;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, trace};

use crate::{
   errors::TorrentEngineError,
   hashes::{Hash, InfoHash},
   parser::{MagnetUri, MetaInfo, TorrentFile},
   peers::{tcp::TcpProtocol, utp::UtpProtocol, Peer, TransportHandler, TransportProtocol},
   tracker::Tracker,
};

/// The name of this enum is intentionally awkward as to highlight the difference between [TransportProtocol] and [TransportProtocolS]
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
///
/// TorrentEngine only supports torrenting a single file at a time (at the moment).
pub struct TorrentEngine<T: TransportProtocol> {
   handler: TransportHandler<T>,
   metainfo: MetaInfo,
   peers: Arc<Mutex<Vec<Peer>>>,
}

impl<T> TorrentEngine<T>
where
   T: TransportProtocol + 'static,
{
   async fn new(input: String, protocol: T) -> Self {
      trace!("Parsing input to torrent()");
      let metainfo = TorrentEngine::<T>::parse_input(input).await;

      trace!("Creating new transport handler");
      let peer_id = Hash::new(rand::random::<[u8; 20]>());
      let handler = TransportHandler::new(
         protocol,
         Arc::new(peer_id),
         Arc::new(metainfo.info_hash().unwrap()),
      );

      TorrentEngine {
         handler,
         metainfo,
         peers: Arc::new(Mutex::new(vec![])),
      }
   }

   /// Parses the input to the torrent function. Can either be a Magnet URI or a valid file path a
   /// torrent file. The definition of a "valid file path" depends on where you're running the
   /// program from.
   pub async fn parse_input(input: String) -> MetaInfo {
      // Is this accurate 100% of the time? Hopefully.
      match &input[0..7] {
         // Is a Magnet URI
         "magnet:" => {
            trace!("Input was Magnet URI");
            MagnetUri::parse(input).await.unwrap()
         }

         // Is a torrent file
         _ => {
            trace!("Input was Torrent file");
            let path = std::env::current_dir().unwrap().join(input);

            TorrentFile::parse(path).await.unwrap()
         }
      }
   }

   /// Returns the announce list. Flattens vec of vecs if the initial input was a torrent file (or
   /// more specifically, a path to a torrent file)
   fn get_announce_list(&self) -> Vec<Tracker> {
      // Is cloning the announce_list efficient? Probably not. But hey, it's Rust.
      match &self.metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let announce_list = magnet.announce_list.clone().unwrap();
            trace!(
               "Succesfully cloned and unwrapped announce_list: {:?}",
               announce_list
            );
            announce_list
         }
         MetaInfo::Torrent(file) => {
            // Vec of vecs
            let flattened = file.announce_list.clone().unwrap().into_iter().flatten();

            // No longer a vec of vecs
            let res = flattened.collect();

            trace!(
               "Succesfully cloned, unwrapped, and flattened announce_list: {:?}",
               res
            );
            res
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

   /// FIXME: Do trackers send duplicate peers? How do we handle that?
   /// Contacts all given trackers for a list of peers
   async fn get_all_peers(
      &mut self,
      announce_list: &[Tracker],
   ) -> Result<mpsc::Receiver<Vec<Peer>>> {
      let (tx, rx) = mpsc::channel(100);
      // Get an rx for each tracker
      let mut rx_list = vec![];

      trace!("Making initial requests to trackers");
      let info_hash = self.get_info_hash();
      for tracker in announce_list.iter() {
         rx_list.push(tracker.stream_peers(info_hash).await.unwrap());
      }

      // Repeatedly gather data from each rx and update self.peers as new peers are added
      // rx
      trace!("Spawning task to handle output from initial requests");
      tokio::spawn(async move {
         loop {
            for rx in rx_list.iter_mut() {
               // Gather any peers that a tracker returned
               let mut cur_peers = vec![];
               while let Some(peer_vec) = rx.recv().await {
                  cur_peers.extend_from_slice(&peer_vec);
               }

               // Update self.peers
               tx.send(cur_peers)
                  .await
                  .map_err(|e| error!("Error when sending peers back to torrent(): {}", e))
                  .unwrap();
            }
         }
      });

      Ok(rx)
   }

   /// The full torrenting process, summarized in a single function. As of 5/23/25, the return
   /// value of this function is temporary.
   ///
   /// TODO: This function will likely return a torrented file, or a path to a locally torrented file.
   pub async fn torrent(&mut self) -> anyhow::Result<(), anyhow::Error> {
      let info_hash = self.get_info_hash();

      let announce_list: Vec<Tracker> = self.get_announce_list();
      let port: u16 = random_range(1024..65535);

      // Call get_all_peers
      let mut rx = self.get_all_peers(&announce_list).await.unwrap();

      // FIXME: Will this run infinitely?
      // Get initial peers
      trace!("Getting initial peers...");
      while let Some(res) = rx.recv().await {
         self.peers.lock().await.extend(res);
      }

      // Handle edge cases (ex. no peers)
      if self.peers.lock().await.is_empty() {
         trace!("No peers were provided by trackers.");
         return Err(TorrentEngineError::InsufficientPeers.into());
      };

      // Create single instance of transport

      // Handshake with all given peers

      // Wait for bitfield from each peer

      // Receive pieces

      // TEMPORARY
      Ok(())
   }
}

#[cfg(test)]
mod tests {
   use std::net::SocketAddr;

   use rand::random_range;
   use tracing_test::traced_test;

   use crate::{engine::TorrentEngine, peers::tcp::TcpProtocol};

   // THIS TEST IS NOT COMPLETE!!! (DELETEME when torrent() is completed)
   // Until torrent() is fully implemented, this test is not complete.
   // The purpose of this test at this point in time is to ensure that torrent() works to the expected point.
   #[tokio::test]
   #[traced_test]
   async fn test_torrent_with_magnet_uri_over_tcp() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/zenshuu.txt");
      let magnet_uri = tokio::fs::read_to_string(path).await.unwrap();

      let client_port: u16 = random_range(20001..30000);
      let client_addr = SocketAddr::from(([0, 0, 0, 0], client_port));
      let protocol = TcpProtocol::new(Some(client_addr)).await;

      let mut engine = TorrentEngine::new(magnet_uri, protocol).await;
      engine.torrent().await.unwrap();
   }
}
