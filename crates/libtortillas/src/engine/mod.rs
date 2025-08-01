use std::{
   collections::{HashMap, HashSet},
   net::SocketAddr,
   ops::BitOrAssign,
   str::FromStr,
   sync::Arc,
};

use anyhow::{Error, Result, anyhow};
use bitvec::vec::BitVec;
use librqbit_utp::{UtpSocket, UtpSocketUdp};
use tokio::{
   net::TcpListener,
   sync::{Mutex, RwLock, broadcast, mpsc},
   time::{Duration, Instant},
};
use tracing::{debug, error, info, instrument, trace, warn};

use crate::{
   hashes::Hash,
   parser::MetaInfo,
   peers::{
      Peer, PeerId, PeerKey,
      peer_comms::{
         commands::{PeerCommand, PeerResponse},
         messages::PeerMessages,
         stream::PeerStream,
      },
   },
};

type PeerMessenger = mpsc::Sender<PeerCommand>;

/// Helper enum for spawning listeners in listen_for_incoming_peers
pub enum ProtocolListener {
   Utp(Arc<UtpSocketUdp>),
   Tcp(TcpListener),
}

/// Helper enum for managing the input to the [torrent](TorrentEngine::torrent)
/// function.
#[derive(Debug)]
pub enum TorrentInput {
   MagnetUri(String),
   File(String),
}

/// The main engine that any outside libraries/programs should be interacting
/// with. Automatically handles all supported protocols.
///
/// TorrentEngine only supports torrenting a single file at a time (at the
/// moment). However, it should be noted that it does support all supported
/// protocols on initialization. In other words, both tcp_handler and
/// utp_handler are available directly after TorrentEngine::new() is called.
///
/// It should be noted that TorrentEngine does not seed files at the moment. In
/// other words, TorrentEngine is a leecher. The ability to seed files will be
/// added in a future commit/issue/pull request.
#[derive(Debug)]
pub struct TorrentEngine {
   metainfo: MetaInfo,
   id: PeerId,
   active_peers: Arc<Mutex<HashMap<PeerKey, PeerMessenger>>>,
   to_engine_tx_rx: (
      broadcast::Sender<PeerResponse>,
      broadcast::Receiver<PeerResponse>,
   ),
   tcp_addr: Arc<Mutex<Option<SocketAddr>>>,
   utp_addr: Arc<Mutex<Option<SocketAddr>>>,
   bitfield: Arc<RwLock<BitVec<u8>>>,

   // Statistics tracking
   session_start: Instant,
   stats: Arc<Mutex<TorrentStats>>,
}

#[derive(Debug, Default)]
#[allow(dead_code)]
struct TorrentStats {
   total_peers_discovered: usize,
   unique_peers_discovered: usize,
   active_connections: usize,
   failed_connections: usize,
   bytes_downloaded: usize,
   bytes_uploaded: usize,
}

