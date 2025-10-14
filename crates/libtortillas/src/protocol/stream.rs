use std::{
   fmt,
   fmt::Display,
   net::SocketAddr,
   pin::Pin,
   sync::Arc,
   task::{Context, Poll},
};

use anyhow::Result;
use async_trait::async_trait;
use bytes::BytesMut;
use librqbit_utp::{UtpSocketUdp, UtpStream, UtpStreamReadHalf, UtpStreamWriteHalf};
use tokio::{
   io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
   net::{TcpStream, tcp},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, instrument, trace};

use super::messages::{Handshake, PeerMessages};
use crate::{
   errors::PeerActorError,
   hashes::InfoHash,
   peer::{MAGIC_STRING, PeerId},
};

/// A very simple enum to help differentiate between streams. TcpStream and
/// UtpStream are so incredibly similar in functionality that it's ususally
/// possible to simply make a blanket function as it implements both [AsyncRead]
/// and [AsyncWrite]
pub enum PeerStream {
   Tcp(TcpStream),
   Utp(UtpStream),
}

#[async_trait]
pub trait PeerSend: AsyncWrite + Unpin {
   /// Sends a PeerMessage to a peer.
   async fn send(&mut self, data: PeerMessages) -> Result<(), PeerActorError> {
      let bytes = data.to_bytes()?;
      self.write_all(&bytes).await.map_err(|e| {
         error!(error = %e, "Failed to send message to peer");
         PeerActorError::SendFailed(e.to_string())
      })
   }

   /// Sends a message to a peer with a cancellation support, returning an
   /// error if the operation is cancel
   async fn send_with_cancel(
      &mut self, data: PeerMessages, token: CancellationToken,
   ) -> Result<(), PeerActorError> {
      tokio::select! {
         _ = token.cancelled() => {
            trace!("Sending message to peer was cancelled");
            return Err(PeerActorError::MessageCancelled);

         },
         result = self.send(data) => {
            result
         }
      }
   }
}

#[async_trait]
pub trait PeerRecv: AsyncRead + Unpin {
   /// Receives data from a peers stream. In other words, if you wish to
   /// directly contact a peer, use this function.
   async fn recv(&mut self) -> Result<PeerMessages, PeerActorError> {
      // First 4 bytes is the big endian encoded length field and the 5th byte is a
      // PeerMessage tag
      let mut length_buf = [0u8; 4];

      self
         .read_exact(&mut length_buf)
         .await
         .map_err(PeerActorError::ReceiveFailed)?;

      let length = u32::from_be_bytes(length_buf);

      trace!(message_length = length, "Received message length header");

      // Safety check -- BitTorrent docs do not specify if KeepAlive messages have an
      // ID (and I'm pretty sure they don't)
      if length == 0 {
         trace!("Received KeepAlive message");
         return Ok(PeerMessages::KeepAlive);
      }

      let mut message_buf = BytesMut::with_capacity(4 + length as usize);
      message_buf.extend_from_slice(&length_buf);

      let mut message_type = [0u8; 1];
      self.read_exact(&mut message_type).await.map_err(|e| {
         error!(error = %e, "Failed to read message type from peer");
         PeerActorError::ReceiveFailed(e)
      })?;

      message_buf.extend_from_slice(&message_type);

      // Read the rest of the message payload
      let mut rest = vec![0u8; (length - 1) as usize];
      self
         .read_exact(&mut rest)
         .await
         .map_err(PeerActorError::ReceiveFailed)?;

      message_buf.extend_from_slice(&rest);

      PeerMessages::from_bytes(message_buf.freeze())
   }
   /// Receives a message from a peer with cancellation support, returning
   /// an error if the operation is cancelled
   async fn recv_with_cancel(
      &mut self, token: CancellationToken,
   ) -> Result<PeerMessages, PeerActorError> {
      tokio::select! {
         _ = token.cancelled() => {
            trace!("Receiving message from peer was cancelled");
            return Err(PeerActorError::MessageCancelled);
         },
         result = self.recv() => {
            result
         }
      }
   }
}

impl PeerStream {
   /// Connect to a peer with the given peer_addr (ip & port in the form of a
   /// [SocketAddr])
   ///
   /// When connecting to a peer, we attempt to connect over both TCP and uTP,
   /// and use whichever one works. While this may seem "not to spec", this
   /// is how the transmission BitTorrent client does it:
   /// <https://github.com/transmission/transmission/discussions/7603>
   ///
   /// utp_socket should be None ONLY for testing, when we only wish to utilize
   /// a TcpStream.
   #[instrument(fields(peer_addr = %peer_addr))]
   pub async fn connect(
      peer_addr: SocketAddr, utp_socket: Option<Arc<UtpSocketUdp>>,
   ) -> Result<Self, PeerActorError> {
      if let Some(utp_socket) = utp_socket {
         tokio::select! {
             stream = utp_socket.connect(peer_addr) => {
                 trace!(protocol = "uTP", "Connected to peer");
                 Ok(PeerStream::Utp(stream?))
             },
             stream = TcpStream::connect(peer_addr) => {
                 trace!(protocol = "TCP", "Connected to peer");
                 Ok(PeerStream::Tcp(stream?))
             }
         }
      } else {
         trace!(protocol = "TCP", "Connecting to peer");
         Ok(PeerStream::Tcp(TcpStream::connect(peer_addr).await?))
      }
   }

