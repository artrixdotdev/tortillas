use core::fmt;
use std::{
   collections::HashMap,
   fmt::{Debug, Display},
   hash::{Hash as InternalHash, Hasher},
   net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
   sync::{Arc, atomic::Ordering},
   time::Duration,
};

use atomic_time::AtomicOptionInstant;
use bitvec::vec::BitVec;
use commands::{PeerCommand, PeerResponse};
use librqbit_utp::UtpSocketUdp;
use messages::{ExtendedMessage, ExtendedMessageType, MAGIC_STRING, PeerMessages};
use stream::{PeerRecv, PeerSend, PeerStream};
use tokio::{
   sync::{
      Mutex, broadcast,
      mpsc::{self, Receiver, Sender},
   },
   time::{Instant, sleep, timeout},
};
use tracing::{debug, error, info, trace, warn};

use crate::{
   hashes::{Hash, InfoHash},
   peers::{
      peer::{info::PeerInfo, state::PeerState, supports::PeerSupports},
      stream::PeerWriter,
   },
};

pub mod commands;
pub mod messages;
pub mod peer;
pub mod stream;

/// It should be noted that the *name* PeerKey is slightly deprecated from
/// previous renditions of libtortillas. The idea of having a type for the "key"
/// of a peer is still completely relevant though.
pub type PeerKey = SocketAddr;
pub type PeerId = Arc<Hash<20>>;

/// Represents a BitTorrent peer with connection state and statistics
/// Download rate and upload rate are measured in kilobytes per second.
#[derive(Clone)]
pub struct Peer {
   pub ip: IpAddr,
   pub port: u16,
   pub state: PeerState,
   pub pieces: BitVec<u8>,
   /// The reserved bytes that the peer sent us in their handshake. This
   /// indicates what extensions the peer supports.
   pub reserved: [u8; 8],
   pub peer_supports: PeerSupports,
   pub id: Option<Hash<20>>,
   pub info: PeerInfo,
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
         reserved: [0u8; 8],
         peer_supports: PeerSupports::new(),
         id: None,
         info: PeerInfo::new(0, vec![]),
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
      let peer_addr = self.socket_addr();

      // If the peer does not have the piece, don't request it.
      if self.pieces.get(piece_num as usize).unwrap() == false {
         trace!(%peer_addr, piece_num, "Peer does not have requested piece");
         return;
      }

      // https://github.com/vimpunk/cratetorrent/blob/master/PEER_MESSAGES.md#6-request
      //
      // > All current implementations use 2^14 (16 kiB)
      // - BEP 0003
      //
      // For now, we are assuming that the offset is 0. This may need to be changed in
      // the future.
      let request = PeerMessages::Request(piece_num, 0, 16384);
      const REQUEST_TIMEOUT: u64 = 5;

      let request_result =
         timeout(Duration::from_secs(REQUEST_TIMEOUT), stream.send(request)).await;

