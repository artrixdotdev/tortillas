use std::{
   collections::{HashMap, HashSet},
   net::SocketAddr,
   str::FromStr,
   sync::Arc,
   thread::sleep,
   time::Duration,
};

use anyhow::{anyhow, Error, Result};
use bitvec::vec::BitVec;
use librqbit_utp::{UtpSocket, UtpSocketUdp};
use tokio::{
   net::TcpListener,
   sync::{mpsc, oneshot, Mutex, RwLock},
};
use tracing::{error, trace};

use crate::{
   errors::{PeerTransportError, TorrentEngineError},
   hashes::{Hash, InfoHash},
   parser::{MagnetUri, MetaInfo, TorrentFile},
   peers::{
      commands::{PeerCommand, PeerResponse},
      messages::PeerMessages,
      stream::PeerStream,
      tcp::TcpProtocol,
      transport_messages::{TransportCommand, TransportResponse},
      utp::UtpProtocol,
      Peer, PeerId, PeerKey, TransportHandler,
   },
   tracker::Tracker,
};

type PeerMessenger = (mpsc::Sender<PeerCommand>, mpsc::Receiver<PeerResponse>);

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
#[derive(Debug)]
pub struct TorrentEngine {
   metainfo: MetaInfo,
   id: PeerId,
   active_peers: Arc<Mutex<HashMap<PeerKey, PeerMessenger>>>,
   peers: Arc<RwLock<HashSet<Peer>>>,
   tcp_addr: Arc<Mutex<Option<SocketAddr>>>,
   utp_addr: Arc<Mutex<Option<SocketAddr>>>,
   bitfield: BitVec<u8>,
}

impl TorrentEngine {
   async fn new(metainfo: MetaInfo) -> Self {
      trace!("Creating new transport handler");

      TorrentEngine {
         metainfo,
         id: Arc::new(Hash::from_bytes(rand::random::<[u8; 20]>())),
         active_peers: Arc::new(Mutex::new(HashMap::new())),
         peers: Arc::new(RwLock::new(HashSet::new())),
         tcp_addr: Arc::new(Mutex::new(None)),
         utp_addr: Arc::new(Mutex::new(None)),
         bitfield: BitVec::EMPTY,
      }
   }

   /// Contacts all given trackers for a list of peers
   async fn get_all_peers(self: Arc<Self>) {
      // Get an rx for each tracker
      let mut rx_list = vec![];

      let me = self.clone();
      let primary_addr = if let Some(addr) = *me.tcp_addr.lock().await {
         addr
      } else {
         me.utp_addr
            .lock()
            .await
            .expect("Neither TCP nor UTP address was provided")
      };

      trace!("Making initial requests to trackers");
      let info_hash = me.metainfo.info_hash().unwrap();
      for tracker in me.metainfo.announce_list().iter() {
         rx_list.push(
            tracker
               .stream_peers(info_hash, Some(primary_addr))
               .await
               .unwrap(),
         );
      }
      {
         let me = Arc::clone(&me);
         // Repeatedly gather data from each rx and update self.peers as new peers are added
         trace!("Spawning task to handle output from initial requests");
         tokio::spawn(async move {
            // Loops through every rx and awaits a response. This may be extremely efficient; ex: we
            // are given three trackers. One has a delay of 300 seconds, and the others have a delay
            // of 15 seconds. The other two trackers will be forced to wait 300 seconds. FIXME.
            //
            // A list of peers that we've already seen
            let mut peers_in_action = HashSet::new();
            loop {
               // We do not need a timeout/sleep here as stream_peers handles that for us.
               for rx in rx_list.iter_mut() {
                  let res = rx.recv().await.unwrap();

                  trace!("Received peers from get_all_peers()");
                  let mut guard = me.peers.write().await;
                  for peer in res {
                     if !peers_in_action.insert(peer.clone()) {
                        guard.insert(peer.clone());
                        trace!("Added peer: {}", peer.clone());
                     }
                  }
               }
            }
         });
      }
   }

   async fn listen(self: Arc<Self>) -> (Arc<UtpSocketUdp>, TcpListener) {
      (
         UtpSocket::new_udp(SocketAddr::from_str("0.0.0.0:0").unwrap())
            .await
            .unwrap(),
         TcpListener::bind("0.0.0.0:0").await.unwrap(),
      )
   }

