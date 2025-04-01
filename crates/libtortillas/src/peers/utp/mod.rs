use crate::{
   errors::PeerTransportError,
   hashes::{Hash, InfoHash},
   peers::{Peer, PeerKey, Transport, TransportRequest},
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use librqbit_utp::UtpSocket;
use std::{net::SocketAddr, str::FromStr, sync::Arc};
use tokio::sync::{mpsc, oneshot};

mod manager;

#[derive(Clone)]
pub struct UtpTransport {
   id: Arc<Hash<20>>,
   info_hash: Arc<InfoHash>,
   request_tx: mpsc::Sender<TransportRequest>,
}

impl UtpTransport {
   pub async fn new(
      id: Arc<Hash<20>>,
      info_hash: Arc<InfoHash>,
      socket_addr: Option<SocketAddr>,
   ) -> Result<Self, anyhow::Error> {
      let socket_addr = socket_addr.unwrap_or(SocketAddr::from_str("0.0.0.0:6881").unwrap());

      // Create the UTP socket
      let socket = UtpSocket::new_udp(socket_addr)
         .await
         .map_err(|e| anyhow!("Failed to create UTP socket: {}", e))?;

      // Create the channel for communication with the manager
      let (request_tx, request_rx) = mpsc::channel(100);

      // Create and start the manager in a separate task
      let manager =
         manager::UtpTransportManager::new(id.clone(), info_hash.clone(), socket, request_rx);

      tokio::spawn(async move {
         manager.start().await;
      });

      Ok(Self {
         id,
         info_hash,
         request_tx,
      })
   }

   async fn send_request<T>(&self, request: TransportRequest) -> Result<T, anyhow::Error>
   where
      T: Send + 'static,
   {
      let (_, response_rx) = oneshot::channel();

      self
         .request_tx
         .send(request)
         .await
         .map_err(|_| anyhow!("Transport manager is no longer running"))?;

      response_rx
         .await
         .map_err(|_| anyhow!("Transport manager dropped response channel"))
   }
}

#[async_trait]
impl Transport for UtpTransport {
   fn id(&self) -> Arc<Hash<20>> {
      self.id.clone()
   }

   fn info_hash(&self) -> Arc<InfoHash> {
      self.info_hash.clone()
   }

   async fn connect(&mut self, peer: &mut Peer) -> Result<PeerKey, PeerTransportError> {
      let (response_tx, response_rx) = oneshot::channel();

      let request = TransportRequest::Connect {
         peer: peer.clone(),
         response: response_tx,
      };

      self
         .request_tx
         .send(request)
         .await
         .map_err(|_| PeerTransportError::TransportClosed)?;

      let result = response_rx
         .await
         .map_err(|_| PeerTransportError::TransportClosed)?;

      // If connection successful, update the peer with the connection result
      if let Ok(peer_key) = &result {
         // Request the updated peer information from the manager
         let (get_tx, get_rx) = oneshot::channel();
         self
            .request_tx
            .send(TransportRequest::GetPeer {
               peer_key: *peer_key,
               response: get_tx,
            })
            .await
            .map_err(|_| PeerTransportError::TransportClosed)?;

         if let Some(updated_peer) = get_rx
            .await
            .map_err(|_| PeerTransportError::TransportClosed)?
         {
            // Only update ID as that's what's being set in the manager
            peer.id = updated_peer.id;
         }
      }

      result
   }

   async fn send_raw(&mut self, to: PeerKey, message: Vec<u8>) -> Result<(), PeerTransportError> {
      let (response_tx, response_rx) = oneshot::channel();

      let request = TransportRequest::SendRaw {
         to,
         message,
         response: response_tx,
      };

      self
         .request_tx
         .send(request)
         .await
         .map_err(|_| PeerTransportError::TransportClosed)?;

      response_rx
         .await
         .map_err(|_| PeerTransportError::TransportClosed)?
   }

   async fn broadcast_raw(&mut self, message: Vec<u8>) -> Vec<Result<(), PeerTransportError>> {
      let (response_tx, response_rx) = oneshot::channel();

      let request = TransportRequest::BroadcastRaw {
         message,
         response: response_tx,
      };

      match self.request_tx.send(request).await {
         Ok(_) => match response_rx.await {
            Ok(results) => results,
            Err(_) => vec![Err(PeerTransportError::TransportClosed)],
         },
         Err(_) => vec![Err(PeerTransportError::TransportClosed)],
      }
   }

   async fn recv_raw(&mut self) -> Result<(PeerKey, Vec<u8>), PeerTransportError> {
      let (response_tx, response_rx) = oneshot::channel();

      let request = TransportRequest::RecvRaw {
         response: response_tx,
      };

      self
         .request_tx
         .send(request)
         .await
         .map_err(|_| PeerTransportError::TransportClosed)?;

      response_rx
         .await
         .map_err(|_| PeerTransportError::TransportClosed)?
   }

   async fn get_peer(&self, peer_key: PeerKey) -> Option<Peer> {
      let (response_tx, response_rx) = oneshot::channel();

      let request = TransportRequest::GetPeer {
         peer_key,
         response: response_tx,
      };

      match self.request_tx.send(request).await {
         Ok(_) => response_rx.await.unwrap_or(None),
         Err(_) => None,
      }
   }

   fn close(&mut self, peer_key: PeerKey) -> Result<(), anyhow::Error> {
      // This is not async, so we need to block on the future
      let (response_tx, response_rx) = oneshot::channel();

      let request = TransportRequest::Close {
         peer_key,
         response: response_tx,
      };

      // Use a blocking runtime to send the request and wait for response
      let rt = tokio::runtime::Handle::current();
      rt.block_on(async {
         self
            .request_tx
            .send(request)
            .await
            .map_err(|_| anyhow!("Transport manager is no longer running"))?;

         response_rx
            .await
            .map_err(|_| anyhow!("Transport manager dropped response channel"))
      })
      .unwrap()
      .unwrap();
      Ok(())
   }

   fn is_connected(&self, peer_key: PeerKey) -> bool {
      // This is not async, so we need to block on the future
      let (response_tx, response_rx) = oneshot::channel();

      let request = TransportRequest::IsConnected {
         peer_key,
         response: response_tx,
      };

      // Use a blocking runtime to send the request and wait for response
      let rt = tokio::runtime::Handle::current();
      match rt.block_on(async {
         self.request_tx.send(request).await.map_err(|_| false)?;

         response_rx.await.map_err(|_| false)
      }) {
         Ok(result) => result,
         Err(_) => false,
      }
   }
}

#[cfg(test)]
mod tests {
   use futures::future::join_all;
   use rand::random_range;
   use std::net::SocketAddr;
   use std::sync::Arc;
   use tokio::time::{Duration, timeout};
   use tracing::{error, info};
   use tracing_test::traced_test;

   use crate::{
      hashes::{Hash, InfoHash},
      parser::{MagnetUri, MetaInfo},
      peers::{Peer, messages::PeerMessages},
      tracker::{TrackerTrait, udp::UdpTracker},
   };

   use super::*;

   /// Tests connecting to multiple peers concurrently from a single UTP transport
   #[tokio::test(flavor = "multi_thread", worker_threads = 16)]
   #[traced_test]
   async fn test_utp_peer_handshake() -> anyhow::Result<()> {
      // Load the magnet URI for testing
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await?;

      let metainfo = MagnetUri::parse(contents).await?;

      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let info_hash = magnet.info_hash().unwrap();
            let announce_list = magnet.announce_list.unwrap();
            let announce_url = announce_list[0].uri();
            let port: u16 = random_range(1024..65535);

            // Get peers from the tracker
            let mut tracker = UdpTracker::new(
               announce_url,
               None,
               info_hash,
               Some(SocketAddr::from(([0, 0, 0, 0], port))),
            )
            .await?;

            let peer_id = tracker.peer_id;
            let peers = tracker.stream_peers().await?;

            // Skip if no peers found
            if peers.is_empty() {
               error!("No peers found, skipping test");
               return Ok(());
            }

            // Create a single uTP transport
            let utp_transport = UtpTransport::new(
               peer_id.into(),
               Arc::new(info_hash),
               Some(SocketAddr::from(([0, 0, 0, 0], port))),
            )
            .await?;

            info!(
               "Created transport, starting concurrent connections to {} peers",
               peers.len()
            );

            // Connect to multiple peers concurrently
            let connection_futures = peers
               .into_iter()
               .enumerate()
               .map(|(i, mut peer)| {
                  // Clone the peer for debug output
                  let peer_addr = peer.socket_addr();

                  // Create a new transport for each connection
                  // This is efficient since each client is just a thin wrapper around
                  // a channel to the shared manager
                  let mut transport_clone = utp_transport.clone();

                  // Create a timeout future for this connection
                  timeout(Duration::from_secs(10), async move {
                     info!("Starting connection attempt {} to {}", i, peer_addr);
                     let result = transport_clone.connect(&mut peer).await;
                     (peer_addr, result)
                  })
               })
               .collect::<Vec<_>>();

            // Wait for all connections to complete
            let results = join_all(connection_futures).await;

            // Process results
            let mut successful = 0;
            let mut timeouts = 0;
            let mut failures = 0;

            for result in &results {
               match result {
                  Ok((addr, Ok(_))) => {
                     successful += 1;
                     info!("Successfully connected to {}", addr);
                  }
                  Ok((addr, Err(e))) => {
                     failures += 1;
                     error!("Failed to connect to {}: {}", addr, e);
                  }
                  Err(_) => {
                     timeouts += 1;
                     error!("Connection timed out");
                  }
               }
            }

            let total = results.len();
            let success_rate = (successful as f64) / (total as f64);

            info!(
               "Connected to {}/{} peers ({}%) - Failures: {}, Timeouts: {}",
               successful,
               total,
               (success_rate * 100.0) as u32,
               failures,
               timeouts
            );

            // For this test, we consider it a success if at least 10% of connections worked
            // This accounts for peers that might be offline
            assert!(
               success_rate > 0.1,
               "Less than 10% of connections succeeded ({}/{})",
               successful,
               total
            );

            Ok(())
         }
         _ => panic!("Expected MagnetUri"),
      }
   }

   /// Tests bidirectional handshake between two transports
   #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
   #[traced_test]
   async fn test_utp_incoming_handshake() -> anyhow::Result<()> {
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
            UtpTransport::new(Arc::new(server_peer_id), info_hash_arc, Some(server_addr))
               .await
               .unwrap();

         info!("Server listening on {}", server_addr);

         // Accept incoming connection (by receiving a message)
         match timeout(Duration::from_secs(5), async {
            // Use recv() to wait for and process an incoming connection
            let (peer_key, _) = server_transport.recv().await?;
            let peer = server_transport
               .get_peer(peer_key)
               .await
               .ok_or_else(|| PeerTransportError::PeerNotFound(peer_key.to_string()))?;

            Ok::<_, PeerTransportError>(peer)
         })
         .await
         {
            Ok(Ok(peer)) => {
               info!(?peer, "Server accepted connection");
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
      tokio::time::sleep(Duration::from_millis(100)).await;

      // Create client and connect to server
      let client_handle = tokio::spawn(async move {
         // Create client transport
         let mut client_transport =
            UtpTransport::new(Arc::new(client_peer_id), info_hash_clone, Some(client_addr))
               .await
               .unwrap();

         info!("Client connecting from {} to {}", client_addr, server_addr);

         // Create a peer representation of the server
         let mut server_peer = Peer::from_socket_addr(server_addr);

         // Connect to server
         match timeout(
            Duration::from_secs(5),
            client_transport.connect(&mut server_peer),
         )
         .await
         {
            Ok(Ok(peer_key)) => {
               info!("Client connected to server, peer key: {:?}", peer_key);

               // Verify the peer key matches the server address
               assert_eq!(
                  peer_key, server_addr,
                  "Peer key should match server address"
               );

               // Validate peer id was set correctly
               let peer = client_transport.get_peer(peer_key).await.unwrap();
               assert_eq!(
                  peer.id.unwrap(),
                  server_peer_id,
                  "Server's peer ID should match expected"
               );

               Ok(peer_key)
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
      Ok(())
   }

   /// Test concurrent message exchange between multiple peers
   #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
   #[traced_test]
   async fn test_concurrent_message_exchange() -> anyhow::Result<()> {
      // Generate random info hash and peer IDs
      let info_hash = InfoHash::new(rand::random::<[u8; 20]>());
      let transport_peer_id = Hash::new(rand::random::<[u8; 20]>());

      // Create shared info hash
      let info_hash_arc = Arc::new(info_hash);

      // Setup multiple virtual peer connections
      let num_peers = 5;
      let base_port = random_range(30000..40000);

      // Create main transport
      let main_addr = SocketAddr::from(([127, 0, 0, 1], base_port));
      let mut main_transport = UtpTransport::new(
         Arc::new(transport_peer_id),
         info_hash_arc.clone(),
         Some(main_addr),
      )
      .await?;

      info!("Main transport created at {}", main_addr);

      // Create peer transports concurrently
      let mut peer_handles = Vec::new();

      for i in 0..num_peers {
         let peer_port = base_port + i as u16 + 1;
         let peer_addr = SocketAddr::from(([127, 0, 0, 1], peer_port));
         let peer_id = Hash::new(rand::random::<[u8; 20]>());
         let info_hash_clone = info_hash_arc.clone();

         // Spawn peer task
         let peer_handle = tokio::spawn(async move {
            // Create peer transport
            let mut peer_transport =
               UtpTransport::new(Arc::new(peer_id), info_hash_clone, Some(peer_addr))
                  .await
                  .unwrap();

            // Create a representation of the main transport as a peer
            let mut main_peer = Peer::from_socket_addr(main_addr);

            // Connect to main transport
            match timeout(
               Duration::from_secs(5),
               peer_transport.connect(&mut main_peer),
            )
            .await
            {
               Ok(Ok(peer_key)) => {
                  info!("Peer {} connected to main transport", peer_addr);

                  // Send a keep-alive message
                  let keep_alive = PeerMessages::KeepAlive;
                  peer_transport.send(peer_key, keep_alive).await?;

                  // Wait for a message from the main transport
                  let (_, message) =
                     timeout(Duration::from_secs(5), peer_transport.recv()).await??;

                  info!("Peer {} received message: {:?}", peer_addr, message);

                  Ok::<_, anyhow::Error>(peer_addr)
               }
               Ok(Err(e)) => Err(anyhow::anyhow!(
                  "Peer {} connection error: {}",
                  peer_addr,
                  e
               )),
               Err(_) => Err(anyhow::anyhow!("Peer {} connection timeout", peer_addr)),
            }
         });

         peer_handles.push(peer_handle);
      }

      // Main transport logic - handle multiple connections
      let main_handle = tokio::spawn(async move {
         let mut connected_peers = Vec::new();

         // Accept connections from all peers
         for _ in 0..num_peers {
            match timeout(Duration::from_secs(10), main_transport.recv()).await {
               Ok(Ok((peer_key, message))) => {
                  info!("Main transport received connection from {}", peer_key);
                  connected_peers.push(peer_key);
               }
               Ok(Err(e)) => {
                  error!("Main transport error: {}", e);
                  return Err(anyhow::anyhow!("Main transport error: {}", e));
               }
               Err(_) => {
                  return Err(anyhow::anyhow!("Timeout waiting for peer connections"));
               }
            }
         }

         info!(
            "Main transport connected to {} peers",
            connected_peers.len()
         );

         // Broadcast a message to all connected peers
         let interested_msg = PeerMessages::Interested;
         let broadcast_results = main_transport.broadcast(&interested_msg).await;

         // Check broadcast results
         let successful_broadcasts = broadcast_results.iter().filter(|r| r.is_ok()).count();

         info!(
            "Successfully sent broadcast to {}/{} peers",
            successful_broadcasts,
            connected_peers.len()
         );

         assert_eq!(
            successful_broadcasts,
            connected_peers.len(),
            "Should successfully broadcast to all connected peers"
         );

         Ok::<_, anyhow::Error>(connected_peers)
      });

      // Wait for peer results
      let peer_results = join_all(peer_handles).await;
      let successful_peers = peer_results
         .iter()
         .filter(|r| r.as_ref().map_or(false, |pr| pr.is_ok()))
         .count();

      // Wait for main transport result
      let main_result = main_handle.await?;

      // Validate results
      match main_result {
         Ok(connected_peers) => {
            info!(
               "Main transport connected to {} peers",
               connected_peers.len()
            );
            assert_eq!(
               connected_peers.len(),
               num_peers,
               "Main transport should connect to all {} peers",
               num_peers
            );
         }
         Err(e) => {
            return Err(anyhow::anyhow!("Main transport error: {}", e));
         }
      }

      assert_eq!(
         successful_peers, num_peers,
         "All {} peers should connect successfully",
         num_peers
      );

      info!("Concurrent message exchange test completed successfully");
      Ok(())
   }
}