      match request_result {
         Ok(Ok(())) => {
            trace!(%peer_addr, piece_num, "Piece request sent successfully");
         }
         Ok(Err(send_err)) => {
            error!(%peer_addr, piece_num, error = %send_err, "Failed to send piece request");
         }
         Err(_) => {
            warn!(%peer_addr, piece_num, timeout_secs = REQUEST_TIMEOUT, "Piece request timed out");
         }
      }
   }

   // Small helper function for sending messages witih to_engine_tx.
   fn send(
      to_engine_tx: broadcast::Sender<PeerResponse>, message: PeerResponse, peer_addr: SocketAddr,
   ) {
      if let Err(e) = to_engine_tx.send(message) {
         error!(%peer_addr, error = %e, "Failed to send message to engine");
      }
   }

   /// A helper function for [handle_peer](Peer::handle_peer). This is a very
   /// beefy function -- refactors that reduce its size are welcome.
   async fn handle_recv(
      &mut self, message: PeerMessages, to_engine_tx: broadcast::Sender<PeerResponse>,
      from_engine_tx: mpsc::Sender<PeerCommand>, inner_send_tx: mpsc::Sender<PeerCommand>,
      info_hash: InfoHash,
   ) {
      let peer_addr = self.socket_addr();
      match &message {
         PeerMessages::Piece(index, offset, data) => {
            trace!(%peer_addr, piece_index = index, offset, data_len = data.len(), "Received piece data");
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
            debug!(%peer_addr, "Peer choked us");
         }
         PeerMessages::Unchoke => {
            Self::update_message(self.state.last_optimistic_unchoke.clone());
            self.state.am_choked.store(false, Ordering::Release);
            debug!(%peer_addr, "Peer unchoked us");
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
            debug!(%peer_addr, "Peer is interested in our pieces");
         }
         PeerMessages::NotInterested => {
            self.state.am_interested.store(false, Ordering::Release);
            debug!(%peer_addr, "Peer is not interested in our pieces");
         }
         PeerMessages::KeepAlive => {
            trace!(%peer_addr, "Received keep alive");
         }
         PeerMessages::Have(piece_index) => {
            trace!(%peer_addr, piece_index, "Peer has piece");
         }
         PeerMessages::Request(index, offset, length) => {
            trace!(%peer_addr, piece_index = index, offset, length, "Peer requested piece data");
            Self::send(
               to_engine_tx,
               PeerResponse::Receive {
                  message,
                  peer_key: peer_addr,
               },
               peer_addr,
            );
         }
         PeerMessages::Extended(extended_id, extended_message, metadata) => {
            trace!(%peer_addr, extended_id, "Received extended message");

            // If this is an Extended handshake, send a handshake in response.
            if *extended_id == 0 {
               let mut m = HashMap::new();
               m.insert("ut_metadata".into(), 2);
               let mut extended_message = ExtendedMessage::new();
               extended_message.supported_extensions = Some(m);
               let command = PeerCommand::Extended(0, Some(extended_message));

               trace!(%peer_addr, "Sending extended handshake response");

               if let Err(e) = inner_send_tx.send(command).await {
                  error!(%peer_addr, error = %e, "Failed to send extended handshake response");
               }
            }

            // Save to Peer.
            if let Some(inner_metadata) = metadata {
               if let Err(e) = self.info.append_to_bytes(inner_metadata.to_vec()) {
                  warn!(%peer_addr, error = %e, "Failed to append metadata bytes");
               }
            }

            if let Some(extended_message) = &**extended_message {
               if let Some(size) = extended_message.metadata_size {
                  debug!(%peer_addr, metadata_size = size, "Received metadata size from peer");
               }

               if let Ok(id) = extended_message.supports_bep_0009() {
                  self.peer_supports.bep_0009 = id;

                  // If peer has metadata and we don't already have it, request metadata from peer
                  if let Some(metadata_size) = extended_message.metadata_size {
                     self.info.info_size = metadata_size;
                     if self.info.info_size > 0 && !self.info.have_all_bytes() {
                        let piece_num = extended_message.piece.unwrap_or(0);
                        let mut extended_message_command = ExtendedMessage::new();
                        extended_message_command.piece = Some(piece_num);
                        extended_message_command.msg_type = Some(ExtendedMessageType::Request);

                        // The Extended ID as specified in BEP 0009 is the ID from the m dictionary
                        // -- in this case the ID listed under ut_metadata
                        let command = PeerCommand::Extended(
                           self.peer_supports.bep_0009.into(),
                           Some(extended_message_command),
                        );

                        if let Err(e) = inner_send_tx.send(command).await {
                           error!(%peer_addr, error = %e, "Failed to send metadata request");
                        } else {
                           trace!(%peer_addr, piece_num, "Requested metadata piece");
                        }
                     }
                  }
               }
            }

            // If we have everything, validate it and send it back to the engine.
            //
            // If the info dicts hash is not correct, there is currently no error handling
            // (only an error message is shown)
            if self.info.have_all_bytes() {
               debug!(%peer_addr, "Received complete metadata, validating");
               let info = self.info.generate_info_from_bytes(info_hash).await;
               if let Err(e) = info {
                  error!(%peer_addr, error = %e, "Metadata validation failed");
               } else {
                  // We have to convert back to bytes due to issues with deriving Clone on the
                  // Info struct.
                  if let Err(e) = to_engine_tx.send(PeerResponse::Info {
                     bytes: self.info.info_bytes.clone(),
                     peer_key: peer_addr,
                     from_engine_tx: from_engine_tx.clone(),
                  }) {
                     error!(%peer_addr, error = %e, "Failed to send validated metadata to engine");
                  } else {
                     info!(%peer_addr, "Successfully received and validated metadata from peer");
                  }
               }
            }
         }
         PeerMessages::Cancel(index, offset, length) => {
            trace!(%peer_addr, piece_index = index, offset, length, "Peer cancelled piece request");
            // TODO
         }
         PeerMessages::Bitfield(bitfield) => {
            let piece_count = bitfield.len();
            debug!(%peer_addr, piece_count, "Received bitfield from peer");
            self.pieces = bitfield.clone();

            Self::send(
               to_engine_tx,
               PeerResponse::Receive {
                  message,
                  peer_key: peer_addr,
               },
               peer_addr,
            );
         }
         PeerMessages::Handshake(handshake) => {
            warn!(%peer_addr, "Received unexpected handshake from peer");
         }
      }
   }

   /// A helper function for [handle_peer](Peer::handle_peer). This is a very
   /// beefy function -- refactors that reduce its size are welcome.
   async fn handle_peer_command(
      &mut self, message: PeerCommand, writer: Arc<Mutex<PeerWriter>>,
      to_engine_tx: broadcast::Sender<PeerResponse>, from_engine_tx: mpsc::Sender<PeerCommand>,
      inner_recv_tx: mpsc::Sender<PeerMessages>,
   ) {
      let peer_addr = self.socket_addr();
      trace!(%peer_addr, "Processing command from engine");

      if let PeerCommand::Piece(piece_num) = message {
         // If we're choking or the peer isn't interested, we can't do anything.
         if !self.state.am_choked.load(Ordering::Acquire)
            && self.state.interested.load(Ordering::Acquire)
         {
            let mut writer_guard = writer.lock().await;

            self
               .handle_piece_request(&mut writer_guard, piece_num)
               .await;
         } else {
            let am_choked = self.state.am_choked.load(Ordering::Acquire);
            let interested = self.state.interested.load(Ordering::Acquire);

            debug!(%peer_addr, piece_num, am_choked, interested, "Cannot request piece - peer state prevents it");

            Self::send(
               to_engine_tx,
               PeerResponse::Choking {
                  peer_key: peer_addr,
                  from_engine_tx,
               },
               peer_addr,
            );
         }
      }
   }

   async fn handle_send(
      &mut self, message: PeerCommand, writer: Arc<Mutex<PeerWriter>>,
      to_engine_tx: broadcast::Sender<PeerResponse>, from_engine_tx: mpsc::Sender<PeerCommand>,
      inner_recv_tx: mpsc::Sender<PeerMessages>,
   ) {
      let peer_addr = self.socket_addr();

      // Request metadata with an Extended message (if peer supports BEP 0010 and BEP
      // 0009)
      //
      // Note that when we receive the info-dictionary from a peer, we absolutely must
      // compare the hash of it to our info hash.
      if let PeerCommand::Extended(id, extended_message) = message
         && self.peer_supports.bep_0010
         && self.peer_supports.bep_0009 > 0
      {
         let message = PeerMessages::Extended(id as u8, Box::new(extended_message), None);

         {
            let mut writer_guard = writer.lock().await;
            if let Err(e) = writer_guard.send(message).await {
               error!(%peer_addr, error = %e, "Failed to send extended message");
               return;
            }
         }

         trace!(%peer_addr, extended_id = id, "Sent extended message to peer");
      }
   }

   /// A small helper function used to update the time on a message to
   /// `Instant::now()`.
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

   /// Determines and updates what BEPs a given peer supports based off of the
   /// reserved bytes from the peers handshake.
   ///
   /// This function does not inherently have to be async due to the extremely
   /// light workload it takes on, but there's no reason for it not to be.
   async fn determine_supported(&mut self) {
      if self.reserved[5] == 0x10 {
         self.peer_supports.bep_0010 = true;
         trace!("Peer supports BEP 0010 (extended messages)");
      }
   }

   /// Autonomously handles the connection & messages between a peer. The
   /// from_engine_tx/from_engine_rx is provided to facilitate communication
   /// to this function from the caller (likely TorrentEngine -- if so, this
   /// channel will be used to communicate what pieces TorrentEngine still
   /// needs). to_engine_tx is provided to allow communication from
   /// handle_peer to the caller.
   ///
   /// At the moment, handle_peer is a leecher. In that, it is not a seeder --
   /// it only takes from the torrent swarm. Seeding will be implemented in
   /// the future.
   pub async fn handle_peer(
      mut self, to_engine_tx: broadcast::Sender<PeerResponse>, info_hash: InfoHash, our_id: PeerId,
      stream: Option<PeerStream>, utp_socket: Option<Arc<UtpSocketUdp>>,
      init_bitfield: Option<BitVec<u8>>,
   ) {
      let peer_addr = self.socket_addr();
      let (from_engine_tx, mut from_engine_rx) = mpsc::channel(100);

      if let Err(e) = to_engine_tx.send(PeerResponse::Init {
         from_engine_tx: from_engine_tx.clone(),
         peer_key: peer_addr,
      }) {
         error!(%peer_addr, error = %e, "Failed to send init message to engine");
         return;
      }

      debug!(%peer_addr, "Attempting to connect to peer");
      let stream = if let Some(stream) = stream {
         // For incoming peers (they're connecting to them), since they have already sent
         // their handshake and been verified, we can skip that part.
         debug!(%peer_addr, "Using existing connection from incoming peer");
         stream
      } else {
         // For outgoing peers (we are connecting to them), we should create the stream
         // ourselves and send the handshake & bitfield
         let mut stream = PeerStream::connect(peer_addr, utp_socket).await;
         // Send handshake to peer
         let (peer_id, reserved) = match stream.send_handshake(our_id, Arc::new(info_hash)).await {
            Ok(result) => result,
            Err(e) => {
               error!(%peer_addr, error = %e, "Failed to complete handshake with peer");
               return;
            }
         };

         self.id = Some(*peer_id);
         self.reserved = reserved;
         self.determine_supported().await;
         debug!(%peer_addr, "Completed handshake with outgoing peer");
         stream
      };

      let (mut reader, writer) = stream.split();
      let writer = Arc::new(Mutex::new(writer));

      info!(%peer_addr, "Successfully connected to peer");

      // Wait for peer to be unchoked, then send these messages.
      let writer_clone = writer.clone();
      let am_choked_clone = self.state.am_choked.clone();
      let interested_clone = self.state.interested.clone();
      tokio::spawn(async move {
         let bitfield_to_send = init_bitfield.unwrap_or(BitVec::EMPTY);
         let piece_count = bitfield_to_send.len();

         trace!(%peer_addr, piece_count, "Waiting to send initial messages");

         loop {
            if !am_choked_clone.load(Ordering::Acquire) {
               // Send an "interested" message (NOTE: I'm 99% sure we can send this before
               // being unchoked)
               {
                  let mut writer_guard = writer_clone.lock().await;
                  if let Err(e) = writer_guard.send(PeerMessages::Interested).await {
                     error!(%peer_addr, error = %e, "Failed to send interested message");
                     return;
                  }
                  interested_clone.store(true, Ordering::Release);
               }

               debug!(%peer_addr, "Sent interested message to peer");

               // Send empty bitfield. This may need to be refactored in the future to account
               // for seeding.
               {
                  let mut writer_guard = writer_clone.lock().await;
                  if let Err(e) = writer_guard
                     .send(PeerMessages::Bitfield(bitfield_to_send.clone()))
                     .await
                  {
                     error!(%peer_addr, error = %e, "Failed to send bitfield");
                     return;
                  }
               }

               debug!(%peer_addr, piece_count, "Sent bitfield to peer");
               break;
            }
            sleep(Duration::from_millis(250)).await;
         }
      });

      // Start of request/piece message loop

      // Clone state to use (will be moved)
      let state = self.state.clone();

      // Create two channels to allow for a chain of messages to be handled. For
      // instance:
      //
      // Peer sends Extended Handshake
      // We send request for piece 0 of metadata (BEP 0009)
      // Peer sends piece 0 of metadata
      // We send request for piece 1 of metadata
      // Peer sends piece 1 of metadata
      //
      // Naturally, we cannot handle this in a single call to handle_recv or
      // handle_peer_command. Additionally, due to the nature of stream.split(),
      // we cannot easily include both the writer and the reader in a single
      // call to either handle_recv or handle_peer_command.
      //
      // Thus, the best option is to have "inner" channels so something like this is
      // possible:
      //
      // Read from Peer
      // `inner_send_tx.send(some_response)`
      // Send some_response to Peer
      let (inner_send_tx, mut inner_send_rx): (Sender<PeerCommand>, Receiver<PeerCommand>) =
         mpsc::channel(32);
      let (inner_recv_tx, mut inner_recv_rx) = mpsc::channel(32);

      debug!(%peer_addr, "Starting peer message handling loop");

      // Continuously loop and handle messages from reader and from_engine_rx. The
      // only downside with this setup is that messages can only be handled one
      // at a time. But realistically, there are few chokepoints that we would
      // reach with this setup.
      loop {
         tokio::select! {
            message = inner_send_rx.recv() => {
               if let Some(inner) = message {
                  self.handle_send(
                     inner,
                     writer.clone(),
                     to_engine_tx.clone(),
                     from_engine_tx.clone(),
                     inner_recv_tx.clone(),
                  )
                  .await;
               }
            }
            message = inner_recv_rx.recv() => {
               if let Some(inner) = message {
                  self.handle_recv(
                     inner,
                     to_engine_tx.clone(),
                     from_engine_tx.clone(),
                     inner_send_tx.clone(),
                     info_hash,
                  )
                  .await;
               }
            }
            message = reader.recv() => {
               match message {
                  Ok(inner) => {
                     Self::update_message(state.last_message_received.clone());
                     self.handle_recv(
                        inner,
                        to_engine_tx.clone(),
                        from_engine_tx.clone(),
                        inner_send_tx.clone(),
                        info_hash,
                     )
                     .await;
                  }
                  Err(e) => {
                     error!(%peer_addr, error = %e, "Failed to receive message from peer");

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
                        from_engine_tx.clone(),
                        inner_recv_tx.clone(),
                     )
                     .await;
                     Self::update_message(self.state.last_message_sent.clone());
                  }
                  None => {
                     warn!(%peer_addr, "Engine channel closed, terminating peer handler");
                     break;
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

   use bitvec::{bitvec, order::Lsb0};
   use rand::RngCore;
   use tokio::{
      io::{AsyncReadExt, AsyncWriteExt},
      net::TcpListener,
   };
   use tracing_test::traced_test;

   use super::{stream::validate_handshake, *};
   use crate::{parser::MagnetUri, peers::messages::Handshake};

   #[tokio::test]
   #[traced_test]
   async fn test_peer_creation() {
      let peer = Peer::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 6881);
      assert_eq!(peer.ip, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
      assert_eq!(peer.port, 6881);
   }

   #[tokio::test]
   #[traced_test]
   /// As this test contacts a remote peer, it may not always work. However, it
   /// is still included in the general tests for sake of completeness.
   async fn test_peer_connection() {
      // This is a known good peer (as of 06/17/2025) for the torrent located in
      // zenshuu.txt
      let known_good_peer = "81.170.27.38:49999";
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
   /// Keep in mind that this test operates as both the TorrentEngine and the
   /// peer, handling all communication with the peer in such a manner.
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
      assert!(
         validate_handshake(
            &Handshake::from_bytes(&bytes).unwrap(),
            SocketAddr::from_str(peer_addr).unwrap(),
            Arc::new(info_hash),
         )
         .is_ok()
      );

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