impl TorrentEngine {
   #[instrument(skip(metainfo), fields(
        info_hash = %metainfo.info_hash().unwrap(),
        announce_list_count = metainfo.announce_list().len()
    ))]
   #[allow(dead_code)]
   async fn new(metainfo: MetaInfo) -> Self {
      let info_hash = metainfo.info_hash().unwrap();
      let peer_id = Arc::new(Hash::from_bytes(rand::random::<[u8; 20]>()));
      let to_engine_tx_rx = broadcast::channel(100);

      info!(
          info_hash = %info_hash,
          peer_id = %peer_id,
          tracker_count = metainfo.announce_list().len(),
          "Creating new torrent engine"
      );

      TorrentEngine {
         metainfo,
         id: peer_id,
         active_peers: Arc::new(Mutex::new(HashMap::new())),
         tcp_addr: Arc::new(Mutex::new(None)),
         utp_addr: Arc::new(Mutex::new(None)),
         to_engine_tx_rx,
         bitfield: Arc::new(RwLock::new(BitVec::EMPTY)),
         session_start: Instant::now(),
         stats: Arc::new(Mutex::new(TorrentStats::default())),
      }
   }

   #[instrument(skip(self), fields(
        info_hash = %self.metainfo.info_hash().unwrap()
    ))]
   async fn listen(self: Arc<Self>) -> Result<(Arc<UtpSocketUdp>, TcpListener), Error> {
      debug!("Setting up network listeners");

      let utp_socket = match UtpSocket::new_udp(SocketAddr::from_str("0.0.0.0:0").unwrap()).await {
         Ok(socket) => socket,
         Err(e) => {
            error!(error = %e, "Failed to create UTP socket");
            return Err(anyhow!("UTP socket creation failed: {}", e));
         }
      };

      let tcp_listener = match TcpListener::bind("0.0.0.0:0").await {
         Ok(listener) => listener,
         Err(e) => {
            error!(error = %e, "Failed to create TCP listener");
            return Err(anyhow!("TCP listener creation failed: {}", e));
         }
      };

      let tcp_addr = tcp_listener.local_addr().map_err(|e| {
         error!(error = %e, "Failed to get TCP local address");
         anyhow!("TCP address retrieval failed: {}", e)
      })?;

      let utp_addr = utp_socket.bind_addr();

      info!(
          tcp_addr = %tcp_addr,
          utp_addr = %utp_addr,
          "Network listeners established"
      );

      Ok((utp_socket, tcp_listener))
   }

   /// Handles new peer connections by means of a PeerStream, not a listener.
   #[instrument(skip(self, stream), fields(
        peer_addr = %addr,
        protocol = stream.protocol()
    ))]
   async fn handle_peer_connection(self: Arc<Self>, stream: PeerStream, addr: SocketAddr) {
      let protocol = stream.protocol();
      debug!(protocol, "Processing new peer connection");

      let peer = Peer::from_socket_addr(addr);

      tokio::spawn(async move {
         self.spawn_handle_peer(peer, None, Some(stream)).await;
      });
   }

   #[instrument(skip(self), fields(
        session_duration = ?self.session_start.elapsed()
    ))]
   async fn log_statistics(&self) {
      let stats = self.stats.lock().await;
      let active_peer_count = self.active_peers.lock().await.len();
      let session_duration = self.session_start.elapsed();

      info!(
         active_peers = active_peer_count,
         session_duration_secs = session_duration.as_secs(),
         unique_peers_discovered = stats.unique_peers_discovered,
         total_peers_discovered = stats.total_peers_discovered,
         failed_connections = stats.failed_connections,
         bytes_downloaded = stats.bytes_downloaded,
         bytes_uploaded = stats.bytes_uploaded,
         "Torrent session statistics"
      );
   }

   /// Helper function for spawning what we call a "peer thread". A peer thread
   /// is an unnecessarily fancy phrase for the thread that handle_peer runs
   /// on -- the thread that allows a peer to operate semi-autonomously from
   /// TorrentEngine.
   ///
   /// To be more specific, once we spawn this thread, we can only communicate
   /// with the peer through the given channels. The peer thread handles the
   /// remote connection to the peer on its own.
   async fn spawn_handle_peer(
      self: Arc<Self>, peer: Peer, listener: Option<Arc<UtpSocketUdp>>, stream: Option<PeerStream>,
   ) {
      debug!(peer_addr = %peer.socket_addr(), "Initiating outbound connection to peer");

      let bitfield: BitVec<u8>;
      {
         bitfield = self.bitfield.read().await.clone();
      }

      peer
         .handle_peer(
            self.to_engine_tx_rx.0.clone(),
            self.metainfo.info_hash().unwrap(),
            Arc::clone(&self.id),
            stream,
            listener,
            Some(bitfield),
         )
         .await;
   }

   /// A helper function for listening for peers trying to connect to us on
   /// either Tcp or Utp.
   async fn listen_on_protocol(self: Arc<Self>, listener: ProtocolListener) {
      match listener {
         ProtocolListener::Utp(listener) => {
            tokio::spawn(async move {
               info!("UTP peer handler started");

               loop {
                  match listener.accept().await {
                     Ok(stream) => {
                        let addr = stream.remote_addr();
                        let engine_clone = self.clone();
                        tokio::spawn(async move {
                           let stream = PeerStream::Utp(stream);
                           engine_clone.handle_peer_connection(stream, addr).await;
                        });
                     }
                     Err(e) => {
                        error!(error = %e, "Failed to accept UTP connection");
                     }
                  }
               }
            });

            debug!("UTP peer handler spawned");
         }
         ProtocolListener::Tcp(listener) => {
            let engine_ref = self.clone();
            tokio::spawn(async move {
               info!("TCP peer handler started");

               loop {
                  match listener.accept().await {
                     Ok((stream, addr)) => {
                        let engine_clone = engine_ref.clone();
                        tokio::spawn(async move {
                           let stream = PeerStream::Tcp(stream);
                           engine_clone.handle_peer_connection(stream, addr).await;
                        });
                     }
                     Err(e) => {
                        error!(error = %e, "Failed to accept TCP connection");
                     }
                  }
               }
            });

            debug!("TCP peer handler spawned");
         }
      }
   }

   /// Listens for any peers that are trying to connect to us over uTP or TCP.
   /// Returns the created UtpListener for later use. This is unnecessary to
   /// do for TCP due to the nature of the protocol itself.
   async fn listen_for_incoming_peers(self: Arc<Self>) -> Result<Arc<UtpSocketUdp>, Error> {
      debug!("Initializing network listeners");
      let me = self.clone();

      let (utp_listener, tcp_listener) = me.clone().listen().await?;

      // Update addresses
      {
         let tcp_addr = tcp_listener.local_addr().ok();
         let utp_addr = Some(utp_listener.bind_addr());

         debug!(
             tcp_addr = ?tcp_addr,
             utp_addr = ?utp_addr,
             "Updating local addresses in engine state"
         );

         let mut tcp_addr_guard = self.tcp_addr.lock().await;
         *tcp_addr_guard = tcp_addr;

         let mut utp_addr_guard = self.utp_addr.lock().await;
         *utp_addr_guard = utp_addr;
      }

      let me_tcp_listener = me.clone();
      me_tcp_listener
         .listen_on_protocol(ProtocolListener::Tcp(tcp_listener))
         .await;

      let me_utp_listener = me.clone();
      me_utp_listener
         .listen_on_protocol(ProtocolListener::Utp(utp_listener.clone()))
         .await;

      Ok(utp_listener.clone())
   }

   /// The full torrenting process, summarized in a single function. As of
   /// 5/23/25, the return value of this function is temporary.
   ///
   /// The general flow of this function is as follows:
   /// - Get initial peers from trackers
   /// - Go through standard protocol for each peer (ex. handshake, then wait
   ///   for bitfield, etc.).
   /// - Pieces will be maintained in the TorrentEngine struct
   /// - Get new peers from each tracker
   /// - Remove any duplicate peers
   /// - Repeat
   ///
   /// This also makes seeding very easy -- when a peer asks for a piece, just
   /// send them self.pieces at whatever index they asked for.
   ///
   /// TODO: This function will likely return a torrented file, or a path to a
   /// locally torrented file.
   ///
   /// TODO: Fix mutable_key_type
   #[instrument(skip(self), fields(
        info_hash = %self.metainfo.info_hash().unwrap(),
        peer_id = %self.id
    ))]
   #[allow(clippy::mutable_key_type)]
   pub async fn torrent(self: Arc<Self>) -> anyhow::Result<(), Error> {
      let me = self.clone();

      info!("Starting torrent session");

      let me_listen = me.clone();
      let utp_listener = me_listen.listen_for_incoming_peers().await?;

      // Tracker communication setup
      let mut rx_list = vec![];

      {
         let primary_addr = if let Some(addr) = *me.tcp_addr.lock().await {
            debug!(primary_addr = %addr, protocol = "tcp", "Using TCP as primary address");
            addr
         } else {
            let addr = me
               .utp_addr
               .lock()
               .await
               .ok_or_else(|| anyhow!("Neither TCP nor UTP address was available"))?;
            debug!(primary_addr = %addr, protocol = "utp", "Using UTP as primary address");
            addr
         };

         info!(
             primary_addr = %primary_addr,
             tracker_count = me.metainfo.announce_list().len(),
             "Making initial requests to trackers"
         );

         let info_hash = me.metainfo.info_hash().unwrap();
         for (index, tracker) in me.metainfo.announce_list().iter().enumerate() {
            match tracker
               .stream_peers(info_hash, Some(primary_addr), Some(*me.id))
               .await
            {
               Ok(rx) => {
                  debug!(tracker_index = index, "Successfully connected to tracker");
                  rx_list.push(rx);
               }
               Err(e) => {
                  warn!(
                      tracker_index = index,
                      error = %e,
                      "Failed to connect to tracker"
                  );
               }
            }
         }

         if rx_list.is_empty() {
            error!("No trackers available for peer discovery");
            return Err(anyhow!("All tracker connections failed"));
         }

         info!(
            connected_trackers = rx_list.len(),
            "Tracker setup completed"
         );
         primary_addr
      };

      // Spawns a loop to handle responses from `to_engine_tx_rx.1` (AKA the receiver
      // that all peer threads send messages to)
      //
      // In other words, this thread handles all PeerResponses from each spawn of
      // handle_peer.
      let me_handle_peer = self.clone();
      tokio::spawn(async move {
         let mut to_engine_tx = me_handle_peer.to_engine_tx_rx.0.subscribe();

         while let Ok(message) = to_engine_tx.recv().await {
            match message {
               PeerResponse::Init {
                  from_engine_tx,
                  peer_key,
               } => {
                  me_handle_peer
                     .clone()
                     .active_peers
                     .lock()
                     .await
                     .insert(peer_key, from_engine_tx);

                  info!(peer_addr = %peer_key, "Outbound peer connection established");
               }
               // This shouldn't (ideally) result in any peers being sent pieces before we have a
               // bitfield saved, as the first peer that sends us a bitfield will not have unchoked
               // yet.
               PeerResponse::Unchoke {
                  from_engine_tx,
                  peer_key,
               } => {
                  trace!(peer_key = %peer_key, "Peer unchoked, requesting pieces");
                  for piece_num in 0..me_handle_peer.bitfield.read().await.len() {
                     match from_engine_tx.send(PeerCommand::Piece(piece_num)).await {
                        Ok(_) => {
                           trace!(peer_key = %peer_key, piece_num, "Sent piece request to peer");
                        }
                        Err(e) => {
                           error!(
                              peer_key = %peer_key,
                              piece_num,
                              error = %e,
                              "Failed to send piece request to peer"
                           )
                        }
                     }
                  }
               }
               PeerResponse::Receive { message, .. } => {
                  // This is guaranteed to not run until self.bitfield is set to the correct
                  // length.
                  match message {
                     PeerMessages::Piece(index, _, _) => {
                        {
                           let mut bitfield = me_handle_peer.bitfield.write().await;
                           if let Some(mut bit) = bitfield.get_mut(index as usize) {
                              *bit = true;
                           }
                        }

                        trace!(piece_index = index, "Received piece from peer");
                        // TODO: Save piece
                     }
                     PeerMessages::KeepAlive => {
                        // As far as I am aware, we don't have to do anything
                        // for KeepAlive messages.
                     }
                     _ => {}
                  }
               }
               _ => {}
            }
         }
      });

      // Peer discovery loop
      let me_discovery = Arc::clone(&me);
      let stats_ref = Arc::clone(&self.stats);

      tokio::spawn(async move {
         let mut peers_in_action = HashSet::new();
         let mut last_stats_log = Instant::now();
         let stats_interval = Duration::from_secs(30);

         info!("Starting peer discovery loop");

         loop {
            // Log statistics periodically
            if last_stats_log.elapsed() > stats_interval {
               me_discovery.log_statistics().await;
               last_stats_log = Instant::now();
            }

            for (tracker_index, rx) in rx_list.iter_mut().enumerate() {
               match rx.recv().await {
                  Some(peers) => {
                     let peer_count = peers.len();

                     // Update statistics
                     {
                        let mut stats = stats_ref.lock().await;
                        stats.total_peers_discovered += peer_count;
                     }

                     debug!(tracker_index, peer_count, "Received peers from tracker");

                     for peer in peers {
                        if peers_in_action.insert(peer.clone()) {
                           // Update unique peer count
                           {
                              let mut stats = stats_ref.lock().await;
                              stats.unique_peers_discovered += 1;
                           }

                           let peer_addr = peer.socket_addr();

                           trace!(
                               peer_addr = %peer_addr,
                               tracker_index,
                               "Discovered new unique peer"
                           );

                           let listener = utp_listener.clone();
                           let me_inner = me_discovery.clone();
                           tokio::spawn(async move {
                              me_inner.spawn_handle_peer(peer, Some(listener), None).await;
                           });
                        } else {
                           trace!(
                               peer_addr = %peer.socket_addr(),
                               "Skipping duplicate peer"
                           );
                        }
                     }
                  }
                  None => {
                     warn!(tracker_index, "Tracker channel closed unexpectedly");
                  }
               }
            }
         }
      });

      // See the definition of "ready" below for an explanation of this.
      let mut ready_bitfield: BitVec<u8> = BitVec::EMPTY;

      // Gather peers until we are "ready". We define "ready" as having ~80% of the
      // pieces of a torrent accounted for. For example, if a given torrent has
      // 6 pieces, and peer A has pieces 0 and 1, peer B has pieces 2 and 3, and
      // peer C has pieces 4, but not 5, then we are ready.
      //
      // However, if peer A does not have piece 1, then we are not ready, because we
      // only have ~66% of the pieces accounted for.
      //
      // Why 80%? Some peers appear to be consistently missing some (& the same)
      // pieces of a torrent, and 80% should *ideally* guarantee that we have
      // most of the pieces, and we'll get the ones that we don't have later.
      //
      // Additionally, overlap is allowed. If both peer A and B have piece 1, then we
      // simply say that that piece is accounted for.
      //
      // Yes, there are other ways to do this such as using the pieces field of the
      // info dictionary. However, this is simple and effective.
      //
      // This loop will run until we are ready.
      const READY_VALUE: f64 = 0.80;

      let mut accounted_bitfield_peer_rx = me.to_engine_tx_rx.0.subscribe();

      loop {
         let response = accounted_bitfield_peer_rx.recv().await;
         trace!(response = ?response, "Received bitfield response");
         if let Ok(PeerResponse::Receive {
            message: PeerMessages::Bitfield(bitfield),
            ..
         }) = response
         {
            trace!("Processing bitfield message from peer");

            // Hacky way to initialize bitfield to correct length
            if ready_bitfield.is_empty() {
               ready_bitfield = bitfield.clone();
            } else {
               ready_bitfield.bitor_assign(bitfield);
            }

            let ready_ratio = ready_bitfield.count_ones() as f64 / ready_bitfield.len() as f64;
            if ready_ratio >= READY_VALUE {
               info!(
                  ready_percentage = ready_ratio * 100.0,
                  pieces_available = ready_bitfield.count_ones(),
                  total_pieces = ready_bitfield.len(),
                  "Reached readiness threshold for piece availability"
               );
               break;
            }
         }
      }

      // - Handle incoming piece messages and any failed requests
      // - Save files to disk one by one when each piece is done downloading
      // - Handle incoming request messages (seeding)

      info!("Torrent session completed");
      Ok(())
   }
}

#[cfg(test)]
mod tests {
   use std::sync::Arc;

   use tracing_subscriber::fmt;

   use crate::{engine::TorrentEngine, parser::MetaInfo};

   // THIS TEST IS NOT COMPLETE!!! (DELETEME when torrent() is completed)
   // Until torrent() is fully implemented, this test is not complete.
   // The purpose of this test at this point in time is to ensure that torrent()
   // works to the expected point.
   //
   // This test uses its own subscriber in lieu of traced_test as it desperately
   // needs to show line numbers (which requires the use of tracing_subscriber).
   #[tokio::test(flavor = "multi_thread", worker_threads = 50)]
   #[ignore = "Unstable"]
   async fn test_torrent_with_magnet_uri() {
      let subscriber = fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .finish();
      tracing::subscriber::set_global_default(subscriber).expect("subscriber already set");

      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/wired-cd.txt");
      let magnet_uri = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MetaInfo::new(magnet_uri).await.unwrap();

      let engine = Arc::new(TorrentEngine::new(metainfo).await);

      engine.torrent().await.unwrap();
   }
}