   async fn handle_peer(
      self: Arc<Self>,
      mut peer: Peer,
      tcp_tx: mpsc::Sender<TransportCommand>,
      utp_tx: mpsc::Sender<TransportCommand>,
   ) {
      // Send handshake. Unwrap is called on this because this code goes directly to our
      // functions, not a library's.
      //
      // Try connecting over TCP and uTP, and use whichever one works. While this may seem
      // "not to spec", this is how the transmission BitTorrent client does it: https://github.com/transmission/transmission/discussions/7603

      // TCP
      let (tcp_connect_tx, tcp_connect_rx) =
         oneshot::channel::<Result<TransportResponse, PeerTransportError>>();
      tcp_tx
         .send(TransportCommand::Connect {
            peer: (peer.clone()),
            oneshot_tx: tcp_connect_tx,
         })
         .await
         .unwrap();

      // uTP
      let (utp_connect_tx, utp_connect_rx) =
         oneshot::channel::<Result<TransportResponse, PeerTransportError>>();
      utp_tx
         .send(TransportCommand::Connect {
            peer: (peer.clone()),
            oneshot_tx: utp_connect_tx,
         })
         .await
         .unwrap();

      // Assign based on which protocol the peer is operating on.
      // If utp_connect_rx returns first, then the peer is operating on uTP (of course, they could
      // be operating on TCP too, but it wouldn't really matter). If tcp_connect_rx returns first,
      // then the peer is operating on TCP & the same logic would apply.
      //
      // Note that we do NOT care about the return value of either of these oneshots.
      let tx = tokio::select! {
          res = utp_connect_rx => {
              // Ensure that uTP didn't just time out.
              match res {
                Ok(inner) => {
                    match inner {
                        Ok(_) => {
                            // If we got this far, we're good.
                        },
                        Err(e) => {
                            error!("Peer {} timed out when handshaking: {}", peer, e);
                            return;
                        }
                    }
                },
                Err(e) => {
                    error!("Recv error: {}", e);
                    return;
                }
              };

              trace!("Peer {} seems to be using uTP", peer);

              utp_tx},
          res = tcp_connect_rx => {
              // Ensure that uTP didn't just time out.
              match res {
                Ok(inner) => {
                    match inner {
                        Ok(_) => {
                            // If we got this far, we're good.
                        },
                        Err(e) => {
                            error!("Peer {} timed out when handshaking: {}", peer, e);
                            return;
                        }
                    }
                },
                Err(e) => {
                    error!("Recv error from peer {}: {}", peer, e);
                    return;
                }
              };

              trace!("Peer {} seems to be using TCP", peer);

              tcp_tx},
      };

      // Send an empty bitfield (NOTE: this may need to be adjusted in the future for seeding)
      let (send_bitfield_tx, send_bitfield_rx) =
         oneshot::channel::<Result<TransportResponse, PeerTransportError>>();
      tx.send(TransportCommand::Send {
         message: (PeerMessages::Bitfield(BitVec::from_vec(vec![]))),
         peer_key: (peer.socket_addr()),
         oneshot_tx: (send_bitfield_tx),
      })
      .await
      .unwrap();

      // Ensure that bitfield actually sends
      match send_bitfield_rx.await.unwrap() {
         Ok(message) => match message {
            TransportResponse::Send(addr) => {
               trace!("Succesfully sent bitfield to peer {}", addr);
            }
            _ => {
               trace!(
                  "Got something entirely incorrect back from send_bitfield_rx for peer {}",
                  peer
               );
            }
         },
         Err(e) => {
            error!(
               "Error when processing result of send_data after sending bitfield to peer {}: {}",
               peer, e
            );
         }
      }

      // Wait for and receive bitfield
      let (bitfield_tx, bitfield_rx) =
         oneshot::channel::<Result<TransportResponse, PeerTransportError>>();
      tx.send(TransportCommand::Receive {
         peer_key: (peer.clone().socket_addr()),
         oneshot_tx: (bitfield_tx),
      })
      .await
      .unwrap();

      match bitfield_rx.await.unwrap() {
         Ok(res) => {
            match res {
               TransportResponse::Receive { message, peer_key } => {
                  trace!(
                     "Received message from peer {}. Message: {:?}",
                     peer_key,
                     message
                  );

                  // Set bitfield of peer
                  peer.pieces = match message {
                     PeerMessages::Bitfield(bitfield) => bitfield,
                     // If the response isn't a bitfield for some reason...
                     _ => BitVec::EMPTY,
                  }
               }
               // This should never happen.
               _ => {
                  trace!("Got something other than a bitfield from peer {}", peer);
               }
            }
         }
         // We *might* be able to handle this in the future. But for now, just
         // panic.
         Err(e) => {
            error!(
               "An error occurred when handling the bitfield received from the peer {}: {}",
               peer, e
            );
            panic!("");
         }
      }

      trace!("Begin interested/unchoke process for peer {}", peer);

      // TODO
      // Loop to handle requests and incoming pieces. Place any acquired pieces in a field in
      // TorrentEngine
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
      // Start getting peers from tracker
      trace!("Getting initial peers...");

      let me = self.clone();

      let (utp_listener, tcp_listener) = me.clone().listen().await;

      {
         let mut tcp_addr_guard = self.tcp_addr.lock().await;
         *tcp_addr_guard = tcp_listener.local_addr().ok();

         let mut utp_addr_guard = self.utp_addr.lock().await;
         *utp_addr_guard = Some(utp_listener.bind_addr());
      }

      {
         let me = me.clone();
         tokio::spawn(async move {
            loop {
               let (stream, addr) = tcp_listener.accept().await.unwrap();
               let stream = PeerStream::Tcp(stream);
               let peer = Peer::from_socket_addr(addr);

               let (to_tx, mut to_rx) = mpsc::channel(100);

               peer
                  .handle_peer(
                     to_tx,
                     me.metainfo.info_hash().unwrap(),
                     Arc::clone(&me.id),
                     Some(stream),
                  )
                  .await;

               let peer_response = to_rx.recv().await.unwrap();

               if let PeerResponse::Init(from_tx) = peer_response {
                  me.active_peers.lock().await.insert(addr, (from_tx, to_rx));
               }
            }
         });
      }

      trace!("Started listening for TCP peer");

      {
         let me = me.clone();
         tokio::spawn(async move {
            let listener = utp_listener;

            loop {
               let stream = listener.accept().await.unwrap();
               let addr = stream.remote_addr();

               let stream = PeerStream::Utp(stream);
               let peer = Peer::from_socket_addr(addr);

               let (to_tx, mut to_rx) = mpsc::channel(100);

               peer
                  .handle_peer(
                     to_tx,
                     me.metainfo.info_hash().unwrap(),
                     Arc::clone(&me.id),
                     Some(stream),
                  )
                  .await;

               let peer_response = to_rx.recv().await.unwrap();

               if let PeerResponse::Init(from_tx) = peer_response {
                  me.active_peers.lock().await.insert(addr, (from_tx, to_rx));
               }
            }
         });
      }

      trace!("Started listening for uTP peer");

      {
         let me = me.clone();
         tokio::spawn(async move {
            me.get_all_peers().await;
         });
      }

      trace!("Started get_all_peers");

      // If there are no peers, wait until there are. If there aren't, everything implodes on
      // itself. If empty_counter reaches 10, something's probably gone wrong and the program
      // should exit.
      //
      // We are doing this outside the loop -- once we have a few initial peers, we don't need to
      // worry if the trackers don't send any more.
      let mut empty_counter = 0;
      while me.peers.read().await.is_empty() {
         if empty_counter == 5 {
            return Err(TorrentEngineError::InsufficientPeers.into());
         }
         trace!("No peers were provided by trackers yet!");
         sleep(Duration::from_secs(2));
         empty_counter += 1;
      }

      loop {
         // Go through standard protocol for each peer (ex. handshake, then wait for bitfield, etc.).
         trace!("Beginning iteration of peers");
         {
            for peer in me.peers.read().await.clone() {
               let (to_tx, mut to_rx) = mpsc::channel(100);

               let peer_addr = peer.socket_addr();

               let me_inner = me.clone();
               tokio::spawn(async move {
                  peer
                     .handle_peer(
                        to_tx,
                        me_inner.metainfo.info_hash().unwrap(),
                        Arc::clone(&me_inner.id),
                        None,
                     )
                     .await;
               });

               let peer_response = to_rx.recv().await.unwrap();

               if let PeerResponse::Init(from_tx) = peer_response {
                  me.clone()
                     .active_peers
                     .lock()
                     .await
                     .insert(peer_addr, (from_tx, to_rx));
               }
            }
         }

         // Wait for the tracker to add some potentially new peers
         trace!("Sleeping for potential new peers");
         sleep(Duration::from_secs(15));

         // Clear the set to ensure that we don't tokio::spawn for a peer that we've already
         // started working with. This set will be "refilled" on the next cycle of the loop, don't
         // worry. If this is confusing, please refer to the documentation on the torrent()
         // function.
         trace!("Locking & clearing peers");
         me.peers.write().await.clear();
         trace!("Rerunning loop with new peers");
      }
   }
}

