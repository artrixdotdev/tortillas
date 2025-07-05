use std::{
   collections::{HashMap, HashSet},
   net::SocketAddr,
   str::FromStr,
   sync::Arc,
};

use anyhow::{Error, Result, anyhow};
use bitvec::vec::BitVec;
use librqbit_utp::{UtpSocket, UtpSocketUdp};
use tokio::{
   net::TcpListener,
   sync::{Mutex, mpsc},
   time::{Duration, Instant},
};
use tracing::{debug, error, info, instrument, trace, warn};

use crate::{
   hashes::Hash,
   parser::MetaInfo,
   peers::{
      Peer, PeerId, PeerKey,
      commands::{PeerCommand, PeerResponse},
      stream::PeerStream,
   },
};

type PeerMessenger = (mpsc::Sender<PeerCommand>, mpsc::Receiver<PeerResponse>);

/// Helper enum for managing the input to the [torrent()] function.
#[derive(Debug)]
pub enum TorrentInput {
   MagnetUri(String),
   File(String),
}

/// The main engine that any outside libraries/programs should be interacting with.
/// Automatically handles all supported protocols.
///
/// TorrentEngine only supports torrenting a single file at a time (at the moment).
/// However, it should be noted that it does support all supported protocols on initialization.
/// In other words, both tcp_handler and utp_handler are available directly after
/// TorrentEngine::new() is called.
///
/// It should be noted that TorrentEngine does not seed files at the moment. In other words,
/// TorrentEngine is a leecher. The ability to seed files will be added in a future
/// commit/issue/pull request.
#[derive(Debug)]
pub struct TorrentEngine {
   metainfo: MetaInfo,
   id: PeerId,
   active_peers: Arc<Mutex<HashMap<PeerKey, PeerMessenger>>>,
   tcp_addr: Arc<Mutex<Option<SocketAddr>>>,
   utp_addr: Arc<Mutex<Option<SocketAddr>>>,
   bitfield: BitVec<u8>,
   // Statistics tracking
   session_start: Instant,
   stats: Arc<Mutex<TorrentStats>>,
}

#[derive(Debug, Default)]
struct TorrentStats {
   total_peers_discovered: u64,
   unique_peers_discovered: u64,
   active_connections: u64,
   failed_connections: u64,
   bytes_downloaded: u64,
   bytes_uploaded: u64,
}

