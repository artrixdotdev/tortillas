use crate::errors::PeerTransportError;
use crate::hashes::Hash;
use crate::peers::InfoHash;
use crate::peers::messages::Handshake;
use anyhow::Result;
use anyhow::anyhow;
use async_trait::async_trait;
use librqbit_utp::UtpSocketUdp;
use librqbit_utp::UtpStreamReadHalf;
use librqbit_utp::UtpStreamWriteHalf;
use tokio::net::tcp;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::instrument;

use std::fmt;
use std::fmt::Display;
use std::sync::Arc;
use std::{
   net::SocketAddr,
   pin::Pin,
   task::{Context, Poll},
};

use super::MAGIC_STRING;
use super::PeerId;
use super::messages::PeerMessages;
use librqbit_utp::UtpStream;
use tokio::{
   io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
   net::TcpStream,
};
use tracing::trace;
/// A very simple enum to help differentiate between streams. TcpStream and UtpStream are so
/// incredibly similar in functionality that it's ususally possible to simply make a blanket
/// function, such as [write_all](PeerStream::write_all)
pub enum PeerStream {
   Tcp(TcpStream),
   Utp(UtpStream),
}

#[async_trait]
pub trait PeerSend: AsyncWrite + Unpin {
   /// Sends a PeerMessage to a peer.
   async fn send(&mut self, data: PeerMessages) -> Result<(), PeerTransportError> {
      self
         .write_all(&data.to_bytes().unwrap())
         .await
         .map_err(|e| {
            error!("Failed to send message to peer: {e}");
            PeerTransportError::MessageFailed
         })
   }
}

#[async_trait]
pub trait PeerRecv: AsyncRead + Unpin {
   /// Receives data from a peers stream. In other words, if you wish to directly contact a peer,
   /// use this function.
   async fn recv(&mut self) -> Result<PeerMessages, PeerTransportError> {
      // First 4 bytes is the big endian encoded length field and the 5th byte is a PeerMessage tag
      let mut buf = vec![0; 5];

      self.read_exact(&mut buf).await.map_err(|e| {
         error!("Error occurred when reading the peer's response: {e}");
         PeerTransportError::InvalidPeerResponse("Error occured".into())
      })?;

      let length = u32::from_be_bytes(buf[..4].try_into().unwrap());

      trace!(
         message_type = buf[4],
         length = length,
         "Recieved message headers, requesting rest..."
      );

      // Why do we have to do length - 1? Only a higher power knows.
      let mut rest = vec![0; (length - 1) as usize];

      self.read_exact(&mut rest).await.map_err(|e| {
         error!("Error occurred when reading the peer's response: {e}");
         PeerTransportError::InvalidPeerResponse("Error occured".into())
      })?;
      let full_length = length + buf.len() as u32;

      debug!("Read {} action ({} bytes)", buf[4], full_length,);
      buf.extend_from_slice(&rest);

      PeerMessages::from_bytes(buf)
   }
}

impl PeerStream {
   /// Connect to a peer with the given peer_addr (ip & port in the form of a
   /// [SocketAddr](std::net::SocketAddr))
   ///
   /// When connecting to a peer, we attempt to connect over both TCP and uTP, and use whichever
   /// one works. While this may seem "not to spec", this is how the transmission BitTorrent
   /// client does it:
   /// https://github.com/transmission/transmission/discussions/7603
   ///
   /// utp_socket should be None ONLY for testing, when we only wish to utilize a TcpStream.
   pub async fn connect(peer_addr: SocketAddr, utp_socket: Option<Arc<UtpSocketUdp>>) -> Self {
      trace!("Attemping connection to {}", peer_addr);
      if let Some(utp_socket) = utp_socket {
         tokio::select! {
            stream = utp_socket.connect(peer_addr) => {
               debug!(peer_addr = %peer_addr,"Connected to peer over uTP");
               PeerStream::Utp(stream.unwrap())},
            stream = TcpStream::connect(peer_addr) => {
               debug!(peer_addr = %peer_addr,"Connected to peer over TCP");
               PeerStream::Tcp(stream.unwrap())}
         }
      } else {
         debug!(peer_addr = %peer_addr, "Connecting to peer over TCP");
         PeerStream::Tcp(TcpStream::connect(peer_addr).await.unwrap())
      }
   }

