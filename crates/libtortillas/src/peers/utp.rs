use super::{Peer, PeerKey, TransportProtocol};
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
   sync::Mutex,
   time::Instant,
};
use tracing::{debug, error, info, trace};

#[derive(Clone)]
pub struct UtpProtocol {
   pub socket: Arc<UtpSocketUdp>,
   pub peers: HashMap<PeerKey, Arc<Mutex<(Peer, UtpStream)>>>,
}

impl UtpProtocol {
   pub async fn new(socket_addr: Option<SocketAddr>) -> UtpProtocol {
      let socket_addr = socket_addr.unwrap_or(SocketAddr::from_str("0.0.0.0:6881").unwrap());
      trace!("Creating UTP socket at {}", socket_addr);
      let socket = UtpSocket::new_udp(socket_addr).await.unwrap();

      UtpProtocol {
         socket,
         peers: HashMap::new(),
      }
   }
}

#[async_trait]
#[allow(unused_variables)]
impl TransportProtocol for UtpProtocol {
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
   async fn connect_peer(
      &mut self,
      peer: &mut Peer,
      id: Arc<Hash<20>>,
      info_hash: Arc<InfoHash>,
   ) -> Result<PeerKey, PeerTransportError> {
      trace!("Attemping connection to {}", peer.socket_addr());

      // Connect to the peer
      let mut stream = self.socket.connect(peer.socket_addr()).await.map_err(|e| {
         error!("Failed to connect to peer {e}: {}", peer.socket_addr());
         PeerTransportError::ConnectionFailed(peer.socket_addr().to_string())
      })?;

      trace!("Connected to new peer");

      // Create and send handshake. We are unable to use self.send() because the entry in the hashtable with the current peers peer_id does not yet exist
      let handshake = Handshake::new(info_hash.clone(), id.clone());
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
         .validate_handshake(handshake, peer.socket_addr(), info_hash, id)
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

   async fn receive_data(
      &mut self,
      info_hash: Arc<InfoHash>,
      id: Arc<Hash<20>>,
   ) -> Result<(PeerKey, Vec<u8>), PeerTransportError> {
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
         if let Some(last_time) = peer.last_message_received {
            let elapsed_secs = last_time.elapsed().as_secs();
            if elapsed_secs > 0 {
               let current_rate = full_length as u64 / elapsed_secs;
               peer.download_rate = current_rate;
            }
         }

         let now = Instant::now();
         peer.last_message_received = Some(now);
      };

      if is_handshake {
         let handshake =
            Handshake::from_bytes(&buf).map_err(|e| PeerTransportError::Other(anyhow!("{e}")))?;
         let (handshake, peer) = self.validate_handshake(handshake, addr, info_hash, id)?;

         trace!(peer_id = %peer.id.unwrap(), "Successfully validated handshake");

         // Creates a new handshake and sends it
         self
            .peers
            .insert(addr, Arc::new(Mutex::new((peer.clone(), socket))));
         let message = PeerMessages::Handshake(handshake);
         self.send_data(addr, message.to_bytes().unwrap()).await?;

         info!(%peer, "Peer connected");
      }

      Ok((addr, buf))
   }

   async fn send_data(&mut self, to: PeerKey, message: Vec<u8>) -> Result<(), PeerTransportError> {
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
      if let Some(last_time) = peer.last_message_sent {
         let elapsed_secs = last_time.elapsed().as_secs();
         if elapsed_secs > 0 {
            let current_rate = message.len() as u64 / elapsed_secs;
            peer.upload_rate = current_rate;
         }
      }

      let now = Instant::now();
      peer.last_message_sent = Some(now);
      Ok(())
   }

   async fn get_connected_peer(&self, peer_key: PeerKey) -> Option<Peer> {
      let mutex = self.peers.get(&peer_key)?;
      let (peer, _) = &mut *mutex.lock().await;
      Some(peer.clone())
   }

   /// Drops peer from memory and closes the connection to it.
   ///
   /// Note: Does not send a close message, only removes peer & stream from memory.
   fn close_connection(&mut self, peer_key: PeerKey) -> Result<()> {
      self.peers.remove(&peer_key);
      Ok(())
   }

   fn is_peer_connected(&self, peer_key: PeerKey) -> bool {
      self.peers.contains_key(&peer_key)
   }
}

#[cfg(test)]
mod tests {

   use rand::random_range;
   use tokio::{sync::mpsc, task::JoinSet};
   use tracing::info;
   use tracing_test::traced_test;