#[cfg(test)]
mod tests {
   use std::sync::Arc;

   use tracing::Level;
   use tracing_subscriber::fmt;

   use crate::{engine::TorrentEngine, parser::MetaInfo};

   // THIS TEST IS NOT COMPLETE!!! (DELETEME when torrent() is completed)
   // Until torrent() is fully implemented, this test is not complete.
   // The purpose of this test at this point in time is to ensure that torrent() works to the expected point.
   //
   // This test uses its own subscriber in lieu of traced_test as it desperately needs to show
   // line numbers (which requires the use of tracing_subscriber).
   //
   // If debugging, a known good peer for the torrent in zenshuu.txt is 95.234.80.134:46519 (as of 06/17/2025). This was confirmed
   // through use of the transmission BitTorrent client.
   #[tokio::test(flavor = "multi_thread", worker_threads = 50)]
   async fn test_torrent_with_magnet_uri() {
      let subscriber = fmt()
         .with_line_number(true)
         .with_max_level(Level::TRACE)
         .finish();
      tracing::subscriber::set_global_default(subscriber).expect("subscriber already set");

      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/zenshuu.txt");
      let magnet_uri = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MetaInfo::new(magnet_uri).await.unwrap();

      let engine = Arc::new(TorrentEngine::new(metainfo).await);
      engine.torrent().await.unwrap();
   }
}