   /// Handshakes with a peer and returns the socket address of the peer. This socket address is
   /// also a (PeerKey)[super::PeerKey].
   #[instrument(
      skip(self)
      fields(
         protocol = self.protocol(),
         remote_addr = self.remote_addr().unwrap().to_string(),
         info_hash = info_hash.to_string(),
         our_id = our_id.to_string()
      )
   )]
   pub async fn send_handshake(
      &mut self,
      our_id: PeerId,
      info_hash: Arc<InfoHash>,
   ) -> Result<PeerId, PeerTransportError> {
      let handshake = Handshake::new(info_hash.clone(), our_id.clone());
      let remote_addr = self.remote_addr().unwrap();
      self.write_all(&handshake.to_bytes()).await.unwrap();
      trace!("Sent handshake to peer");

      // Calculate expected size for response
      // 1 byte + protocol + reserved + hashes
      const EXPECTED_SIZE: usize = 1 + MAGIC_STRING.len() + 8 + 40;
      let mut buf = [0u8; EXPECTED_SIZE];

      // Read response handshake
      self.read_exact(&mut buf).await.map_err(|e| {
         error!("Failed to read handshake from peer {}: {}", remote_addr, e);
         PeerTransportError::ConnectionFailed(remote_addr.to_string())
      })?;

      let handshake =
         Handshake::from_bytes(&buf).map_err(|e| PeerTransportError::Other(anyhow!("{e}")))?;

      validate_handshake(&handshake, remote_addr, info_hash)?;

      info!(%remote_addr, "Peer connected");

      Ok(handshake.peer_id)
   }

   /// Receives an incoming handshake from a peer.
   #[instrument(
      skip(self)
      fields(
         protocol = self.protocol(),
         remote_addr = self.remote_addr().unwrap().to_string(),
         info_hash = info_hash.to_string(),
         our_id = id.to_string()
      )
   )]
   pub async fn receive_handshake(
      &mut self,
      info_hash: Arc<InfoHash>,
      id: Arc<Hash<20>>,
   ) -> Result<PeerId, PeerTransportError> {
      // First 4 bytes is the big endian encoded length field and the 5th byte is a PeerMessage tag
      let mut buf = vec![0; 5];

      self.read_exact(&mut buf).await.map_err(|e| {
         error!("Error occurred when reading the peer's response: {e}");
         PeerTransportError::InvalidPeerResponse("Error occured".into())
      })?;
      let addr = self.remote_addr().unwrap();

      trace!(message_type = buf[4], ip = %addr, "Recieved message headers, requesting rest...");
      let is_handshake = is_handshake(&buf);

      let length = if is_handshake {
         // This is a handshake.
         // The length of a handshake is always 68 and we already have the
         // first 5 bytes of it, so we need 68 - 5 bytes (the current buffer length)

         68 - buf.len() as u32
      } else {
         // This is not a handshake
         // Non handshake messages have a length field from bytes 0-4
         return Err(PeerTransportError::InvalidPeerResponse(
            "Invalid Handshake".into(),
         ));
      };

      let mut rest = vec![0; length as usize];

      self.read_exact(&mut rest).await.map_err(|e| {
         error!("Error occurred when reading the peer's response: {e}");
         PeerTransportError::InvalidPeerResponse("Error occured".into())
      })?;
      let full_length = length + buf.len() as u32;

      debug!(
         "Read {} action ({} bytes) from {} ",
         buf[4], full_length, addr
      );
      buf.extend_from_slice(&rest);

      // Creates a new handshake and sends it
      let message = PeerMessages::from_bytes(buf)?;
      if let PeerMessages::Handshake(handshake) = message {
         validate_handshake(&handshake, addr, info_hash.clone())?;
         let response = PeerMessages::Handshake(Handshake::new(info_hash, id));
         self.send(response).await?;
         info!("Peer {} connected", self.remote_addr().unwrap());
         Ok(handshake.peer_id.clone())
      } else {
         Err(PeerTransportError::InvalidPeerResponse(
            "Invalid peer response".to_string(),
         ))
      }
   }

   /// Returns the addr of the connected peer
   pub fn remote_addr(&self) -> Result<SocketAddr> {
      match self {
         PeerStream::Tcp(s) => Ok(s.peer_addr().unwrap()),
         PeerStream::Utp(s) => Ok(s.remote_addr()),
      }
   }
   /// Splits the PeerStream into separate reader and writer halves
   pub fn split(self) -> (PeerReader, PeerWriter) {
      match self {
         PeerStream::Tcp(stream) => {
            let (reader, writer) = stream.into_split();
            (PeerReader::Tcp(reader), PeerWriter::Tcp(writer))
         }
         PeerStream::Utp(stream) => {
            let (reader, writer) = stream.split();
            (PeerReader::Utp(reader), PeerWriter::Utp(writer))
         }
      }
   }

   pub fn protocol(&self) -> String {
      match self {
         PeerStream::Tcp(_) => "TCP".to_string(),
         PeerStream::Utp(_) => "uTP".to_string(),
      }
   }
}

impl Display for PeerStream {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      write!(f, "{}@{}", self.protocol(), self.remote_addr().unwrap())
   }
}

impl PeerSend for PeerStream {}
impl PeerRecv for PeerStream {}

impl AsyncRead for PeerStream {
   fn poll_read(
      mut self: Pin<&mut Self>,
      cx: &mut Context<'_>,
      buf: &mut ReadBuf<'_>,
   ) -> Poll<io::Result<()>> {
      match &mut *self {
         PeerStream::Tcp(s) => Pin::new(s).poll_read(cx, buf),
         PeerStream::Utp(s) => Pin::new(s).poll_read(cx, buf),
      }
   }
}

