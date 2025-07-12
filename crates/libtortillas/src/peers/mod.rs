use crate::{
   hashes::{Hash, InfoHash},
   peers::stream::PeerWriter,
};
use atomic_time::AtomicOptionInstant;
use bitvec::vec::BitVec;
use commands::{PeerCommand, PeerResponse};
use core::fmt;
use librqbit_utp::UtpSocketUdp;
use messages::{PeerMessages, MAGIC_STRING};
use std::{
   fmt::{Debug, Display},
   hash::{Hash as InternalHash, Hasher},
   net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
   sync::{
      atomic::{AtomicBool, AtomicU64, Ordering},
      Arc,
   },
   time::Duration,
   usize,
};
use stream::{PeerRecv, PeerSend, PeerStream};
use tokio::{
   sync::{
      broadcast,
      mpsc::{self, Sender},
      Mutex,
   },
   time::{timeout, Instant},
};
use tracing::{error, instrument, trace};

pub mod commands;
pub mod messages;
pub mod stream;

/// It should be noted that the *name* PeerKey is slightly deprecated from previous renditions of
/// libtortillas. The idea of having a type for the "key" of a peer is still completely relevant
/// though.
pub type PeerKey = SocketAddr;
pub type PeerId = Arc<Hash<20>>;

/// A helper struct for Peer that maintains a given peers state. This state includes both the state
/// defined in [BEP 0003](https://www.bittorrent.org/beps/bep_0003.html) and our own state which we
/// wish to maintain (ex. total bytes downloaded).
///
/// The general intent of this struct is to make it easier for us to "throw" state across threads
/// -- every field in here is an atomic Arc, which means that it's very easy to do something
/// like this (in an impl of the Peer struct):
///
/// ```no_run
/// tokio::spawn(async move {
///   some_fn(self.state.clone());
/// })
/// ```
///
/// `clone()` operations on this struct should be relatively lightweight, seeing that everything is
/// contained in an Arc.
#[derive(Clone)]
pub struct PeerState {
   /// Download rate measured in kilobytes per second
   pub download_rate: Arc<AtomicU64>,
   /// Upload rate measured in kilobytes per second
   pub upload_rate: Arc<AtomicU64>,
   /// The remote peer's choke status
   pub choked: Arc<AtomicBool>,
   /// The remote peer's interest status
   pub interested: Arc<AtomicBool>,
   /// Our choke status
   pub am_choked: Arc<AtomicBool>,
   /// Our interest status
   pub am_interested: Arc<AtomicBool>,
   /// The timestamp of the last time that a peer unchoked us
   pub last_optimistic_unchoke: Arc<AtomicOptionInstant>,
   /// Defaults to None. Does not update on initial handshake, initial sending of bitfield, or initial sending of Interested message.
   pub last_message_sent: Arc<AtomicOptionInstant>,
   /// Defaults to None. Does not update on initial handshake, initial sending of bitfield, or initial sending of Interested message.
   pub last_message_received: Arc<AtomicOptionInstant>,
   /// Total bytes downloaded
   pub bytes_downloaded: Arc<AtomicU64>,
   /// Total bytes uploaded
   pub bytes_uploaded: Arc<AtomicU64>,
}

impl PeerState {
   fn new() -> Self {
      PeerState {
         choked: Arc::new(true.into()),
         interested: Arc::new(false.into()),
         am_choked: Arc::new(true.into()),
         am_interested: Arc::new(false.into()),
         download_rate: Arc::new(0u64.into()),
         upload_rate: Arc::new(0u64.into()),
         last_optimistic_unchoke: Arc::new(AtomicOptionInstant::none()),
         last_message_received: Arc::new(AtomicOptionInstant::none()),
         last_message_sent: Arc::new(AtomicOptionInstant::none()),
         bytes_downloaded: Arc::new(0u64.into()),
         bytes_uploaded: Arc::new(0u64.into()),
      }
   }
}

/// Represents a BitTorrent peer with connection state and statistics
/// Download rate and upload rate are measured in kilobytes per second.
#[derive(Clone)]
pub struct Peer {
   pub ip: IpAddr,
   pub port: u16,
   pub state: PeerState,
   pub pieces: BitVec<u8>,
   pub id: Option<Hash<20>>,
}

