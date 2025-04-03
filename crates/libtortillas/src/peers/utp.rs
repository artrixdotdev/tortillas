use super::{Peer, PeerKey, Transport};
use crate::{
   errors::PeerTransportError,
   hashes::{Hash, InfoHash},
   peers::messages::{Handshake, MAGIC_STRING},
   peers::PeerMessages,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use librqbit_utp::{UtpSocket, UtpSocketUdp, UtpStream};
use std::{collections::HashMap, net::SocketAddr, str::FromStr, sync::Arc};
use tokio::{
   io::{AsyncReadExt, AsyncWriteExt},
   sync::{
      mpsc::{self, Receiver, Sender},
      Mutex,
   },
   time::Instant,
};
use tracing::{debug, error, info, instrument, trace};

#[derive(Debug, Clone)]
pub enum TransportCommand {
   Connect { peer: Peer },
}

pub struct UtpTransportHandler {
   pub transport: UtpTransport,
   pub tx: Sender<TransportCommand>,
   pub rx: Receiver<TransportCommand>,
}

impl UtpTransportHandler {
   pub async fn new(
      id: Arc<Hash<20>>,
      info_hash: Arc<InfoHash>,
      socket_addr: Option<SocketAddr>,
   ) -> UtpTransportHandler {
      let (tx, rx) = mpsc::channel(32);
      UtpTransportHandler {
         transport: UtpTransport::new(id, info_hash, socket_addr).await,
         tx,
         rx,
      }
   }

   async fn handle_message(&mut self) -> Result<()> {
      while let Some(cmd) = self.rx.recv().await {
         let mut transport_clone = self.transport.clone();
         match cmd {
            TransportCommand::Connect { mut peer } => {
               trace!("Connecting to peer: {}", peer.ip);
               tokio::spawn(async move {
                  let res = transport_clone
                     .connect(&mut peer)
                     .await
                     .map_err(|e| error!("Error connecting to peer"));
               });
            }
         }
      }
      Ok(())
   }
}

#[derive(Clone)]
pub struct UtpTransport {
   pub socket: Arc<UtpSocketUdp>,
   pub id: Arc<Hash<20>>,
   pub info_hash: Arc<InfoHash>,
   pub peers: HashMap<PeerKey, Arc<Mutex<(Peer, UtpStream)>>>,
}

impl UtpTransport {
   pub async fn new(
      id: Arc<Hash<20>>,
      info_hash: Arc<InfoHash>,
      socket_addr: Option<SocketAddr>,
   ) -> UtpTransport {
      let socket =
         UtpSocket::new_udp(socket_addr.unwrap_or(SocketAddr::from_str("0.0.0.0:6881").unwrap()))
            .await
            .unwrap();

      UtpTransport {
         socket,
         id,
         info_hash,
         peers: HashMap::new(),
      }
   }
}

#[async_trait]
#[allow(unused_variables)]
impl Transport for UtpTransport {
   fn id(&self) -> Arc<Hash<20>> {
      self.id.clone()
   }

   fn info_hash(&self) -> Arc<InfoHash> {
      self.info_hash.clone()
   }

   /// Connects to & Handshakes a peer using the UTP protocol.
   ///
   /// Handshake should start with "character nineteen (decimal) followed by the string
   /// 'BitTorrent protocol'."
   /// All integers should be encoded as four bytes big-endian.
   /// After fixed headers, reserved bytes (0).
   /// 20 byte sha1 hash of bencoded form of info value ([info_hash](InfoHash)). If both sides don't send the
   /// same value, sever the connection.
   /// 20 byte peer id. If receiving side's id doesn't match the one the initiating side expects sever the connection.
   ///
   /// <https://wiki.theory.org/BitTorrentSpecification#Handshake>
   ///
   /// <https://www.bittorrent.org/beps/bep_0003.html>
   #[instrument(skip(self), fields(peer = %peer))]
   async fn connect(&mut self, peer: &mut Peer) -> Result<PeerKey, PeerTransportError> {
      trace!("Attempting connection...");

      // Connect to the peer
      let mut stream = self.socket.connect(peer.socket_addr()).await.map_err(|e| {
         error!("Failed to connect to peer {e}: {}", peer.socket_addr());
         PeerTransportError::ConnectionFailed(peer.socket_addr().to_string())
      })?;

      trace!("Connected to new peer");

      // Create and send handshake. We are unable to use self.send() because the entry in the hashtable with the current peers peer_id does not yet exist
      let handshake = Handshake::new(self.info_hash.clone(), self.id.clone());
      let handshake_bytes = stream.write_all(&handshake.to_bytes()).await.map_err(|e| {
         error!("Failed to write handshake to peer: {}", e);
         PeerTransportError::ConnectionFailed(peer.socket_addr().to_string())
      })?;
      trace!("Sent handshake to peer");

      // Calculate expected size for response
      // 1 byte + protocol + reserved + hashes
      const EXPECTED_SIZE: usize = 1 + MAGIC_STRING.len() + 8 + 40;
      let mut buf = [0u8; EXPECTED_SIZE];

      // Read response handshake
      stream.read_exact(&mut buf).await.map_err(|e| {
         error!("Failed to read handshake from peer: {}", e);
         PeerTransportError::ConnectionFailed(peer.socket_addr().to_string())
      })?;

      let handshake =
         Handshake::from_bytes(&buf).map_err(|e| PeerTransportError::Other(anyhow!("{e}")))?;

      let (_, new_peer) = self
         .validate_handshake(handshake, peer.socket_addr())
         .unwrap();

      // Store peer information
      let peer_id = new_peer.id.unwrap();
      peer.id = Some(peer_id);

      self.peers.insert(
         peer.socket_addr(),
         Arc::new(Mutex::new((peer.clone(), stream))),
      );

      info!(%peer, "Peer connected");

      Ok(peer.socket_addr())
   }

   // async fn recv_from_stream()

   async fn recv_raw(&mut self) -> Result<(PeerKey, Vec<u8>), PeerTransportError> {
      let mut socket = self.socket.accept().await.unwrap();
      // First 4 bytes is the big endian encoded length field and the 5th byte is a PeerMessage tag
      let mut buf = vec![0; 5];

      socket.read_exact(&mut buf).await.map_err(|e| {
         error!("Error occurred when reading the peer's response: {e}");
         PeerTransportError::InvalidPeerResponse("Error occured".into())
      })?;
      let addr = socket.remote_addr();
      trace!(message_type = buf[4], ip = %addr, "Recieved message headers, requesting rest...");
      let mut is_handshake = false;
      let length = if buf[0] as usize == MAGIC_STRING.len() && buf[1..5] == MAGIC_STRING[0..4] {
         // This is a handshake.
         // The length of a handshake is always 68 and we already have the
         // first 5 bytes of it, so we need 68 - 5 bytes (the current buffer length)
         is_handshake = true;
         68 - buf.len() as u32
      } else {
         // This is not a handshake
         // Non handshake messages have a length field from bytes 0-4
         u32::from_be_bytes(buf[..4].try_into().unwrap())
      };

      let mut rest = vec![0; length as usize];

      socket.read_exact(&mut rest).await.map_err(|e| {
         error!("Error occurred when reading the peer's response: {e}");
         PeerTransportError::InvalidPeerResponse("Error occured".into())
      })?;
      let full_length = length + buf.len() as u32;

      debug!(
         "Read {} action ({} bytes) from {} ",
         buf[4], full_length, addr
      );
      buf.extend_from_slice(&rest);

      if let Some(mutex) = self.peers.get_mut(&addr) {
         let peer = &mut mutex.lock().await.0;

         // Completely chat gippity generated code, do not trust
         // Update total bytes uploaded
         peer.bytes_uploaded += full_length as u64;

         // Calculate upload rate based on a time window
         let now = Instant::now();
         if let Some(last_time) = peer.last_message_received {
            let elapsed_secs = last_time.elapsed().as_secs_f64();
            if elapsed_secs > 0.0 {
               // Use an exponential moving average for smoother rate calculation
               const ALPHA: f64 = 0.3; // Smoothing factor (0.0-1.0)
               let current_rate = full_length as f64 / elapsed_secs;
               peer.download_rate = if peer.download_rate > 0.0 {
                  (ALPHA * current_rate) + ((1.0 - ALPHA) * peer.download_rate)
               } else {
                  current_rate
               };
            }
         }

         peer.last_message_received = Some(now);
      };

      if is_handshake {
         let handshake =
            Handshake::from_bytes(&buf).map_err(|e| PeerTransportError::Other(anyhow!("{e}")))?;
         let (handshake, peer) = self.validate_handshake(handshake, addr)?;

         trace!(peer_id = %peer.id.unwrap(), "Successfully validated handshake");

         // Creates a new handshake and sends it
         self
            .peers
            .insert(addr, Arc::new(Mutex::new((peer.clone(), socket))));
         self.send(addr, PeerMessages::Handshake(handshake)).await?;

         info!(%peer, "Peer connected");
      }

      Ok((addr, buf))
   }

   async fn send_raw(&mut self, to: PeerKey, message: Vec<u8>) -> Result<(), PeerTransportError> {
      trace!("Attempting to send message...");

      let (peer, socket) = &mut *self.peers.get_mut(&to).unwrap().lock().await;
      socket.write_all(&message).await.map_err(|e| {
         error!("Failed to send message to peer: {e}");
         PeerTransportError::MessageFailed
      })?;

      // Completely chat gippity generated code, do not trust
      // Update total bytes uploaded
      peer.bytes_uploaded += message.len() as u64;

      // Calculate upload rate based on a time window
      let now = Instant::now();
      if let Some(last_time) = peer.last_message_sent {
         let elapsed_secs = last_time.elapsed().as_secs_f64();
         if elapsed_secs > 0.0 {
            // Use an exponential moving average for smoother rate calculation
            const ALPHA: f64 = 0.3; // Smoothing factor (0.0-1.0)
            let current_rate = message.len() as f64 / elapsed_secs;
            peer.upload_rate = if peer.upload_rate > 0.0 {
               (ALPHA * current_rate) + ((1.0 - ALPHA) * peer.upload_rate)
            } else {
               current_rate
            };
         }
      }

      peer.last_message_sent = Some(now);
      Ok(())
   }

   async fn broadcast_raw(&mut self, message: Vec<u8>) -> Vec<Result<(), PeerTransportError>> {
      let mut results = vec![];

      // Collect the keys first to avoid the borrowing conflict
      let peer_ids: Vec<PeerKey> = self.peers.keys().cloned().collect();

      for id in peer_ids {
         results.push(self.send_raw(id, message.clone()).await);
      }
      results
   }

   async fn get_peer(&self, peer_key: PeerKey) -> Option<Peer> {
      let mutex = self.peers.get(&peer_key)?;
      let (peer, _) = &mut *mutex.lock().await;
      Some(peer.clone())
   }

   /// Drops peer from memory and closes the connection to it.
   ///
   /// Note: Does not send a close message, only removes peer & stream from memory.
   fn close(&mut self, peer_key: PeerKey) -> Result<()> {
      self.peers.remove(&peer_key);
      Ok(())
   }

   fn is_connected(&self, peer_key: PeerKey) -> bool {
      self.peers.contains_key(&peer_key)
   }
}

#[cfg(test)]
mod tests {

   use rand::random_range;
   use tokio::task::JoinSet;
   use tracing::info;
   use tracing_test::traced_test;

   use crate::{
      parser::{MagnetUri, MetaInfo},
      tracker::{http::HttpTracker, Tracker, TrackerTrait},
   };

   use super::*;
   #[tokio::test(flavor = "multi_thread", worker_threads = 50)]
   #[traced_test]
   async fn test_utp_peer_handshake() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/zenshuu.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MagnetUri::parse(contents).await.unwrap();

      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let info_hash = magnet.info_hash().unwrap();

            let announce_list: Vec<Tracker> = magnet
               .announce_list
               .unwrap_or_default()
               .into_iter()
               .filter_map(|e| match e {
                  Tracker::Http(_) => Some(e),
                  _ => None,
               })
               .collect();

            let announce_url = announce_list[0].uri();
            let port: u16 = random_range(1024..65535);

            let mut tracker = HttpTracker::new(
               announce_url,
               info_hash,
               Some(SocketAddr::from(([0, 0, 0, 0], port))),
            );
            let peer_id = tracker.peer_id;
            let peers = tracker.stream_peers().await.unwrap();

            // Skip if no peers found
            if peers.is_empty() {
               error!("No peers found, skipping test");
               return;
            }

            let info_hash_clone = Arc::new(info_hash);

            // Create a single uTP transport instance
            let mut utp_transport_handler = UtpTransportHandler::new(
               peer_id.into(),
               info_hash_clone,
               Some(SocketAddr::from(([0, 0, 0, 0], port))),
            )
            .await;

            let tx = utp_transport_handler.tx.clone();

            // Create a vector to hold all the join handles
            let mut join_set = JoinSet::new();

            // For each peer, spawn a task to connect
            for peer in peers {
               // Clone tx (see Tokio docs on why we need to clone tx: <https://tokio.rs/tokio/tutorial/channels>)
               let tx = tx.clone();
               join_set.spawn(async move {
                  let cmd = TransportCommand::Connect { peer };

                  match tx.send(cmd).await {
                     Ok(()) => Ok(peer_id),
                     Err(_) => Err("Connection failed".to_string()),
                  }
               });
            }

            tokio::spawn(async move {
               let _ = utp_transport_handler.handle_message().await;
            });

            let result = join_set.join_all().await;

            // let mut results = Vec::new();
            // let x = join_all(handles).await;

            // Calculate success rate
            // let total_peers = results.len();
            // let successful_peers = results.iter().filter(|r| r.is_ok()).count();
            // let success_rate = (successful_peers as f64) / (total_peers as f64);
            //
            // info!(
            //    "Connected to {}/{} peers ({}%)",
            //    successful_peers,
            //    total_peers,
            //    (success_rate * 100.0) as u32
            // );
            //
            // Print the errors for debugging
            // for (i, result) in results.iter().enumerate() {
            //    if let Err(e) = result {
            //       error!("Peer {} error: {}", i, e);
            //    }
            // }
            //
            // Test passes if more than 10% of connections succeeded
            // if success_rate > 0.1 {
            //    // Test passed
            //    return;
            // } else {
            //    panic!(
            //       "Less than 10% of peer connections succeeded ({}/{})",
            //       successful_peers, total_peers
            //    );
            // }
         }
         _ => panic!("Expected Torrent"),
      }
   }

   #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
   #[traced_test]
   async fn test_utp_incoming_handshake() {
      // Generate random info hash and peer IDs
      let info_hash = InfoHash::new(rand::random::<[u8; 20]>());
      let server_peer_id = Hash::new(rand::random::<[u8; 20]>());
      let client_peer_id = Hash::new(rand::random::<[u8; 20]>());

      // Use different ports for server and client
      let server_port: u16 = random_range(10000..20000);
      let client_port: u16 = random_range(20001..30000);

      let server_addr = SocketAddr::from(([127, 0, 0, 1], server_port));
      let client_addr = SocketAddr::from(([127, 0, 0, 1], client_port));

      // Create shared info hash for both sides
      let info_hash_arc = Arc::new(info_hash);
      let info_hash_clone = Arc::clone(&info_hash_arc);

      // Spawn the server in a separate task
      let server_handle = tokio::spawn(async move {
         // Create server transport
         let mut server_transport =
            UtpTransport::new(Arc::new(server_peer_id), info_hash_arc, Some(server_addr)).await;

         info!("Server listening on {}", server_addr);

         // Accept incoming connection
         match tokio::time::timeout(std::time::Duration::from_secs(5), server_transport.recv())
            .await
         {
            Ok(Ok((key, _))) => {
               let peer = server_transport.get_peer(key).await;
               assert!(peer.is_some(), "Peer should exist");

               let peer = peer.unwrap();

               info!(%peer, "Server accepted connection",);
               assert!(
                  peer.id.is_some(),
                  "Peer ID should be present after handshake"
               );
               assert_eq!(
                  peer.id.unwrap(),
                  client_peer_id,
                  "Received peer ID should match client's"
               );
               Ok(peer)
            }
            Ok(Err(e)) => Err(format!("Server error accepting connection: {}", e)),
            Err(_) => Err("Server timed out waiting for connection".to_string()),
         }
      });

      // Give the server a moment to start up
      tokio::time::sleep(std::time::Duration::from_millis(100)).await;

      // Create client and connect to server
      let client_handle = tokio::spawn(async move {
         // Create client transport
         let mut client_transport =
            UtpTransport::new(Arc::new(client_peer_id), info_hash_clone, Some(client_addr)).await;

         info!("Client connecting from {} to {}", client_addr, server_addr);

         // Create a peer representation of the server
         let mut server_peer = Peer::from_socket_addr(server_addr);

         // Connect to server
         match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client_transport.connect(&mut server_peer),
         )
         .await
         {
            Ok(Ok(peer_id)) => {
               info!(
                  "Client connected to server, received peer ID: {:?}",
                  peer_id
               );
               assert_eq!(
                  peer_id,
                  server_peer.socket_addr(),
                  "Received peer ID should match server's"
               );
               Ok(peer_id)
            }
            Ok(Err(e)) => Err(format!("Client error connecting: {}", e)),
            Err(_) => Err("Client timed out connecting to server".to_string()),
         }
      });

      // Wait for both operations to complete
      let server_result = server_handle.await.expect("Server task panicked");
      let client_result = client_handle.await.expect("Client task panicked");

      // Verify both sides completed successfully
      assert!(
         server_result.is_ok(),
         "Server error: {:?}",
         server_result.err()
      );
      assert!(
         client_result.is_ok(),
         "Client error: {:?}",
         client_result.err()
      );

      info!("uTP handshake test completed successfully");
   }
}
