use std::{collections::HashSet, net::SocketAddr, sync::Arc, thread::sleep, time::Duration};

use anyhow::{Error, Ok, Result};
use rand::random_range;
use tokio::{
   sync::{mpsc, Mutex},
   task::JoinSet,
};
use tracing::{error, trace};

use crate::{
   errors::TorrentEngineError,
   hashes::{Hash, InfoHash},
   parser::{MagnetUri, MetaInfo, TorrentFile},
   peers::{
      messages::PeerMessages,
      tcp::TcpProtocol,
      transport_messages::{TransportCommand, TransportResponse},
      utp::UtpProtocol,
      Peer, Transport, TransportHandler,
   },
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
/// However, it should be noted that it does support all supported protocols on initialization. In other words, both
/// tcp_handler and utp_handler are available directly after TorrentEngine::new() is called.
///
/// It should be noted that TorrentEngine does not seed files at the moment. In other words,
/// TorrentEngine is a leecher. The ability to seed files will be added in a future commit/issue/pull request.
pub struct TorrentEngine {
   tcp_handler: Arc<Mutex<TransportHandler<TcpProtocol>>>,
   utp_handler: Arc<Mutex<TransportHandler<UtpProtocol>>>,
   metainfo: MetaInfo,
   peers: Arc<Mutex<HashSet<Peer>>>,
}

impl TorrentEngine {
   async fn new(input: String) -> Self {
      trace!("Parsing input to torrent()");
      let metainfo = TorrentEngine::parse_input(input).await;

      trace!("Creating new transport handler");

      // For TCP
      let tcp_peer_id = Hash::new(rand::random::<[u8; 20]>());
      let tcp_client_port: u16 = random_range(20001..30000);
      let tcp_addr = SocketAddr::from(([0, 0, 0, 0], tcp_client_port));
      let tcp_protocol = TcpProtocol::new(Some(tcp_addr)).await;
      let tcp_handler = TransportHandler::new(
         tcp_protocol,
         Arc::new(tcp_peer_id),
         Arc::new(metainfo.info_hash().unwrap()),
      );

      // For uTP
      let utp_peer_id = Hash::new(rand::random::<[u8; 20]>());
      let utp_client_port: u16 = random_range(20001..30000);
      let utp_addr = SocketAddr::from(([0, 0, 0, 0], utp_client_port));
      let utp_protocol = UtpProtocol::new(Some(utp_addr)).await;
      let utp_handler = TransportHandler::new(
         utp_protocol,
         Arc::new(utp_peer_id),
         Arc::new(metainfo.info_hash().unwrap()),
      );

      TorrentEngine {
         tcp_handler: Arc::new(Mutex::new(tcp_handler)),
         utp_handler: Arc::new(Mutex::new(utp_handler)),
         metainfo,
         peers: Arc::new(Mutex::new(HashSet::new())),
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

   /// Contacts all given trackers for a list of peers
   async fn get_all_peers(
      self: Arc<Self>,
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

   /// Handshake with all initially given peers
   /// If this code seems confusing, refer to test_utp_peer_handshake.
   ///
   /// At the moment, this function returns nothing. This is due to the fact that utp_handler will maintain a hashmap of all successfully connected peers, thus there is nothing to return. The key for
   /// the hashmap is the socket address of the peer. See PeerKey.
   ///
   /// We allow the user of this function to pick and choose which peers they want to connect to
   /// (likely only new peers).
   pub async fn connect_to_peers(self: Arc<Self>, peers: Vec<Peer>) -> Result<(), anyhow::Error> {
      let utp_handler_guard = self.utp_handler.lock().await;
      let handler_tx = utp_handler_guard.sender();
      let mut join_set = JoinSet::new();

      trace!("Locked peers. Beginning handshake process.");

      // FIXME: How are we supposed to determine whether a peer is operating on TCP or uTP?
      // Until we figure this out, let's just assume that peers are operating over uTP.
      //
      {
         for peer in peers {
            let tx = handler_tx.clone();
            join_set.spawn(async move {
               let cmd = TransportCommand::Connect { peer: peer.clone() };

               tx.send(cmd)
                  .await
                  .map_err(|e| {
                     error!(
                        "An error occured when sending the result back to torrent(): {}",
                        e
                     );
                  })
                  .unwrap();
            });
         }
      }
      let (tx, _) = mpsc::channel(100);

      trace!("Gathering handshakes");

      let me = Arc::clone(&self);
      tokio::spawn(async move {
         me.utp_handler
            .lock()
            .await
            .handle_commands(tx)
            .await
            .unwrap();
      });

      // Handle all connections on a separate thread and "immediately" return.
      tokio::spawn(async move {
         join_set.join_all().await;
      });

      // (The "immediate" return)
      Ok(())
   }

   /// Spawns a loop to wait for peer messages from each peer.
   pub async fn receive_peer_messages(self: Arc<Self>, peers: Vec<Peer>) -> Result<(), Error> {
      let mut join_set = JoinSet::new();

      {
         let handler_tx = self.utp_handler.lock().await.sender();
         for peer in peers {
            let tx = handler_tx.clone();
            let peer_key = peer.socket_addr();

            join_set.spawn(async move {
               let cmd = TransportCommand::Receive { peer_key };

               tx.send(cmd)
                  .await
                  .map_err(|e| {
                     error!(
                        "An error occured when sending the result back to torrent(): {}",
                        e
                     );
                  })
                  .unwrap();
            });
         }
      }

      let (tx, mut rx) = mpsc::channel(100);

      trace!("Gathering bitfields");

      let me = Arc::clone(&self);
      tokio::spawn(async move {
         me.utp_handler
            .lock()
            .await
            .handle_commands(tx)
            .await
            .unwrap();
      });

      tokio::spawn(async move {
         join_set.join_all().await;
      });

      // Receive AND handle results
      while let Some(res) = rx.recv().await {
         match res.unwrap() {
            TransportResponse::Receive { message, peer_key } => {
               // Is this scope necessary? Probably not. Better safe than sorry though! (The
               // scope was initially added for the sake of ensuring self.utp_handler isn't
               // locked for an obscene amount of time).
               {
                  let mut peer = self
                     .utp_handler
                     .lock()
                     .await
                     .get_peer(peer_key)
                     .await
                     .unwrap();

                  match message {
                     PeerMessages::Bitfield(bitfield) => {
                        // Set peer's bitfield (AKA pieces field)
                        //
                        // NOTE: Will this actually assign to the peer in utp_handler? It
                        // should.
                        let pieces = bitfield.iter().by_vals().collect();
                        peer.pieces = pieces;
                     }
                     _ => {
                        error!(
                           "Couldn't handle given peer message for peer {}: {:?}",
                           peer_key, message
                        );
                     }
                  }
               }
            }
            _ => {
               error!("Received a message other than receive.");
            }
         }
      }

      Ok(())
   }

   /// The full torrenting process, summarized in a single function. As of 5/23/25, the return
   /// value of this function is temporary.
   ///
   /// The general flow of this function is as follows:
   /// - Get initial peers from trackers
   /// - Go through standard protocol for each peer (ex. handshake, then wait for bitfield, etc.).
   /// - Pieces will be maintained in the TorrentEngine struct
   /// - Get new peers from each tracker
   /// - Remove any duplicate peers
   /// - Repeat
   ///
   /// This also makes seeding very easy -- when a peer asks for a piece, just send them
   /// self.pieces at whatever index they asked for.
   ///
   /// TODO: This function will likely return a torrented file, or a path to a locally torrented file.
   pub async fn torrent(self: Arc<Self>) -> anyhow::Result<(), Error> {
      // A list of peers that we've already seen
      let mut peers_in_action = HashSet::new();
      loop {
         let me = Arc::clone(&self);
         // Start getting peers from tracker
         trace!("Getting initial peers...");
         let mut trackers_rx = self
            .clone()
            .get_all_peers(&self.get_announce_list())
            .await
            .unwrap();

         // Handle edge cases (ex. no peers)
         // This isn't a good way to do this. Refactor later.
         //
         // if self.peers.lock().await.is_empty() {
         //    trace!("No peers were provided by trackers.");
         //    return Err(TorrentEngineError::InsufficientPeers.into());
         // };

         let announce_list_length = self.get_announce_list().len();
         let mut trackers_seen = 0;
         while let Some(res) = trackers_rx.recv().await {
            // This additional scope might not be necessary. But for the sake of confidence that
            // peers will be unlocked in the appropriate amount of time,
            // this is what I'm doing.

            // NOTE: This is a potential issue. Ideally, once we get a single message from all the
            // trackers, we end this loop. However, I'm not confident that trackers_rx.recv() will
            // return anything if something goes wrong on the tracker's end, meaning that
            // trackers_seen might not be correctly updated..
            if trackers_seen >= announce_list_length {
               break;
            }
            {
               let mut guard = me.peers.lock().await;
               res.iter().for_each(|peer| {
                  // If we haven't seen this peer before...
                  if !peers_in_action.insert(peer.clone()) {
                     guard.insert(peer.clone());
                  }
               });
               trace!("Inserted peers from tracker succesfully");
            }
            trackers_seen += 1;
         }

         // Go through standard protocol for each peer (ex. handshake, then wait for bitfield, etc.).
         for mut peer in self.peers.lock().await.clone() {
            let me = Arc::clone(&self);
            tokio::spawn(async move {
               // Send handshake
               let mut utp_handler = me.utp_handler.lock().await;
               utp_handler
                  .connect(&mut peer)
                  .await
                  .map_err(|e| {
                     error!(
                        "Error handshaking with peer (socket address {}): {}",
                        peer.socket_addr(),
                        e
                     );
                     TorrentEngineError::InitialHandshakeFailed
                  })
                  .unwrap();

               // TODO
               // Wait for and receive bitfield
               //
               // Loop to handle requests and incoming pieces. Place any acquired pieces in a field in
               // TorrentEngine
            });
         }

         // Wait for the tracker to add some potentially new peers
         sleep(Duration::from_secs(15));

         // Clear the set to ensure that we don't tokio::spawn for a peer that we've already
         // started working with. This set will be "refilled" on the next cycle of the loop, don't
         // worry. If this is confusing, please refer to the documentation on the torrent()
         // function.
         self.peers.lock().await.clear();
      }
   }
}

#[cfg(test)]
mod tests {
   use std::sync::Arc;

   use tracing_test::traced_test;

   use crate::engine::TorrentEngine;

   // THIS TEST IS NOT COMPLETE!!! (DELETEME when torrent() is completed)
   // Until torrent() is fully implemented, this test is not complete.
   // The purpose of this test at this point in time is to ensure that torrent() works to the expected point.
   #[tokio::test]
   #[traced_test]
   async fn test_torrent_with_magnet_uri() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/zenshuu.txt");
      let magnet_uri = tokio::fs::read_to_string(path).await.unwrap();

      let engine = Arc::new(TorrentEngine::new(magnet_uri).await);
      engine.torrent().await.unwrap();
   }
}