   /// Sends a handshake to a peer. Returns nothing if the handshake is sent
   /// without error.
   pub async fn send_handshake(
      &mut self, our_id: PeerId, info_hash: Arc<InfoHash>,
   ) -> Result<(), PeerActorError> {
      let handshake = Handshake::new(info_hash.clone(), our_id);

      self.write_all(&handshake.to_bytes()).await?;
      trace!("Sent handshake to peer");
      Ok(())
   }

   /// Receives an incoming handshake from a peer.
   ///
   /// Will fail if the next message is not a handshake.
   pub async fn recv_handshake(&mut self) -> Result<(PeerId, [u8; 8]), PeerActorError> {
      // Handshakes will always be 68 bytes
      //
      // mem::size_of::<Handshake>() is 72 bytes, for some reason. We think it might
      // be due to the Arc<>(s) in the Handshake struct
      let mut buf = [0u8; 68];

      self.read_exact(&mut buf).await?;

      let Handshake {
         peer_id, reserved, ..
      } = Handshake::from_bytes(&buf)
         .map_err(|e| PeerActorError::HandshakeFailed { reason: e.into() })?;

      Ok((peer_id, reserved))
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
      mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>,
   ) -> Poll<io::Result<()>> {
      match &mut *self {
         PeerStream::Tcp(s) => Pin::new(s).poll_read(cx, buf),
         PeerStream::Utp(s) => Pin::new(s).poll_read(cx, buf),
      }
   }
}

impl AsyncWrite for PeerStream {
   fn poll_write(
      mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8],
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
      mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>,
   ) -> Poll<io::Result<()>> {
      match &mut *self {
         PeerReader::Tcp(s) => Pin::new(s).poll_read(cx, buf),
         PeerReader::Utp(s) => Pin::new(s).poll_read(cx, buf),
      }
   }
}

impl AsyncWrite for PeerWriter {
   fn poll_write(
      mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8],
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

/// Takes in a received handshake and returns the handshake we should respond
/// with as well as the new peer. It preassigns the our_id to the peer.
pub fn validate_handshake(
   received_handshake: &Handshake, peer_addr: SocketAddr, info_hash: Arc<InfoHash>,
) -> Result<(), PeerActorError> {
   // Validate protocol string
   if MAGIC_STRING != received_handshake.protocol.as_ref() {
      error!(
          peer_addr = %peer_addr,
          received_protocol = %String::from_utf8_lossy(&received_handshake.protocol),
          expected_protocol = %String::from_utf8_lossy(MAGIC_STRING),
          "Invalid protocol string received from peer"
      );
      return Err(PeerActorError::HandshakeMagicMismatch {
         received: String::from_utf8_lossy(&received_handshake.protocol).into(),
         expected: String::from_utf8_lossy(MAGIC_STRING).into(),
      });
   }

   // Validate info hash
   if info_hash.clone() != received_handshake.info_hash {
      error!(
          peer_addr = %peer_addr,
          received_info_hash = %received_handshake.info_hash.to_hex(),
          expected_info_hash = %info_hash.to_hex(),
          "Invalid info hash received from peer"
      );
      return Err(PeerActorError::HandshakeInfoHashMismatch {
         received: received_handshake.info_hash.to_hex(),
         expected: info_hash.clone().to_hex(),
      });
   }

   trace!(
       peer_addr = %peer_addr,
       peer_id = %received_handshake.peer_id,
       "Handshake validation successful"
   );

   Ok(())
}

#[cfg(test)]
mod tests {
   use tokio::net::TcpListener;
   use tracing_test::traced_test;

   use super::*;
   use crate::hashes::Hash;

   #[tokio::test]
   #[traced_test]
   async fn test_peer_stream_receive_handshake_success() {
      let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
      let addr = listener.local_addr().unwrap();

      let info_hash = Arc::new(Hash::new([1u8; 20]));
      let client_id = PeerId::new();

      // Spawn client that sends handshake
      let client_info_hash = info_hash.clone();
      tokio::spawn(async move {
         let mut stream = PeerStream::Tcp(TcpStream::connect(addr).await.unwrap());

         stream
            .send_handshake(client_id, client_info_hash)
            .await
            .unwrap();
      });

      // Server side
      let (stream, _) = listener.accept().await.unwrap();
      let mut peer_stream = PeerStream::Tcp(stream);

      let (incoming_id, _) = peer_stream.recv_handshake().await.unwrap();

      assert_eq!(incoming_id, client_id);
   }
}