impl Debug for Peer {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.debug_struct("Peer")
         .field("addr", &self.socket_addr())
         .field("choked", &self.state.choked.load(Ordering::Relaxed))
         .field("interested", &self.state.interested.load(Ordering::Relaxed))
         .field("am_choked", &self.state.am_choked.load(Ordering::Relaxed))
         .field(
            "am_interested",
            &self.state.am_interested.load(Ordering::Relaxed),
         )
         .field("id", &self.id)
         .finish()
   }
}

impl InternalHash for Peer {
   fn hash<H: Hasher>(&self, state: &mut H) {
      self.socket_addr().hash(state)
   }
}

impl PartialEq for Peer {
   fn eq(&self, other: &Self) -> bool {
      self.socket_addr() == other.socket_addr()
   }
}

impl Display for Peer {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(f, "{}:{}", self.ip, self.port)
   }
}

impl Eq for Peer {}

impl Peer {
   /// Create a new peer with the given IP address and port
   pub fn new(ip: IpAddr, port: u16) -> Self {
      Peer {
         ip,
         port,
         state: PeerState::new(),
         pieces: BitVec::EMPTY,
         id: None,
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

   /// Extract piece request handling into a separate method for clarity
   async fn handle_piece_request(&mut self, stream: &mut PeerWriter, piece_num: u32) {
      // If the peer does not have the piece, don't request it.
      if self.pieces.get(piece_num as usize).unwrap() == false {
         return;
      }

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

   // Small helper function for sending messages witih to_engine_tx.
   fn send(
      to_engine_tx: broadcast::Sender<PeerResponse>,
      message: PeerResponse,
      peer_addr: SocketAddr,
   ) {
      to_engine_tx
         .send(message)
         .map_err(|e| error!(%peer_addr, "Failed to send Piece message from peer: {}", e))
         .unwrap();
   }

   /// A helper function for [handle_peer](Peer::handle_peer). This is a very beefy function --
   /// refactors that reduce its size are welcome.
   async fn handle_recv(
      &mut self,
      message: PeerMessages,
      to_engine_tx: broadcast::Sender<PeerResponse>,
      from_engine_tx: mpsc::Sender<PeerCommand>,
   ) {
      let peer_addr = self.socket_addr();
      match &message {
         PeerMessages::Piece(_, _, _) => {
            trace!("Received a Piece message from peer {}", peer_addr);
            Self::send(
               to_engine_tx,
               PeerResponse::Receive {
                  message,
                  peer_key: peer_addr,
               },
               peer_addr,
            );
         }
         PeerMessages::Choke => {
            self.state.am_choked.store(true, Ordering::Release);
            trace!("Peer {} is now choked", peer_addr);
         }
         PeerMessages::Unchoke => {
            Self::update_message(self.state.last_optimistic_unchoke.clone());
            self.state.am_choked.store(false, Ordering::Release);
            trace!("Peer {} is now unchoked", peer_addr);
            Self::send(
               to_engine_tx,
               PeerResponse::Unchoke {
                  from_engine_tx,
                  peer_key: peer_addr,
               },
               peer_addr,
            );
         }
         PeerMessages::Interested => {
            self.state.am_interested.store(true, Ordering::Release);
            trace!("Peer {} is now interested", peer_addr);
         }
         PeerMessages::NotInterested => {
            self.state.am_interested.store(false, Ordering::Release);
            trace!("Peer {} is now not interested", peer_addr);
         }
         PeerMessages::KeepAlive => {
            trace!(%peer_addr, "Received a keep alive message from peer");
         }
         PeerMessages::Have(piece) => {
            trace!(%peer_addr, "Peer has piece {}", piece);
         }
         PeerMessages::Request(index, _, _) => {
            trace!(%peer_addr, "Peer wants piece {}", index);
            Self::send(
               to_engine_tx,
               PeerResponse::Receive {
                  message,
                  peer_key: peer_addr,
               },
               peer_addr,
            );
         }
         PeerMessages::Extended(extended_id, extended_handshake_message) => {
            trace!(%peer_addr, "Peer sent an Extended message with id {}", extended_id);
            if let Some(inner) = extended_handshake_message {
               trace!(%peer_addr, "Peer sent a payload with Extended message: {:?}", inner);
            }
         }
         PeerMessages::Cancel(index, _, _) => {
            trace!(%peer_addr, "Peer wants to cancel piece {}", index);
            // TODO
         }
         PeerMessages::Bitfield(_) => {
            trace!(%peer_addr, "Received new bitfield from peer");
         }
         PeerMessages::Handshake(handshake) => {
            trace!(%peer_addr, "Peer sent a handshake for some reason: {:?}", handshake);
         }
      }
   }

   /// A helper function for [handle_peer](Peer::handle_peer). This is a very beefy function --
   /// refactors that reduce its size are welcome.
   async fn handle_peer_command(
      &mut self,
      message: PeerCommand,
      writer: Arc<Mutex<PeerWriter>>,
      to_engine_tx: broadcast::Sender<PeerResponse>,
   ) {
      let peer_addr = self.socket_addr();
      trace!(
         "Received message from from_engine_rx for peer {}: {:?}",
         peer_addr,
         message
      );

      match message {
         PeerCommand::Piece(piece_num) => {
            // If we're choking or the peer isn't interested, we can't do anything.
            if !self.state.am_choked.load(Ordering::Acquire)
               && self.state.interested.load(Ordering::Acquire)
            {
               let mut writer_guard = writer.lock().await;

               self
                  .handle_piece_request(&mut writer_guard, piece_num)
                  .await;
            } else {
               trace!(
                  "Couldn't accept PeerCommand::Piece because peer is choking and/or not interested"
               );
               to_engine_tx
                  .send(PeerResponse::Choking(peer_addr))
                  .map_err(|e| {
                     error!(
                        "Error when sending message back with to_engine_tx from peer {}: {}",
                        peer_addr, e
                     )
                  })
                  .unwrap();
            }
         }
      }
   }

   /// A small helper function used to update the time on a message to `Instant::now()`.
   ///
   /// # Examples
   ///
   /// ```no_run
   /// // In an impl of Peer
   ///
   /// Self::update_message(self.last_message_sent.clone());
   /// ```
   fn update_message(message: Arc<AtomicOptionInstant>) {
      message.store(Some(Instant::now().into()), Ordering::Release);
   }

   /// Autonomously handles the connection & messages between a peer. The from_engine_tx/from_engine_rx is provided to
   /// facilitate communication to this function from the caller (likely TorrentEngine -- if so, this channel will be used to communicate what pieces TorrentEngine still needs).
   /// to_engine_tx is provided to allow communication from handle_peer to the caller.
   ///
   /// At the moment, handle_peer is a leecher. In that, it is not a seeder -- it only takes from
   /// the torrent swarm. Seeding will be implemented in the future.
   pub async fn handle_peer(
      mut self,
      to_engine_tx: broadcast::Sender<PeerResponse>,
      info_hash: InfoHash,
      our_id: PeerId,
      stream: Option<PeerStream>,
      utp_socket: Option<Arc<UtpSocketUdp>>,
      init_bitfield: Option<BitVec<u8>>,
   ) {
      let peer_addr = self.socket_addr();
      let (from_engine_tx, mut from_engine_rx) = mpsc::channel(100);

      to_engine_tx
         .send(PeerResponse::Init {
            from_engine_tx: from_engine_tx.clone(),
            peer_key: peer_addr,
         })
         .unwrap();

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

      let bitfield_to_send = init_bitfield.unwrap_or(BitVec::EMPTY);

      trace!(%peer_addr, %bitfield_to_send, "Bitfield to send to peer");

      // Send empty bitfield. This may need to be refactored in the future to account for seeding.
      stream
         .send(PeerMessages::Bitfield(bitfield_to_send.clone()))
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

            to_engine_tx
               .send(PeerResponse::Receive {
                  message: to_send,
                  peer_key: peer_addr,
               })
               .map_err(|e| {
                  error!(
                     "Error sending bitfield with to_engine_tx on peer {}: {}",
                     peer_addr, e
                  )
               })
               .unwrap();

            trace!("Successfully sent bitfield message back to function that spawned this thread (likely TorrentEngine)");
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
      self.state.interested.store(true, Ordering::Release);
      trace!("Sent an Interested message to peer {}", peer_addr);

      // Start of request/piece message loop
      let (mut reader, writer) = stream.split();
      let writer = Arc::new(Mutex::new(writer));

      // Clone state to use (will be moved)
      let state = self.state.clone();

      // Continuously loop and handle messages from reader and from_engine_rx. The only downside with this
      // setup is that messages can only be handled one at a time. But realistically, there are few
      // chokepoints that we would reach with this setup.
      loop {
         tokio::select! {
            message = reader.recv() => {
               match message {
                  Ok(inner) => {
                     Self::update_message(state.last_message_received.clone());
                     self.handle_recv(
                        inner,
                        to_engine_tx.clone(),
                        from_engine_tx.clone(),
                     )
                     .await;
                  }
                  Err(e) => {
                     error!(%peer_addr, "An error occured when receiving the message from the reader: {e}");

                     // Is this the best practice? Eh. But realistically, if a peer sends us
                     // something that is simply invalid, we probably don't want to work with them
                     // anymore.
                     panic!("Error occured when receiving a message from the peer");
                  }
               }
            }
            message = from_engine_rx.recv() => {
               match message {
                  Some(inner) => {
                     // Are all these clones horribly inefficient? Hopefully not.
                     self.handle_peer_command(
                        inner,
                        writer.clone(),
                        to_engine_tx.clone(),
                     )
                     .await;
                     Self::update_message(self.state.last_message_sent.clone());
                  }
                  None => {
                     error!(%peer_addr, "Received a None message from from_engine_rx");
                  }
               }
            }
         }
      }
   }
}

#[cfg(test)]
mod tests {
   use std::str::FromStr;

   use bitvec::bitvec;
   use bitvec::order::Lsb0;
   use rand::RngCore;
   use tokio::io::AsyncReadExt;
   use tokio::io::AsyncWriteExt;
   use tokio::net::TcpListener;
   use tracing_test::traced_test;

   use crate::{parser::MagnetUri, peers::messages::Handshake};

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
   /// As this test contacts a remote peer, it may not always work. However, it is still included
   /// in the general tests for sake of completeness.
   async fn test_peer_connection() {
      // This is a known good peer (as of 06/17/2025) for the torrent located in zenshuu.txt
      let known_good_peer = "78.192.97.58:51413";
      let peer = Peer::from_socket_addr(SocketAddr::from_str(known_good_peer).unwrap());
      let (to_engine_tx, mut to_engine_rx) = broadcast::channel(100);

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
            to_engine_tx,
            data.info_hash().unwrap(),
            Arc::new(our_id),
            None,
            None,
            None,
         )
         .await;

      let _ = to_engine_rx.recv().await.unwrap();

      let peer_response_bitfield = to_engine_rx.recv().await.unwrap();
      assert!(matches!(
         peer_response_bitfield,
         PeerResponse::Receive {
            message: PeerMessages::Bitfield(..),
            ..
         }
      ))
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
      let (to_engine_tx, mut to_engine_rx) = broadcast::channel(100);

      tokio::spawn(async move {
         peer
            .handle_peer(to_engine_tx, info_hash, Arc::new(peer_id), None, None, None)
            .await;
      });

      let from_engine_tx_wrapped = to_engine_rx.recv().await.unwrap();
      let from_engine_tx = if let PeerResponse::Init { from_engine_tx, .. } = from_engine_tx_wrapped
      {
         from_engine_tx
      } else {
         panic!("Was not PeerResponse::Init");
      };

      trace!("Got from_engine_tx from peer");

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

      // Send bitfield in return
      peer_stream
         .send(PeerMessages::Bitfield(bitvec![u8, Lsb0; 0, 1, 0]))
         .await
         .unwrap();

      // Wait for a bitfield from to_engine_tx
      let peer_response_bitfield = to_engine_rx.recv().await.unwrap();
      assert!(matches!(
         peer_response_bitfield,
         PeerResponse::Receive { .. }
      ));

      trace!("Sent bitfield");

      // Receive an Interested message
      let peer_response_unchoke = peer_stream.recv().await.unwrap();
      assert!(matches!(peer_response_unchoke, PeerMessages::Interested));

      trace!("Recieved an Interested message");

      // Send an Unchoke message
      peer_stream.send(PeerMessages::Unchoke).await.unwrap();

      trace!("Sent an unchoke message");

      let unchoke_message = to_engine_rx.recv().await.unwrap();
      assert!(matches!(unchoke_message, PeerResponse::Unchoke { .. }));

      trace!("Got unchoke message from to_engine_rx");

      // Tell the peer to request piece 1
      from_engine_tx.send(PeerCommand::Piece(1)).await.unwrap();

      trace!("Sent piece message");

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

      // Wait for a piece from to_engine_tx
      let peer_response_piece = to_engine_rx.recv().await.unwrap();
      assert!(matches!(
         peer_response_piece,
         PeerResponse::Receive {
            message: PeerMessages::Piece(..),
            ..
         }
      ))
   }
}
