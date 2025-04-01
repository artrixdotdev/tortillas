// utp/manager.rs
use crate::{
   errors::PeerTransportError,
   hashes::{Hash, InfoHash},
   peers::{
      Peer, PeerKey, PeerMessages, TransportRequest,
      messages::{Handshake, MAGIC_STRING},
   },
};
use anyhow::{Result, anyhow};
use librqbit_utp::{UtpSocketUdp, UtpStream};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::{
   io::{AsyncReadExt, AsyncWriteExt},
   sync::{Mutex, mpsc},
   time::Instant,
};
use tracing::{debug, error, info, trace};

pub struct UtpTransportManager {
   socket: Arc<UtpSocketUdp>,
   id: Arc<Hash<20>>,
   info_hash: Arc<InfoHash>,
   peers: HashMap<PeerKey, Arc<Mutex<(Peer, UtpStream)>>>,
   request_rx: mpsc::Receiver<TransportRequest>,
}

impl UtpTransportManager {
   pub fn new(
      id: Arc<Hash<20>>,
      info_hash: Arc<InfoHash>,
      socket: Arc<UtpSocketUdp>,
      request_rx: mpsc::Receiver<TransportRequest>,
   ) -> Self {
      Self {
         socket,
         id,
         info_hash,
         peers: HashMap::new(),
         request_rx,
      }
   }

   pub async fn start(mut self) {
      loop {
         match self.request_rx.recv().await {
            Some(request) => self.handle_request(request).await,
            None => break, // Channel closed, manager should shut down
         }
      }
      info!("UtpTransportManager shutting down");
   }

   async fn handle_request(&mut self, request: TransportRequest) {
      match request {
         TransportRequest::Connect { peer, response } => {
            let result = self.connect(peer).await;
            let _ = response.send(result); // Ignore if receiver dropped
         }
         TransportRequest::SendRaw {
            to,
            message,
            response,
         } => {
            let result = self.send_raw(to, message).await;
            let _ = response.send(result);
         }
         TransportRequest::BroadcastRaw { message, response } => {
            let result = self.broadcast_raw(message).await;
            let _ = response.send(result);
         }
         TransportRequest::RecvRaw { response } => {
            let result = self.recv_raw().await;
            let _ = response.send(result);
         }
         TransportRequest::GetPeer { peer_key, response } => {
            let result = self.get_peer(peer_key).await;
            let _ = response.send(result);
         }
         TransportRequest::Close { peer_key, response } => {
            let result = self.close(peer_key);
            let _ = response.send(result);
         }
         TransportRequest::IsConnected { peer_key, response } => {
            let result = self.is_connected(peer_key);
            let _ = response.send(result);
         }
         TransportRequest::GetId { response } => {
            let _ = response.send(self.id.clone());
         }
         TransportRequest::GetInfoHash { response } => {
            let _ = response.send(self.info_hash.clone());
         }
      }
   }

   // Implement all the Transport trait methods but as regular async methods