impl TorrentEngine {
   #[instrument(skip(metainfo), fields(
        info_hash = %metainfo.info_hash().unwrap(),
        announce_list_count = metainfo.announce_list().len()
    ))]
   async fn new(metainfo: MetaInfo) -> Self {
      let info_hash = metainfo.info_hash().unwrap();
      let peer_id = Arc::new(Hash::from_bytes(rand::random::<[u8; 20]>()));

      info!(
          info_hash = %info_hash,
          peer_id = %peer_id,
          trackers = metainfo.announce_list().len(),
          "Creating new torrent engine"
      );

      debug!("Torrent metadata loaded");

      TorrentEngine {
         metainfo,
         id: peer_id,
         active_peers: Arc::new(Mutex::new(HashMap::new())),
         tcp_addr: Arc::new(Mutex::new(None)),
         utp_addr: Arc::new(Mutex::new(None)),
         bitfield: BitVec::EMPTY,
         session_start: Instant::now(),
         stats: Arc::new(Mutex::new(TorrentStats::default())),
      }
   }

   #[instrument(skip(self), fields(
        info_hash = %self.metainfo.info_hash().unwrap()
    ))]
   async fn listen(self: Arc<Self>) -> Result<(Arc<UtpSocketUdp>, TcpListener), Error> {
      let span = tracing::debug_span!("network_setup");
      let _enter = span.enter();

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
          "Network listeners established successfully"
      );

      Ok((utp_socket, tcp_listener))
   }

   #[instrument(skip(self, stream, me), fields(
        peer_addr = %addr,
        protocol = stream.protocol()
    ))]
   async fn handle_peer_connection(
      &self,
      stream: PeerStream,
      addr: SocketAddr,
      me: Arc<TorrentEngine>,
   ) {
      let protocol = stream.protocol();
      debug!("Processing new {protocol} peer connection");

      let peer = Peer::from_socket_addr(addr);
      let (to_tx, mut to_rx) = mpsc::channel(100);

      // Handle peer connection
      let peer_span = tracing::trace_span!("peer_handshake");
      let _peer_enter = peer_span.enter();

      peer
         .handle_peer(
            to_tx,
            me.metainfo.info_hash().unwrap(),
            Arc::clone(&me.id),
            Some(stream),
            None,
            Some(self.bitfield.clone()),
         )
         .await;

      match to_rx.recv().await {
         Some(PeerResponse::Init(from_tx)) => {
            me.active_peers.lock().await.insert(addr, (from_tx, to_rx));

            // Update statistics
            {
               let mut stats = me.stats.lock().await;
               stats.active_connections += 1;
            }

            info!("{protocol} peer successfully initialized and added to active peers");
         }
         Some(response) => {
            warn!(
               ?response,
               "Unexpected peer response during {protocol} initialization"
            );
            let mut stats = me.stats.lock().await;
            stats.failed_connections += 1;
         }
         None => {
            warn!("{protocol} peer channel closed during initialization");
            let mut stats = me.stats.lock().await;
            stats.failed_connections += 1;
         }
      }
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
   #[instrument(skip(self), fields(
        info_hash = %self.metainfo.info_hash().unwrap(),
        peer_id = %self.id
    ))]
   pub async fn torrent(self: Arc<Self>) -> anyhow::Result<(), Error> {
      let session_span = tracing::info_span!("torrent_session");
      let _session_enter = session_span.enter();

      info!("Starting torrent session");

      // Network setup phase
      let network_span = tracing::debug_span!("network_setup");
      let me = self.clone();

      let (utp_listener, tcp_listener) = {
         let _network_enter = network_span.enter();
         debug!("Initializing network listeners");
         me.clone().listen().await?
      };

      // Update addresses with detailed logging
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

      // TCP peer handler
      {
         let me = me.clone();
         let engine_ref = self.clone();
         tokio::spawn(async move {
            let span = tracing::info_span!("tcp_peer_handler");
            let _enter = span.enter();

            info!("TCP peer handler started");

            loop {
               match tcp_listener.accept().await {
                  Ok((stream, addr)) => {
                     let me_clone = me.clone();
                     let engine_clone = engine_ref.clone();
                     tokio::spawn(async move {
                        let stream = PeerStream::Tcp(stream);
                        engine_clone
                           .handle_peer_connection(stream, addr, me_clone)
                           .await;
                     });
                  }
                  Err(e) => {
                     error!(error = %e, "Failed to accept TCP connection");
                  }
               }
            }
         });
      }

      info!("TCP peer handler spawned successfully");

      // UTP peer handler
      {
         let me = me.clone();
         let listener = utp_listener.clone();
         let engine_ref = self.clone();
         tokio::spawn(async move {
            let span = tracing::info_span!("utp_peer_handler");
            let _enter = span.enter();

            info!("UTP peer handler started");

            loop {
               match listener.accept().await {
                  Ok(stream) => {
                     let addr = stream.remote_addr();
                     let me_clone = me.clone();
                     let engine_clone = engine_ref.clone();
                     tokio::spawn(async move {
                        let stream = PeerStream::Utp(stream);
                        engine_clone
                           .handle_peer_connection(stream, addr, me_clone)
                           .await;
                     });
                  }
                  Err(e) => {
                     error!(error = %e, "Failed to accept UTP connection");
                  }
               }
            }
         });
      }

      info!("UTP peer handler spawned successfully");

      // Tracker communication setup
      let tracker_span = tracing::debug_span!("tracker_communication");
      let mut rx_list = vec![];

      {
         let _tracker_enter = tracker_span.enter();

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
                  debug!(tracker_index = index, tracker_url = ?tracker, "Successfully connected to tracker");
                  rx_list.push(rx);
               }
               Err(e) => {
                  warn!(
                      tracker_index = index,
                      tracker_url = ?tracker,
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

      // Peer discovery loop
      let me_discovery = Arc::clone(&me);
      let stats_ref = Arc::clone(&self.stats);

      tokio::spawn(async move {
            let span = tracing::info_span!("peer_discovery");
            let _enter = span.enter();

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
                                stats.total_peers_discovered += peer_count as u64;
                            }

                            debug!(
                                tracker_index,
                                peer_count,
                                "Received peers from tracker"
                            );

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
                                    let (to_tx, mut to_rx) = mpsc::channel(100);
                                    let me_inner = me_discovery.clone();

                                    tokio::spawn(async move {
                                        let peer_span = tracing::debug_span!(
                                            "outbound_peer_connection",
                                            peer_addr = %peer_addr
                                        );
                                        let _peer_enter = peer_span.enter();

                                        debug!("Initiating outbound connection to peer");

                                        peer.handle_peer(
                                            to_tx,
                                            me_inner.metainfo.info_hash().unwrap(),
                                            Arc::clone(&me_inner.id),
                                            None,
                                            Some(listener),
                                            Some(me_inner.bitfield.clone()),
                                        ).await;
                                    });

                                    match to_rx.recv().await {
                                        Some(PeerResponse::Init(from_tx)) => {
                                            me_discovery.clone()
                                                .active_peers
                                                .lock()
                                                .await
                                                .insert(peer_addr, (from_tx, to_rx));

                                            info!(peer_addr = %peer_addr, "Outbound peer connection established");
                                        }
                                        Some(response) => {
                                            warn!(
                                                peer_addr = %peer_addr,
                                                ?response,
                                                "Unexpected response from outbound peer"
                                            );
                                        }
                                        None => {
                                            debug!(
                                                peer_addr = %peer_addr,
                                                "Outbound peer connection failed"
                                            );
                                        }
                                    }
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
        })
        .await
        .map_err(|e| {
            error!(error = %e, "Peer discovery task failed");
            anyhow!("Peer discovery failed: {}", e)
        })?;

      // TODO: Implement the following phases with proper tracing:
      // - Continuously gather bitfields using to_rx
      // - Request pieces from peers that have them
      // - Handle incoming piece messages
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
   // The purpose of this test at this point in time is to ensure that torrent() works to the expected point.
   //
   // This test uses its own subscriber in lieu of traced_test as it desperately needs to show
   // line numbers (which requires the use of tracing_subscriber).
   //
   // If debugging, a known good peer for the torrent in zenshuu.txt is 95.234.80.134:46519 (as of 06/17/2025).
   // This was confirmed through use of the transmission BitTorrent client.
   #[tokio::test(flavor = "multi_thread", worker_threads = 50)]
   async fn test_torrent_with_magnet_uri() {
      let subscriber = fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
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
