use core::fmt;
use std::{
   fmt::{Debug, Display},
   hash::{Hash as InternalHash, Hasher},
   net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
   sync::{Arc, atomic::Ordering},
   time::Duration,
};

use bitvec::vec::BitVec;
use librqbit_utp::UtpSocketUdp;
pub use peer::id::PeerId;
use peer::{info::PeerInfo, state::PeerState, supports::PeerSupports};
use peer_comms::{
   commands::{PeerCommand, PeerResponse},
   messages::PeerMessages,
   stream::{PeerRecv, PeerSend, PeerStream},
};
use tokio::{
   sync::{
      Mutex, broadcast,
      mpsc::{self, Receiver, Sender},
   },
   time::sleep,
};
use tracing::{debug, error, info, trace, warn};

use crate::hashes::InfoHash;

mod peer;
pub mod peer_comms;

/// It should be noted that the *name* PeerKey is slightly deprecated from
/// previous renditions of libtortillas. The idea of having a type for the "key"
/// of a peer is still completely relevant though.
pub type PeerKey = SocketAddr;

pub const MAGIC_STRING: &[u8] = b"BitTorrent protocol";

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
   pub id: Option<PeerId>,
   pub info: PeerInfo,
}

impl Debug for Peer {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.debug_struct("Peer")
         .field("addr", &self.socket_addr())
         .field("choked", &self.choked())
         .field("interested", &self.interested())
         .field("am_choked", &self.am_choked())
         .field("am_interested", &self.am_interested())
         .field("id", &self.id)
         .finish()
   }
}

impl InternalHash for Peer {
   fn hash<H: Hasher>(&self, state: &mut H) {
      self.socket_addr().hash(state)
   }
}

impl Eq for Peer {}
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

   /// Small helper function for sending messages with to_engine_tx.
   fn send_to_engine(
      to_engine_tx: broadcast::Sender<PeerResponse>, message: PeerResponse, peer_addr: SocketAddr,
   ) {
      if let Err(e) = to_engine_tx.send(message) {
         error!(%peer_addr, error = %e, "Failed to send message to engine");
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
   pub(crate) async fn handle_peer(
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

         self.id = Some(peer_id);
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

      // This is directly accessing self.state when we have helper methods. However,
      // due to move(s), this is the best way to do this (AFAIK).
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
      let (_, mut inner_recv_rx) = mpsc::channel(32);

      debug!(%peer_addr, "Starting peer message handling loop");

      // Continuously loop and handle messages from reader and from_engine_rx. The
      // only downside with this setup is that messages can only be handled one
      // at a time. But realistically, there are few chokepoints that we would
      // reach with this setup.
      loop {
         tokio::select! {
            message = inner_send_rx.recv() => {
               if let Some(inner) = message {
                  self.send_to_peer(
                     inner,
                     writer.clone(),
                     to_engine_tx.clone(),
                     from_engine_tx.clone(),
                  )
                  .await;
               }
            }
            message = inner_recv_rx.recv() => {
               if let Some(inner) = message {
                  self.recv_from_peer(
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
                     self.update_last_message_received();
                     self.recv_from_peer(
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
                     self.send_to_peer(
                        inner,
                        writer.clone(),
                        to_engine_tx.clone(),
                        from_engine_tx.clone(),
                     )
                     .await;
                     self.update_last_message_sent();
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

   use super::{peer_comms::stream::validate_handshake, *};
   use crate::{hashes::Hash, parser::MagnetUri, peers::peer_comms::messages::Handshake};

   #[tokio::test]
   #[traced_test]
   async fn test_peer_creation() {
      let peer = Peer::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 6881);
      assert_eq!(peer.ip, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
      assert_eq!(peer.port, 6881);
   }

   #[tokio::test]
   #[traced_test]
   #[ignore = "No known good peer"] // Until we find a stable peer
   /// As this test contacts a remote peer, it may not always work. However, it
   /// is still included in the general tests for sake of completeness.
   async fn test_peer_connection() {
      // This is NO LONGER a known good peer.
      let known_good_peer = "81.170.27.38:49999";
      let peer = Peer::from_socket_addr(SocketAddr::from_str(known_good_peer).unwrap());
      let (to_engine_tx, mut to_engine_rx) = broadcast::channel(100);

      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/cachy-desktop-linux-250713.txt");
      let magnet_uri = tokio::fs::read_to_string(path).await.unwrap();
      let data = MagnetUri::parse(magnet_uri).unwrap();

      // Stuff for generating our_id (yes, literally our ID as a peer in the network)
      let our_id = PeerId::new();

      peer
         .handle_peer(
            to_engine_tx,
            data.info_hash().unwrap(),
            our_id,
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
   #[ignore = "Not stable yet"]
   /// Keep in mind that this test operates as both the TorrentEngine and the
   /// peer, handling all communication with the peer in such a manner.
   async fn test_peer_to_peer_pieces() {
      let info_hash = Hash::new(rand::random::<[u8; 20]>());

      // Start listener
      let peer_addr = "127.0.0.1:9883";
      let listener = TcpListener::bind(peer_addr).await.unwrap();

      // Start peer
      let peer = Peer::from_socket_addr(SocketAddr::from_str(peer_addr).unwrap());
      let peer_id = PeerId::new();
      let (to_engine_tx, mut to_engine_rx) = broadcast::channel(100);

      tokio::spawn(async move {
         peer
            .handle_peer(to_engine_tx, info_hash, peer_id, None, None, None)
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
      let our_id = PeerId::new();

      // peer_stream.send_handshake() does not work?
      peer_stream
         .write_all(&Handshake::new(Arc::new(info_hash), our_id).to_bytes())
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
