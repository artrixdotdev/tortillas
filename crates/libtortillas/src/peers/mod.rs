use crate::{
   errors::PeerTransportError,
   hashes::{Hash, InfoHash},
   peers::stream::PeerWriter,
};
use anyhow::Result;
use async_trait::async_trait;
use bitvec::vec::BitVec;
use commands::{PeerCommand, PeerResponse};
use librqbit_utp::UtpSocketUdp;
use messages::{Handshake, PeerMessages, MAGIC_STRING};
use std::{
   fmt::Display,
   net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
   sync::Arc,
   time::Duration,
};
use stream::{PeerRecv, PeerSend, PeerStream};
use tokio::{
   sync::{
      mpsc::{self, Receiver, Sender},
      Mutex,
   },
   time::{timeout, Instant},
};
use tracing::{error, trace};

pub mod commands;
pub mod messages;
pub mod stream;

/// It should be noted that the *name* PeerKey is slightly deprecated from previous renditions of
/// libtortillas. The idea of having a type for the "key" of a peer is still completely relevant
/// though.
pub type PeerKey = SocketAddr;
pub type PeerId = Arc<Hash<20>>;

/// Represents a BitTorrent peer with connection state and statistics
/// Download rate and upload rate are measured in kilobytes per second.
///
/// `am_choked` and `am_interested` refers to our status of choking and interest, and `choked` and
/// `interested` refers to the peers status of choking and interest.
#[derive(Eq, PartialEq, Hash, Debug, Clone)]
pub struct Peer {
   pub ip: IpAddr,
   pub port: u16,
   pub choked: bool,
   pub interested: bool,
   pub am_choking: bool,
   pub am_interested: bool,
   pub download_rate: u64,
   pub upload_rate: u64,
   pub pieces: BitVec<u8>,
   pub last_optimistic_unchoke: Option<Instant>,
   pub id: Option<Hash<20>>,
   pub last_message_sent: Option<Instant>,
   pub last_message_received: Option<Instant>,
   pub bytes_downloaded: u64,
   pub bytes_uploaded: u64,
}

impl Peer {
   /// Create a new peer with the given IP address and port
   pub fn new(ip: IpAddr, port: u16) -> Self {
      Peer {
         ip,
         port,
         choked: true,
         interested: false,
         am_choking: true,
         am_interested: false,
         download_rate: 0,
         upload_rate: 0,
         pieces: BitVec::EMPTY,
         last_optimistic_unchoke: None,
         id: None,
         last_message_received: None,
         last_message_sent: None,
         bytes_downloaded: 0,
         bytes_uploaded: 0,
      }
   }

   /// Create a new peer from an IPv4 address and port
   pub fn from_ipv4(ip: Ipv4Addr, port: u16) -> Self {
      Self::new(IpAddr::V4(ip), port)
   }

   /// Create a new peer from an IPv6 address and port
   pub fn from_ipv6(ip: Ipv6Addr, port: u16) -> Self {
      Self::new(IpAddr::V6(ip), port)
   }

   /// Get the socket address of the peer
   pub fn socket_addr(&self) -> SocketAddr {
      SocketAddr::new(self.ip, self.port)
   }

   /// Create a new peer from a socket address
   pub fn from_socket_addr(peer_addr: SocketAddr) -> Self {
      Self::new(peer_addr.ip(), peer_addr.port())
   }

   // Extract piece request handling into a separate method for clarity
   async fn handle_piece_request(stream: &mut PeerWriter, piece_num: u32) {
      // https://github.com/vimpunk/cratetorrent/blob/master/PEER_MESSAGES.md#6-request
      //
      // > All current implementations use 2^14 (16 kiB)
      // - BEP 0003
      //
      // For now, we are assuming that the offset is 0. This may need to be changed in the future.
      let request = PeerMessages::Request(piece_num, 0, 16384);
      const REQUEST_TIMEOUT: u64 = 5;

      let request_result =
         timeout(Duration::from_secs(REQUEST_TIMEOUT), stream.send(request)).await;

      match request_result {
         Ok(Ok(())) => {
            trace!("Successfully made request");
         }
         Ok(Err(_)) => {
            let error_msg = match request_result {
               Ok(Err(err)) => format!("Send error: {}", err),
               Err(_) => "Request timeout".to_string(),
               _ => unreachable!(),
            };

            error!("Failed to send request: {}", error_msg);

            // Note: We don't wait for a piece response here since the request failed
            // The piece response will be handled in the main select! loop if it arrives
         }
         Err(_) => {
            error!("Failed to send request to peer: {}", "Request timeout");

            // Note: We don't wait for a piece response here since the request failed
            // The piece response will be handled in the main select! loop if it arrives
         }
      }
   }