impl AsyncWrite for PeerStream {
   fn poll_write(
      mut self: Pin<&mut Self>,
      cx: &mut Context<'_>,
      buf: &[u8],
   ) -> Poll<Result<usize, io::Error>> {
      match &mut *self {
         PeerStream::Tcp(s) => Pin::new(s).poll_write(cx, buf),
         PeerStream::Utp(s) => Pin::new(s).poll_write(cx, buf),
      }
   }

   fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
      match &mut *self {
         PeerStream::Tcp(s) => Pin::new(s).poll_flush(cx),
         PeerStream::Utp(s) => Pin::new(s).poll_flush(cx),
      }
   }

   fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
      match &mut *self {
         PeerStream::Tcp(s) => Pin::new(s).poll_shutdown(cx),
         PeerStream::Utp(s) => Pin::new(s).poll_shutdown(cx),
      }
   }
}

pub enum PeerReader {
   Tcp(tcp::OwnedReadHalf),
   Utp(UtpStreamReadHalf),
}

pub enum PeerWriter {
   Tcp(tcp::OwnedWriteHalf),
   Utp(UtpStreamWriteHalf),
}

impl AsyncRead for PeerReader {
   fn poll_read(
      mut self: Pin<&mut Self>,
      cx: &mut Context<'_>,
      buf: &mut ReadBuf<'_>,
   ) -> Poll<io::Result<()>> {
      match &mut *self {
         PeerReader::Tcp(s) => Pin::new(s).poll_read(cx, buf),
         PeerReader::Utp(s) => Pin::new(s).poll_read(cx, buf),
      }
   }
}

impl AsyncWrite for PeerWriter {
   fn poll_write(
      mut self: Pin<&mut Self>,
      cx: &mut Context<'_>,
      buf: &[u8],
   ) -> Poll<Result<usize, io::Error>> {
      match &mut *self {
         PeerWriter::Tcp(s) => Pin::new(s).poll_write(cx, buf),
         PeerWriter::Utp(s) => Pin::new(s).poll_write(cx, buf),
      }
   }

   fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
      match &mut *self {
         PeerWriter::Tcp(s) => Pin::new(s).poll_flush(cx),
         PeerWriter::Utp(s) => Pin::new(s).poll_flush(cx),
      }
   }

   fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
      match &mut *self {
         PeerWriter::Tcp(s) => Pin::new(s).poll_shutdown(cx),
         PeerWriter::Utp(s) => Pin::new(s).poll_shutdown(cx),
      }
   }
}

// Implement the traits to get the send/recv methods
impl PeerRecv for PeerReader {}
impl PeerSend for PeerWriter {}

/// Takes in a received handshake and returns the handshake we should respond with as well as the new peer. It preassigns the our_id to the peer.
pub(super) fn validate_handshake(
   received_handshake: &Handshake,
   peer_addr: SocketAddr,
   info_hash: Arc<InfoHash>,
) -> Result<(), PeerTransportError> {
   // Validate protocol string
   if MAGIC_STRING != received_handshake.protocol {
      error!("Invalid magic string received from peer {}", peer_addr);
      return Err(PeerTransportError::InvalidMagicString {
         received: String::from_utf8_lossy(&received_handshake.protocol).into(),
         expected: String::from_utf8_lossy(MAGIC_STRING).into(),
      });
   }

   // Validate info hash
   if info_hash.clone() != received_handshake.info_hash {
      error!("Invalid info hash received from peer {}", peer_addr);
      return Err(PeerTransportError::InvalidInfoHash {
         received: received_handshake.info_hash.to_hex(),
         expected: info_hash.clone().to_hex(),
      });
   }

   Ok(())
}

/// Checks to see if a peer message is a handshake using the first 5 bytes.
fn is_handshake(buf: &[u8]) -> bool {
   buf[0] as usize == MAGIC_STRING.len() && buf[1..5] == MAGIC_STRING[0..4]
}

#[cfg(test)]
mod tests {
   use tokio::net::TcpListener;
   use tracing_test::traced_test;

   use super::*;

   #[tokio::test]
   #[traced_test]
   async fn test_peer_stream_receive_handshake_success() {
      let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
      let addr = listener.local_addr().unwrap();

      let info_hash = Arc::new(Hash::new([1u8; 20]));
      let server_id = Arc::new(Hash::new([2u8; 20]));
      let client_id = Arc::new(Hash::new([3u8; 20]));

      // Spawn client that sends handshake
      let client_info_hash = info_hash.clone();
      let client_server_id = server_id.clone();
      let client_peer_id = client_id.clone();
      tokio::spawn(async move {
         let mut stream = PeerStream::Tcp(TcpStream::connect(addr).await.unwrap());

         let response = stream
            .send_handshake(client_peer_id, client_info_hash)
            .await
            .unwrap();

         assert_eq!(response, client_server_id);
      });

      // Server side
      let (stream, _) = listener.accept().await.unwrap();
      let mut peer_stream = PeerStream::Tcp(stream);

      let response = peer_stream
         .receive_handshake(info_hash, server_id.clone())
         .await
         .unwrap();

      assert_eq!(response, client_id.clone());
   }
}
