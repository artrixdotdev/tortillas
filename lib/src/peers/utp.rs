use super::{Peer, PeerMessages, Transport};
use crate::{
   errors::PeerTransportError,
   hashes::{Hash, InfoHash},
};
use anyhow::Result;
use async_trait::async_trait;
use librqbit_utp::{UtpSocket, UtpSocketUdp, UtpStream};
use std::{collections::HashMap, net::SocketAddr, str::FromStr, sync::Arc, time::Duration};
use tokio::{
   io::{AsyncReadExt, AsyncWriteExt},
   time::Instant,
};
use tracing::{error, instrument, trace};

const MAGIC_STRING: &[u8; 19] = b"BitTorrent protocol";

pub struct UtpTransport<'a> {
   pub socket: Arc<UtpSocketUdp>,
   pub id: Arc<Hash<20>>,
   pub info_hash: Arc<InfoHash>,
   pub peers: HashMap<Hash<20>, (&'a mut Peer, &'a mut UtpStream)>,
}

impl<'a> UtpTransport<'a> {
   pub async fn new(id: Arc<Hash<20>>, info_hash: Arc<InfoHash>) -> UtpTransport<'a> {
      let socket = UtpSocket::new_udp(SocketAddr::from_str("0.0.0.0:0").unwrap())
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
impl<'a> Transport for UtpTransport<'a> {
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
   /// 20 byte sha1 hash of bencoded form of info value (info_hash). If both sides don't send the
   /// same value, sever the connection.
   /// 20 byte peer id. If receiving side's id doesn't match the one the initiating side expects sever the connection.
   ///
   /// <https://wiki.theory.org/BitTorrentSpecification#Handshake>
   ///
   /// <https://www.bittorrent.org/beps/bep_0003.html>
   #[instrument(skip(self), fields(peer = %peer))]
   async fn connect(&mut self, peer: &mut Peer) -> Result<Hash<20>, PeerTransportError> {
      trace!("Attmepting connection...");

      let mut stream = self.socket.connect(peer.socket_addr()).await.map_err(|e| {
         error!("Failed to connect to peer {}: {}", peer.socket_addr(), e);
         PeerTransportError::ConnectionFailed(peer.socket_addr().to_string())
      })?;
      trace!("Connected to new peer");

      // Create headers
      let mut headers = Vec::with_capacity(68);

      headers.extend_from_slice(&[MAGIC_STRING.len() as u8]); // length of MAGIC_STRING as a single raw byte
      headers.extend_from_slice(MAGIC_STRING);
      headers.extend_from_slice(&[0u8; 8]); // Reserved bytes
      headers.extend_from_slice(self.info_hash.as_bytes());
      headers.extend_from_slice(self.id.as_bytes());

      stream.write_all(&headers).await.map_err(|e| {
         error!("Failed to write headers to peer:  {}", e);
         PeerTransportError::ConnectionFailed(peer.socket_addr().to_string())
      })?;
      trace!("Sent headers to peer");

      let mut buf = [0u8; 68];
      stream.read_exact(&mut buf).await.map_err(|e| {
         error!("Failed to read headers from peer: {}", e);
         PeerTransportError::ConnectionFailed(peer.socket_addr().to_string())
      })?;

      // +1 byte for the length of MAGIC_STRING
      let name_length = buf[0] as usize;
      let magic = &buf[1..name_length + 1];
      trace!(
         "Received magic string: {:?}",
         String::from_utf8_lossy(magic)
      );
      if magic != MAGIC_STRING {
         error!("Invalid magic string received from peer");
         return Err(PeerTransportError::InvalidMagicString);
      }

      let info_hash: InfoHash = Hash::new(buf[28..48].try_into().unwrap());
      if info_hash.to_hex() != self.info_hash.to_hex() {
         error!("Invalid info hash received from peer");
         return Err(PeerTransportError::InvalidInfoHash {
            received: info_hash.to_hex(),
            expected: self.info_hash.to_hex(),
         });
      }

      let peer_id: Hash<20> = Hash::new(buf[48..68].try_into().unwrap());

      peer.id = Some(peer_id);
      peer.last_seen = Instant::now();

      Ok(peer_id)
   }

   async fn broadcast(&mut self, message: &PeerMessages) -> Result<()> {
      Ok(())
   }

   async fn send(&mut self, to: Hash<20>, message: &PeerMessages) -> Result<()> {
      Ok(())
   }

   async fn recv(&mut self, timeout: Option<Duration>) -> Result<PeerMessages> {
      Ok(PeerMessages::Choke)
   }

   async fn close(&mut self) -> Result<()> {
      Ok(())
   }

   fn is_connected(&self) -> bool {
      false
   }
}

#[cfg(test)]
mod tests {
   use tracing::info;
   use tracing_test::traced_test;

   use crate::{
      parser::{MagnetUri, MetaInfo},
      tracker::{TrackerTrait, udp::UdpTracker},
   };

   use super::*;

   #[tokio::test]
   #[traced_test]
   async fn test_utp_peer_handshake() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();

      let metainfo = MagnetUri::parse(contents).await.unwrap();

      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let info_hash = magnet.info_hash().unwrap();
            let announce_list = magnet.announce_list.unwrap();
            let announce_url = announce_list[0].uri();

            let mut tracker = UdpTracker::new(announce_url, None, info_hash)
               .await
               .unwrap();
            let peer_id = tracker.peer_id;
            let peers = tracker.stream_peers().await.unwrap();

            // Skip if no peers found
            if peers.is_empty() {
               error!("No peers found, skipping test");
               return;
            }

            // Create a vector to hold all the join handles
            let mut handles = Vec::new();

            // For each peer, spawn a task to connect
            for mut peer in peers {
               let info_hash_clone = Arc::new(info_hash);

               let handle = tokio::spawn(async move {
                  // Create a timeout for the connection attempt
                  let result = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                     let mut utp_transport =
                        UtpTransport::new(peer_id.into(), info_hash_clone).await;

                     utp_transport.connect(&mut peer).await
                  })
                  .await;

                  // If timeout occurred or connection failed, return Err
                  match result {
                     Ok(Ok(peer_id)) => Ok(peer_id),
                     Ok(Err(e)) => Err(format!("Connection error: {}", e)),
                     Err(_) => Err("Connection timed out after 1 second".to_string()),
                  }
               });

               handles.push(handle);
            }

            // Wait for all connections to complete
            let mut results = Vec::new();
            for handle in handles {
               match handle.await {
                  Ok(result) => results.push(result),
                  Err(e) => results.push(Err(format!("Task panicked: {}", e))),
               }
            }

            // Calculate success rate
            let total_peers = results.len();
            let successful_peers = results.iter().filter(|r| r.is_ok()).count();
            let success_rate = (successful_peers as f64) / (total_peers as f64);

            info!(
               "Connected to {}/{} peers ({}%)",
               successful_peers,
               total_peers,
               (success_rate * 100.0) as u32
            );

            // Test passes if more than 10% of connections succeeded
            if success_rate > 0.1 {
               // Test passed
               assert!(true);
            } else {
               // Print the errors for debugging
               for (i, result) in results.iter().enumerate() {
                  if let Err(e) = result {
                     error!("Peer {} error: {}", i, e);
                  }
               }

               panic!(
                  "Less than 10% of peer connections succeeded ({}/{})",
                  successful_peers, total_peers
               );
            }
         }
         _ => panic!("Expected Torrent"),
      }
   }
}
