use std::{
   collections::{HashMap, HashSet},
   net::SocketAddr,
   str::FromStr,
   sync::Arc,
   thread::sleep,
   time::Duration,
};

use anyhow::{Error, Result, anyhow};
use bitvec::vec::BitVec;
use librqbit_utp::{UtpSocket, UtpSocketUdp};
use tokio::{
   net::TcpListener,
   sync::{Mutex, RwLock, mpsc, oneshot},
};
use tracing::{error, trace};

use crate::{
   errors::{PeerTransportError, TorrentEngineError},
   hashes::{Hash, InfoHash},
   parser::{MagnetUri, MetaInfo, TorrentFile},
   peers::{
      Peer, PeerId, PeerKey,
      commands::{PeerCommand, PeerResponse},
      messages::PeerMessages,
      stream::PeerStream,
   },
   tracker::Tracker,
};

type PeerMessenger = (mpsc::Sender<PeerCommand>, mpsc::Receiver<PeerResponse>);

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
         tcp_addr: Arc::new(Mutex::new(None)),
         utp_addr: Arc::new(Mutex::new(None)),
         bitfield: BitVec::EMPTY,
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
                     None,
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
         let listener = utp_listener.clone();
         tokio::spawn(async move {
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
                     None,
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
               .stream_peers(info_hash, Some(primary_addr), Some(*me.id))
               .await
               .unwrap(),
         );
      }

      let me = Arc::clone(&me);

      // Repeatedly gather data from each rx and update self.peers as new peers are added
      trace!("Spawning task to handle output from initial requests");

      // Loops through every rx and awaits a response. This may be extremely efficient; ex: we
      // are given three trackers. One has a delay of 300 seconds, and the others have a delay
      // of 15 seconds. The other two trackers will be forced to wait 300 seconds. FIXME.
      //
      // A list of peers that we've already seen
      let mut peers_in_action = HashSet::new();

      tokio::spawn(async move {
         loop {
            // We do not need a timeout/sleep here as stream_peers handles that for us.
            for rx in rx_list.iter_mut() {
               let res = rx.recv().await.unwrap();

               trace!("Received peers from get_all_peers()");
               for peer in res {
                  if !peers_in_action.insert(peer.clone()) {
                     let listener = utp_listener.clone();
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
                              // This enables the peer to connect via UTP or TCP
                              Some(listener),
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
            }
         }
      })
      .await
      .unwrap();

      // NOTE: Remove await.unwrap() after adding next few parts.

      // Continously gather bitfields using to_rx

      // Request a piece from every peer that has that piece

      // Handle incoming piece messages

      // Handle incoming request messages (in this case, we are the peer)

      Ok(())
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