   use crate::{
      parser::{MagnetUri, MetaInfo},
      peers::{transport_messages::TransportCommand, Transport, TransportHandler},
      tracker::{http::HttpTracker, Tracker, TrackerTrait},
   };

   use super::*;
   #[tokio::test(flavor = "multi_thread", worker_threads = 50)]
   #[traced_test]
   async fn test_utp_peer_handshake() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/test1.txt");
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
            let peers = tracker.get_peers().await.unwrap();

            // Skip if no peers found
            if peers.is_empty() {
               error!("No peers found, skipping test");
               return;
            }

            let info_hash_clone = Arc::new(info_hash);

            // Create a single uTP transport instance
            let client_peer_id = Hash::new(rand::random::<[u8; 20]>());
            let client_port: u16 = random_range(20001..30000);
            let client_addr = SocketAddr::from(([0, 0, 0, 0], client_port));

            info!("Running transport on {client_addr}");

            let protocol = UtpProtocol::new(Some(client_addr)).await;
            let mut utp_transport_handler =
               TransportHandler::new(protocol, Arc::new(client_peer_id), info_hash_clone);

            let tx = utp_transport_handler.tx.clone();

            // This is how UtpTransports should be handled async

            // Create a vector to hold all the join handles
            let mut join_set = JoinSet::new();

            // For each peer, spawn a task to connect
            let num_of_peers = peers.len();
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

            let (tx, mut rx) = mpsc::channel(100);

            // Start handling mpsc messages from the join set
            tokio::spawn(async move {
               utp_transport_handler.handle_commands(tx).await.unwrap();
            });

            // Await the join_set.spawn()
            join_set.join_all().await;

            // Collect responses from handle_message
            tokio::spawn(async move {
               let mut total_peers_seen = 0;
               while let Some(_res) = rx.recv().await {
                  total_peers_seen += 1;
                  if num_of_peers == total_peers_seen {
                     break;
                  }
               }
            })
            .await
            .unwrap();
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

      let server_addr = SocketAddr::from(([127, 0, 0, 1], 9881));
      let client_addr = SocketAddr::from(([127, 0, 0, 1], 9882));

      // Create shared info hash for both sides
      let info_hash_arc = Arc::new(info_hash);
      let info_hash_clone = Arc::clone(&info_hash_arc);

      let mut set = JoinSet::new();

      // Spawn the server in a separate task
      set.spawn(async move {
         // Create server transport
         info!("Server listening on {}", server_addr);
         let protocol = UtpProtocol::new(Some(server_addr)).await;
         let mut server_transport =
            TransportHandler::new(protocol, Arc::new(server_peer_id), info_hash_arc);
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
               Ok(peer.id)
            }
            Ok(Err(e)) => Err(format!("Server error accepting connection: {}", e)),
            Err(_) => Err("Server timed out waiting for connection".to_string()),
         }
      });

      // Create client and connect to server
      set.spawn(async move {
         // Give the server a moment to start up
         tokio::time::sleep(std::time::Duration::from_millis(100)).await;

         // Create client transport
         let protocol = UtpProtocol::new(Some(client_addr)).await;
         let mut client_transport =
            TransportHandler::new(protocol, Arc::new(client_peer_id), info_hash_clone);

         // Create a peer representation of the server
         let mut server_peer = Peer::from_socket_addr(server_addr);

         // Connect to server
         match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client_transport.connect(&mut server_peer),
         )
         .await
         {
            Ok(Ok(peer_key)) => {
               info!(
                  "Client connected to server, received peer ID: {:?}",
                  peer_key
               );
               assert_eq!(
                  peer_key,
                  server_peer.socket_addr(),
                  "Received peer ID should match server's"
               );
               let peer = client_transport.get_peer(peer_key).await.unwrap();

               Ok(peer.id)
            }
            Ok(Err(e)) => Err(format!("Client error connecting: {}", e)),
            Err(_) => Err("Client timed out connecting to server".to_string()),
         }
      });

      // Wait for both operations to complete
      let set = set.join_all().await;
      let server_result = &set[0];
      let client_result = &set[1];

      // Verify both sides completed successfully
      assert!(
         server_result.is_ok(),
         "Server error: {:?}",
         server_result.clone().err()
      );
      assert!(
         client_result.is_ok(),
         "Client error: {:?}",
         client_result.clone().err()
      );

      info!("uTP handshake test completed successfully");
   }
}