   /// Autonomously handles the connection & messages between a peer. The from_tx/from_rx is provided to
   /// facilitate communication to this function from the caller (likely TorrentEngine -- if so, this channel will be used to communicate what pieces TorrentEngine still needs).
   /// to_tx is provided to allow communication from handle_peer to the caller.
   ///
   /// At the moment, handle_peer is a leecher. In that, it is not a seeder -- it only takes from
   /// the torrent swarm. Seeding will be implemented in the future.
   pub async fn handle_peer(
      mut self,
      to_tx: mpsc::Sender<PeerResponse>,
      info_hash: InfoHash,
      our_id: PeerId,
      stream: Option<PeerStream>,
      utp_socket: Option<Arc<UtpSocketUdp>>,
   ) {
      let peer_addr = self.socket_addr();
      let (from_tx, mut from_rx) = mpsc::channel(100);

      to_tx.send(PeerResponse::Init(from_tx)).await.unwrap();

      // For outgoing peers (we are connecting to them), we should create the stream ourselves and send the handshake & bitfield
      trace!("Attempting to connect to peer {}", peer_addr);
      let mut stream = if let None = stream {
         let mut stream = PeerStream::connect(peer_addr, utp_socket).await;
         // Send handshake to peer
         let peer_id = stream
            .send_handshake(our_id, Arc::new(info_hash))
            .await
            .unwrap();
         self.id = Some(*peer_id);
         stream
      } else {
         // Otherwise, since they have already sent their handshake and been verified, we can skip that part.
         stream.unwrap()
      };

      trace!("Connected to peer {}", peer_addr);

      // Send empty bitfield. This may need to be refactored in the future to account for seeding.
      stream
         .send(PeerMessages::Bitfield(BitVec::EMPTY))
         .await
         .unwrap();

      trace!("Sent empty bitfield to peer {}", peer_addr);

      // Wait for bitfield in return
      let message = stream.recv().await.unwrap();
      let to_send = message.clone();

      match message {
         PeerMessages::Bitfield(bitfield) => {
            trace!("Received bitfield message from peer {}", peer_addr);
            self.pieces = bitfield;

            to_tx
               .send(PeerResponse::Receive {
                  message: to_send,
                  peer_key: peer_addr,
               })
               .await
               .map_err(|e| {
                  error!(
                     "Error sending bitfield with to_tx on peer {}: {}",
                     peer_addr, e
                  )
               })
               .unwrap();
         }
         res => {
            error!(
               "Message received from peer {} was not a bitfield -- it was a {:?}",
               peer_addr, res
            )
         }
      }

      // Send an "interested" message
      stream.send(PeerMessages::Interested).await.unwrap();
      self.am_interested = true;

      trace!("Sent an Interested message to peer {}", peer_addr);

      if let PeerMessages::Unchoke = stream.recv().await.unwrap() {
         trace!("Peer {} unchoked us", peer_addr);
         self.choked = false;
      }

      // Start of request/piece message loop
      let (mut reader, writer) = stream.split();

      let writer = Arc::new(Mutex::new(writer));

      // 1st of 2 tokio::spawn(s) that allow the peer to communicate (read + write) concurrently. See
      // above `stream.split()`.
      let reader_to_tx = to_tx.clone();
      tokio::spawn(async move {
         while let Ok(message) = reader.recv().await {
            // Handle different message types
            match &message {
               PeerMessages::Piece(_, _, _) => {
                  trace!("Received a Piece message from peer {}", peer_addr);
                  reader_to_tx
                     .send(PeerResponse::Piece(message))
                     .await
                     .map_err(|e| {
                        error!(
                           "Failed to send Piece message from peer {}: {}",
                           peer_addr, e
                        )
                     })
                     .unwrap();
               }
               _ => {
                  // Handle other message types or forward them
                  trace!(
                     "Received a message other than supported options from peer {}: {:?}",
                     peer_addr,
                     message
                  );
                  reader_to_tx
                     .send(PeerResponse::Receive {
                        message,
                        peer_key: peer_addr,
                     })
                     .await
                     .map_err(|e| error!("Failed to send message from peer {}: {}", peer_addr, e))
                     .unwrap();
               }
            }
         }
      });

      let writer_clone = Arc::clone(&writer);
      let writer_to_tx = to_tx.clone();

      // 2nd tokio::spawn (see comment above)
      tokio::spawn(async move {
         while let Some(message) = from_rx.recv().await {
            trace!(
               "Received message from from_rx for peer {}: {:?}",
               peer_addr,
               message
            );

            // If we're choking, we can't do anything.
            if !self.am_choking {
               match message {
                  PeerCommand::Piece(piece_num) => {
                     let mut writer_guard = writer_clone.lock().await;
                     Self::handle_piece_request(&mut writer_guard, piece_num).await;
                  }
               }
            } else {
               writer_to_tx
                  .send(PeerResponse::Choking)
                  .await
                  .map_err(|e| {
                     error!(
                        "Error when sending message back with to_tx from peer {}: {}",
                        peer_addr, e
                     )
                  })
                  .unwrap();
            }
         }
      });
   }
}

impl Display for Peer {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(f, "{}:{}", self.ip, self.port)
   }
}