   async fn connect(&mut self, mut peer: Peer) -> Result<PeerKey, PeerTransportError> {
      trace!("Attempting connection to {}...", peer);

      // Connect to the peer
      let mut stream = self.socket.connect(peer.socket_addr()).await.map_err(|e| {
         error!("Failed to connect to peer {e}: {}", peer.socket_addr());
         PeerTransportError::ConnectionFailed(peer.socket_addr().to_string())
      })?;

      trace!("Connected to new peer");

      // Create and send handshake
      let handshake = Handshake::new(self.info_hash.clone(), self.id.clone());
      stream.write_all(&handshake.to_bytes()).await.map_err(|e| {
         error!("Failed to write handshake to peer: {}", e);
         PeerTransportError::ConnectionFailed(peer.socket_addr().to_string())
      })?;
      trace!("Sent handshake to peer");

      // Calculate expected size for response
      const EXPECTED_SIZE: usize = 1 + MAGIC_STRING.len() + 8 + 40;
      let mut buf = [0u8; EXPECTED_SIZE];

      // Read response handshake
      stream.read_exact(&mut buf).await.map_err(|e| {
         error!("Failed to read handshake from peer: {}", e);
         PeerTransportError::ConnectionFailed(peer.socket_addr().to_string())
      })?;

      let handshake =
         Handshake::from_bytes(&buf).map_err(|e| PeerTransportError::Other(anyhow!("{e}")))?;

      let (_, new_peer) = self.validate_handshake(handshake, peer.socket_addr())?;

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

   async fn send_raw(&mut self, to: PeerKey, message: Vec<u8>) -> Result<(), PeerTransportError> {
      trace!("Attempting to send message...");

      let peer_mutex = self.peers.get(&to).ok_or_else(|| {
         error!("Peer not found: {}", to);
         PeerTransportError::PeerNotFound(to.to_string())
      })?;

      let (peer, socket) = &mut *peer_mutex.lock().await;
      socket.write_all(&message).await.map_err(|e| {
         error!("Failed to send message to peer: {e}");
         PeerTransportError::MessageFailed
      })?;

      // Update peer statistics
      peer.bytes_uploaded += message.len() as u64;

      let now = Instant::now();
      if let Some(last_time) = peer.last_message_sent {
         let elapsed_secs = last_time.elapsed().as_secs_f64();
         if elapsed_secs > 0.0 {
            const ALPHA: f64 = 0.3;
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
      let peer_ids: Vec<PeerKey> = self.peers.keys().cloned().collect();

      for id in peer_ids {
         results.push(self.send_raw(id, message.clone()).await);
      }
      results
   }

   async fn recv_raw(&mut self) -> Result<(PeerKey, Vec<u8>), PeerTransportError> {
      let mut socket = self.socket.accept().await.unwrap();
      let mut buf = vec![0; 5];

      socket.read_exact(&mut buf).await.map_err(|e| {
         error!("Error reading peer's response: {e}");
         PeerTransportError::InvalidPeerResponse("Read error".into())
      })?;

      let addr = socket.remote_addr();
      trace!(message_type = buf[4], ip = %addr, "Received message headers");

      let mut is_handshake = false;
      let length = if buf[0] as usize == MAGIC_STRING.len() && buf[1..5] == MAGIC_STRING[0..4] {
         is_handshake = true;
         68 - buf.len() as u32
      } else {
         u32::from_be_bytes(buf[..4].try_into().unwrap())
      };

      let mut rest = vec![0; length as usize];
      socket.read_exact(&mut rest).await.map_err(|e| {
         error!("Error reading peer's response: {e}");
         PeerTransportError::InvalidPeerResponse("Read error".into())
      })?;

      let full_length = length + buf.len() as u32;
      debug!(
         "Read {} action ({} bytes) from {}",
         buf[4], full_length, addr
      );
      buf.extend_from_slice(&rest);

      // Update peer statistics if it exists
      if let Some(mutex) = self.peers.get_mut(&addr) {
         let peer = &mut mutex.lock().await.0;
         peer.bytes_uploaded += full_length as u64;

         let now = Instant::now();
         if let Some(last_time) = peer.last_message_received {
            let elapsed_secs = last_time.elapsed().as_secs_f64();
            if elapsed_secs > 0.0 {
               const ALPHA: f64 = 0.3;
               let current_rate = full_length as f64 / elapsed_secs;
               peer.download_rate = if peer.download_rate > 0.0 {
                  (ALPHA * current_rate) + ((1.0 - ALPHA) * peer.download_rate)
               } else {
                  current_rate
               };
            }
         }
         peer.last_message_received = Some(now);
      }

      if is_handshake {
         let handshake =
            Handshake::from_bytes(&buf).map_err(|e| PeerTransportError::Other(anyhow!("{e}")))?;

         let (handshake, peer) = self.validate_handshake(handshake, addr)?;
         trace!(peer_id = %peer.id.unwrap(), "Validated handshake");

         self
            .peers
            .insert(addr, Arc::new(Mutex::new((peer.clone(), socket))));

         // Create a new PeerMessages::Handshake and send it
         let handshake_msg = PeerMessages::Handshake(handshake);
         let handshake_bytes = handshake_msg.to_bytes().map_err(|e| {
            PeerTransportError::Other(anyhow!("Failed to serialize handshake: {e}"))
         })?;

         self.send_raw(addr, handshake_bytes).await?;
         info!(%peer, "Peer connected");
      }

      Ok((addr, buf))
   }

   async fn get_peer(&self, peer_key: PeerKey) -> Option<Peer> {
      let mutex = self.peers.get(&peer_key)?;
      let (peer, _) = &*mutex.lock().await;
      Some(peer.clone())
   }

   fn close(&mut self, peer_key: PeerKey) -> Result<(), anyhow::Error> {
      self.peers.remove(&peer_key);
      Ok(())
   }

   fn is_connected(&self, peer_key: PeerKey) -> bool {
      self.peers.contains_key(&peer_key)
   }

   fn validate_handshake(
      &self,
      received_handshake: Handshake,
      peer_addr: SocketAddr,
   ) -> Result<(Handshake, Peer), PeerTransportError> {
      let peer_id = received_handshake.peer_id;

      // Validate protocol string
      if MAGIC_STRING != received_handshake.protocol {
         error!("Invalid magic string from peer {}", peer_addr);
         return Err(PeerTransportError::InvalidMagicString {
            received: String::from_utf8_lossy(&received_handshake.protocol).into(),
            expected: String::from_utf8_lossy(MAGIC_STRING).into(),
         });
      }

      // Validate info hash
      if &*self.info_hash != &*received_handshake.info_hash {
         error!("Invalid info hash from peer {}", peer_addr);
         return Err(PeerTransportError::InvalidInfoHash {
            received: received_handshake.info_hash.to_hex(),
            expected: self.info_hash.to_hex(),
         });
      }

      trace!("Received valid handshake from {}", peer_addr);

      let mut peer = Peer::from_socket_addr(peer_addr);
      peer.id = Some(*peer_id);

      let handshake = Handshake::new(self.info_hash.clone(), self.id.clone());
      Ok((handshake, peer))
   }
}