#[cfg(test)]
mod tests {
   use std::{
      io::{Read, Write},
      str::FromStr,
   };

   use rand::RngCore;
   use tokio::io::AsyncReadExt;
   use tokio::io::AsyncWriteExt;
   use tokio::net::TcpListener;
   use tracing_test::traced_test;

   use crate::parser::MagnetUri;

   use super::{stream::validate_handshake, *};

   #[tokio::test]
   #[traced_test]
   async fn test_peer_creation() {
      let peer = Peer::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 6881);
      assert_eq!(peer.ip, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
      assert_eq!(peer.port, 6881);
   }

   #[tokio::test]
   #[traced_test]
   async fn test_peer_connection() {
      // This is a known good peer (as of 06/17/2025) for the torrent located in zenshuu.txt
      let known_good_peer = "78.192.97.58:51413";
      let mut peer = Peer::from_socket_addr(SocketAddr::from_str(known_good_peer).unwrap());
      let (to_tx, mut rx) = mpsc::channel(100);

      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/zenshuu.txt");
      let magnet_uri = tokio::fs::read_to_string(path).await.unwrap();
      let data = MagnetUri::parse(magnet_uri).unwrap();

      // Stuff for generating our_id (yes, literally our ID as a peer in the network)
      let mut our_id = [0u8; 20];
      rand::rng().fill_bytes(&mut our_id);
      let our_id = Hash::from_bytes(our_id);

      peer
         .handle_peer(
            to_tx,
            data.info_hash().unwrap(),
            Arc::new(our_id),
            None,
            None,
         )
         .await;

      let from_tx = rx.recv().await.unwrap();
   }

   #[tokio::test]
   #[traced_test]
   /// Keep in mind that this test operates as both the TorrentEngine and the peer, handling all
   /// communication with the peer in such a manner.
   async fn test_peer_to_peer_pieces() {
      let info_hash = Hash::new(rand::random::<[u8; 20]>());

      // Start listener
      let peer_addr = "127.0.0.1:9883";
      let listener = TcpListener::bind(peer_addr).await.unwrap();

      // Start peer
      let peer = Peer::from_socket_addr(SocketAddr::from_str(peer_addr).unwrap());
      let peer_id = Hash::new(rand::random::<[u8; 20]>());
      let (to_tx, mut to_rx) = mpsc::channel(100);

      tokio::spawn(async move {
         peer
            .handle_peer(to_tx, info_hash, Arc::new(peer_id), None, None)
            .await;
      });

      let from_tx_wrapped = to_rx.recv().await.unwrap();
      let from_tx = if let PeerResponse::Init(from_tx) = from_tx_wrapped {
         from_tx
      } else {
         panic!("Was not PeerResponse::Init");
      };

      trace!("Got from_tx from peer");

      // First message should be a handshake
      let stream = listener.accept().await.unwrap().0;
      let mut peer_stream = PeerStream::Tcp(stream);
      let mut bytes = [0; 68];
      peer_stream.read_exact(&mut bytes).await.unwrap();

      // Ensure the handshake we received is valid
      assert!(validate_handshake(
         &Handshake::from_bytes(&bytes).unwrap(),
         SocketAddr::from_str(peer_addr).unwrap(),
         Arc::new(info_hash),
      )
      .is_ok());

      trace!("Received valid handshake");

      // Send a handshake back
      let our_id = Hash::new(rand::random::<[u8; 20]>());
      // peer_stream.send_handshake() does not work?
      peer_stream
         .write_all(&Handshake::new(Arc::new(info_hash), Arc::new(our_id)).to_bytes())
         .await
         .unwrap();

      trace!("Sent handshake back to peer");

      // Wait for a bitfield
      let bitfield = peer_stream.recv().await.unwrap();
      assert!(matches!(bitfield, PeerMessages::Bitfield { .. }));
      trace!("Received bitfield from peer");

      // Send empty bitfield in return
      peer_stream
         .send(PeerMessages::Bitfield(BitVec::EMPTY))
         .await
         .unwrap();

      // Wait for a bitfield from to_tx
      let peer_response_bitfield = to_rx.recv().await.unwrap();
      assert!(matches!(
         peer_response_bitfield,
         PeerResponse::Receive { .. }
      ));

      trace!("Sent empty bitfield");

      // Tell the peer to request piece 1
      from_tx.send(PeerCommand::Piece(1)).await.unwrap();

      // Wait for a request for piece 1
      let request = peer_stream.recv().await.unwrap();

      assert!(matches!(request, PeerMessages::Request { .. }));

      trace!("Got request message from peer");

      // Send a fake piece message back
      peer_stream
         .send(PeerMessages::Piece(1, 0, vec![0, 1, 0, 1]))
         .await
         .unwrap();

      trace!("Sent fake piece message to peer");

      // Wait for a piece from to_tx
      let peer_response_piece = to_rx.recv().await.unwrap();
      assert!(matches!(peer_response_piece, PeerResponse::Piece { .. }))
   }
}
