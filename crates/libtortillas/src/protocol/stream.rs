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
use bytes::{Buf, BytesMut};
use librqbit_utp::{UtpSocketUdp, UtpStream, UtpStreamReadHalf, UtpStreamWriteHalf};
use tokio::{
   io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
   net::{TcpStream, tcp},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, instrument, trace};

use super::messages::{Handshake, PeerMessages};
use crate::{
   errors::PeerActorError,
   hashes::InfoHash,
   peer::{MAGIC_STRING, PeerId, PeerState},
};

enum PeerTransport {
   Tcp(TcpStream),
   Utp(UtpStream),
}

/// A TCP or uTP peer connection with buffered protocol reads and traffic
/// accounting.
pub struct PeerStream {
   transport: PeerTransport,
   read_buffer: BytesMut,
   peer_state: PeerState,
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

      // Safety check -- BitTorrent docs do not specify if KeepAlive messages have an
      // ID (and I'm pretty sure they don't)
      if length == 0 {
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
   pub fn tcp(stream: TcpStream) -> Self {
      Self {
         transport: PeerTransport::Tcp(stream),
         read_buffer: BytesMut::new(),
         peer_state: PeerState::default(),
      }
   }

   pub fn utp(stream: UtpStream) -> Self {
      Self {
         transport: PeerTransport::Utp(stream),
         read_buffer: BytesMut::new(),
         peer_state: PeerState::default(),
      }
   }

   pub(crate) fn peer_state(&self) -> PeerState {
      self.peer_state.clone()
   }

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
                  Ok(PeerStream::utp(stream?))
             },
             stream = TcpStream::connect(peer_addr) => {
                 trace!(protocol = "TCP", "Connected to peer");
                  Ok(PeerStream::tcp(stream?))
             }
         }
      } else {
         trace!(protocol = "TCP", "Connecting to peer");
         Ok(PeerStream::tcp(TcpStream::connect(peer_addr).await?))
      }
   }

   /// Sends a handshake to a peer. Returns nothing if the handshake is sent
   /// without error.
   pub async fn send_handshake(
      &mut self, our_id: PeerId, info_hash: Arc<InfoHash>,
   ) -> Result<(), PeerActorError> {
      let handshake = Handshake::new(info_hash.clone(), our_id);

      self.write_all(&handshake.to_bytes()).await?;
      Ok(())
   }

   /// Receives an incoming handshake from a peer.
   pub async fn recv_handshake_message(&mut self) -> Result<Handshake, PeerActorError> {
      let protocol_len = self.read_u8().await?;
      let mut buf = Vec::with_capacity(1 + protocol_len as usize + 8 + 40);
      buf.push(protocol_len);

      let mut rest = vec![0u8; protocol_len as usize + 8 + 40];
      self.read_exact(&mut rest).await?;
      buf.extend_from_slice(&rest);

      Handshake::from_bytes(&buf).map_err(|e| PeerActorError::HandshakeFailed {
         reason: e.to_string(),
      })
   }

   /// Receives an incoming handshake from a peer.
   ///
   /// Will fail if the next message is not a handshake.
   pub async fn recv_handshake(&mut self) -> Result<(PeerId, [u8; 8]), PeerActorError> {
      let Handshake {
         peer_id, reserved, ..
      } = self.recv_handshake_message().await?;

      Ok((peer_id, reserved))
   }

   /// Returns the addr of the connected peer
   pub fn remote_addr(&self) -> Result<SocketAddr> {
      match &self.transport {
         PeerTransport::Tcp(stream) => Ok(stream.peer_addr()?),
         PeerTransport::Utp(stream) => Ok(stream.remote_addr()),
      }
   }

   /// Splits the PeerStream into separate reader and writer halves.
   ///
   /// Any bytes already buffered by [`PeerRecv::recv`] are transferred to the
   /// reader and returned before it reads from the transport.
   pub fn split(self) -> (PeerReader, PeerWriter) {
      let Self {
         transport,
         read_buffer,
         peer_state,
      } = self;
      let (reader, writer) = match transport {
         PeerTransport::Tcp(stream) => {
            let (reader, writer) = stream.into_split();
            (PeerReadHalf::Tcp(reader), PeerWriteHalf::Tcp(writer))
         }
         PeerTransport::Utp(stream) => {
            let (reader, writer) = stream.split();
            (PeerReadHalf::Utp(reader), PeerWriteHalf::Utp(writer))
         }
      };
      (
         PeerReader {
            reader,
            read_buffer,
            peer_state: peer_state.clone(),
         },
         PeerWriter { writer, peer_state },
      )
   }

   pub fn protocol(&self) -> &'static str {
      match &self.transport {
         PeerTransport::Tcp(_) => "TCP",
         PeerTransport::Utp(_) => "uTP",
      }
   }
}

impl Display for PeerStream {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      match self.remote_addr() {
         Ok(addr) => write!(f, "{}@{}", self.protocol(), addr),
         Err(_) => write!(f, "{}@<disconnected>", self.protocol()),
      }
   }
}

impl PeerSend for PeerStream {}
#[async_trait]
impl PeerRecv for PeerStream {
   async fn recv(&mut self) -> Result<PeerMessages, PeerActorError> {
      loop {
         if let Some(message) = buffered_message(&mut self.read_buffer) {
            return message;
         }
         let bytes_read = match &mut self.transport {
            PeerTransport::Tcp(stream) => stream.read_buf(&mut self.read_buffer).await?,
            PeerTransport::Utp(stream) => stream.read_buf(&mut self.read_buffer).await?,
         };
         if bytes_read == 0 {
            return Err(PeerActorError::ReceiveFailed(io::Error::new(
               io::ErrorKind::UnexpectedEof,
               "peer closed connection",
            )));
         }
         self.peer_state.increment_bytes_downloaded(bytes_read);
      }
   }
}

fn buffered_message(read_buffer: &mut BytesMut) -> Option<Result<PeerMessages, PeerActorError>> {
   if read_buffer.len() < 4 {
      return None;
   }

   let length = u32::from_be_bytes(
      read_buffer[..4]
         .try_into()
         .expect("slice has exactly 4 bytes"),
   ) as usize;
   let frame_len = 4 + length;

   if length == 0 {
      read_buffer.advance(4);
      return Some(Ok(PeerMessages::KeepAlive));
   }

   if read_buffer.len() < frame_len {
      return None;
   }

   let frame = read_buffer.split_to(frame_len).freeze();
   Some(PeerMessages::from_bytes(frame))
}

impl AsyncRead for PeerStream {
   fn poll_read(
      mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>,
   ) -> Poll<io::Result<()>> {
      let before = buf.filled().len();
      let result = match &mut self.transport {
         PeerTransport::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
         PeerTransport::Utp(stream) => Pin::new(stream).poll_read(cx, buf),
      };
      if matches!(&result, Poll::Ready(Ok(()))) {
         self
            .peer_state
            .increment_bytes_downloaded(buf.filled().len().saturating_sub(before));
      }
      result
   }
}

impl AsyncWrite for PeerStream {
   fn poll_write(
      mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8],
   ) -> Poll<Result<usize, io::Error>> {
      let result = match &mut self.transport {
         PeerTransport::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
         PeerTransport::Utp(stream) => Pin::new(stream).poll_write(cx, buf),
      };
      if let Poll::Ready(Ok(bytes_written)) = &result {
         self.peer_state.increment_bytes_uploaded(*bytes_written);
      }
      result
   }

   fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
      match &mut self.transport {
         PeerTransport::Tcp(stream) => Pin::new(stream).poll_flush(cx),
         PeerTransport::Utp(stream) => Pin::new(stream).poll_flush(cx),
      }
   }

   fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
      match &mut self.transport {
         PeerTransport::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
         PeerTransport::Utp(stream) => Pin::new(stream).poll_shutdown(cx),
      }
   }
}

enum PeerReadHalf {
   Tcp(tcp::OwnedReadHalf),
   Utp(UtpStreamReadHalf),
}

enum PeerWriteHalf {
   Tcp(tcp::OwnedWriteHalf),
   Utp(UtpStreamWriteHalf),
}

pub struct PeerReader {
   reader: PeerReadHalf,
   read_buffer: BytesMut,
   peer_state: PeerState,
}

pub struct PeerWriter {
   writer: PeerWriteHalf,
   peer_state: PeerState,
}

impl AsyncRead for PeerReader {
   fn poll_read(
      mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>,
   ) -> Poll<io::Result<()>> {
      if !self.read_buffer.is_empty() {
         let length = buf.remaining().min(self.read_buffer.len());
         buf.put_slice(&self.read_buffer.split_to(length));
         return Poll::Ready(Ok(()));
      }
      let before = buf.filled().len();
      let result = match &mut self.reader {
         PeerReadHalf::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
         PeerReadHalf::Utp(stream) => Pin::new(stream).poll_read(cx, buf),
      };
      if matches!(&result, Poll::Ready(Ok(()))) {
         self
            .peer_state
            .increment_bytes_downloaded(buf.filled().len().saturating_sub(before));
      }
      result
   }
}

impl AsyncWrite for PeerWriter {
   fn poll_write(
      mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8],
   ) -> Poll<Result<usize, io::Error>> {
      let result = match &mut self.writer {
         PeerWriteHalf::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
         PeerWriteHalf::Utp(stream) => Pin::new(stream).poll_write(cx, buf),
      };
      if let Poll::Ready(Ok(bytes_written)) = &result {
         self.peer_state.increment_bytes_uploaded(*bytes_written);
      }
      result
   }

   fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
      match &mut self.writer {
         PeerWriteHalf::Tcp(stream) => Pin::new(stream).poll_flush(cx),
         PeerWriteHalf::Utp(stream) => Pin::new(stream).poll_flush(cx),
      }
   }

   fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
      match &mut self.writer {
         PeerWriteHalf::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
         PeerWriteHalf::Utp(stream) => Pin::new(stream).poll_shutdown(cx),
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
   validate_handshake_protocol(received_handshake, peer_addr)?;

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

pub fn validate_handshake_protocol(
   received_handshake: &Handshake, peer_addr: SocketAddr,
) -> Result<(), PeerActorError> {
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

   Ok(())
}

#[cfg(test)]
mod tests {
   use std::time::Duration;

   use tokio::{io::AsyncWriteExt, net::TcpListener, time::timeout};
   use tracing_test::traced_test;

   use super::*;
   use crate::{
      errors::PeerActorError,
      hashes::Hash,
      protocol::messages::Handshake,
      testing::{self, LocalPeer},
   };

   #[tokio::test]
   #[traced_test]
   async fn peer_stream_when_handshake_is_valid_then_returns_peer_id() {
      let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
      let addr = listener.local_addr().unwrap();

      let info_hash = Arc::new(Hash::new([1u8; 20]));
      let client_id = PeerId::new();

      // Spawn client that sends handshake
      let client_info_hash = info_hash.clone();
      let client = tokio::spawn(async move {
         let mut stream = PeerStream::tcp(TcpStream::connect(addr).await.unwrap());

         stream
            .send_handshake(client_id, client_info_hash)
            .await
            .unwrap();
      });

      // Server side
      let (stream, _) = timeout(Duration::from_secs(1), listener.accept())
         .await
         .expect("client should connect before timeout")
         .unwrap();
      let mut peer_stream = PeerStream::tcp(stream);

      let (incoming_id, _) = timeout(Duration::from_secs(1), peer_stream.recv_handshake())
         .await
         .expect("handshake should arrive before timeout")
         .unwrap();
      client.await.expect("client task should not panic");

      assert_eq!(incoming_id, client_id);
   }

   #[tokio::test]
   async fn peer_stream_when_frames_are_exchanged_then_counts_every_wire_byte() {
      let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
      let addr = listener.local_addr().unwrap();
      let info_hash = Arc::new(Hash::new([1u8; 20]));
      let client_id = PeerId::new();
      let server_id = PeerId::new();
      let handshake_len = Handshake::new(info_hash.clone(), client_id)
         .to_bytes()
         .len();
      let interested_len = PeerMessages::Interested.to_bytes().unwrap().len();
      let piece = PeerMessages::Piece(2, 4, b"payload".as_slice().into());
      let piece_len = piece.to_bytes().unwrap().len();

      let server_info_hash = info_hash.clone();
      let server = tokio::spawn(async move {
         let (stream, _) = listener.accept().await.unwrap();
         let mut stream = PeerStream::tcp(stream);
         stream.recv_handshake_message().await.unwrap();
         stream
            .send_handshake(server_id, server_info_hash)
            .await
            .unwrap();
         assert_eq!(stream.recv().await.unwrap(), PeerMessages::Interested);
         stream.send(piece).await.unwrap();
         stream.peer_state().traffic_totals()
      });

      let mut client = PeerStream::tcp(TcpStream::connect(addr).await.unwrap());
      client.send_handshake(client_id, info_hash).await.unwrap();
      client.recv_handshake_message().await.unwrap();
      client.send(PeerMessages::Interested).await.unwrap();
      assert!(matches!(
         client.recv().await.unwrap(),
         PeerMessages::Piece(2, 4, _)
      ));

      let client_totals = client.peer_state().traffic_totals();
      let server_totals = server.await.unwrap();
      assert_eq!(
         client_totals.uploaded.0,
         (handshake_len + interested_len) as u64
      );
      assert_eq!(
         client_totals.downloaded.0,
         (handshake_len + piece_len) as u64
      );
      assert_eq!(server_totals.uploaded, client_totals.downloaded);
      assert_eq!(server_totals.downloaded, client_totals.uploaded);
   }

   #[tokio::test]
   async fn peer_stream_transfers_buffered_data_to_split_reader() {
      let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
      let address = listener.local_addr().unwrap();
      let connect = tokio::spawn(TcpStream::connect(address));
      let (stream, _) = listener.accept().await.unwrap();
      let _client = connect.await.unwrap().unwrap();
      let mut stream = PeerStream::tcp(stream);
      stream.read_buffer.extend_from_slice(&[0, 0, 0, 0]);
      let (mut reader, _writer) = stream.split();
      let mut buffered = [1; 4];

      reader.read_exact(&mut buffered).await.unwrap();

      assert_eq!(buffered, [0; 4]);
   }

   #[tokio::test]
   async fn peer_stream_when_local_peer_is_available_then_completes_handshake() {
      let remote_peer_id = PeerId::new();
      let local_peer = LocalPeer::start(remote_peer_id, vec![PeerMessages::KeepAlive])
         .await
         .unwrap();
      let mut stream = PeerStream::connect(local_peer.peer().socket_addr(), None)
         .await
         .unwrap();
      let info_hash = Arc::new(testing::test_info_hash());
      let client_id = PeerId::new();

      stream
         .send_handshake(client_id, info_hash.clone())
         .await
         .unwrap();
      let (received_peer_id, _) = timeout(Duration::from_secs(1), stream.recv_handshake())
         .await
         .expect("handshake should arrive before timeout")
         .unwrap();
      let message = timeout(Duration::from_secs(1), stream.recv())
         .await
         .expect("message should arrive before timeout")
         .unwrap();

      assert_eq!(received_peer_id, remote_peer_id);
      assert_eq!(message, PeerMessages::KeepAlive);
      let handshakes = local_peer.handshakes().await;
      assert_eq!(handshakes[0].peer_id, client_id);
      assert_eq!(handshakes[0].info_hash, info_hash);
   }

   #[tokio::test]
   async fn peer_stream_when_handshake_protocol_is_invalid_then_returns_handshake() {
      let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
      let addr = listener.local_addr().unwrap();

      let info_hash = Arc::new(Hash::new([1u8; 20]));
      let mut handshake = Handshake::new(info_hash, PeerId::new());
      handshake.protocol = "not bittorrent".into();
      let handshake_bytes = handshake.to_bytes();

      let client = tokio::spawn(async move {
         let mut stream = TcpStream::connect(addr).await.unwrap();
         stream.write_all(&handshake_bytes).await.unwrap();
      });

      let (stream, _) = timeout(Duration::from_secs(1), listener.accept())
         .await
         .expect("client should connect before timeout")
         .unwrap();
      let mut peer_stream = PeerStream::tcp(stream);

      let received_handshake =
         timeout(Duration::from_secs(1), peer_stream.recv_handshake_message())
            .await
            .expect("handshake should arrive before timeout")
            .unwrap();
      client.await.expect("client task should not panic");

      assert_eq!(received_handshake.protocol.as_ref(), b"not bittorrent");
   }

   #[test]
   fn validate_handshake_when_protocol_is_invalid_then_returns_magic_mismatch() {
      let info_hash = Arc::new(Hash::new([1u8; 20]));
      let mut handshake = Handshake::new(info_hash.clone(), PeerId::new());
      handshake.protocol = "not bittorrent".into();

      let error =
         validate_handshake(&handshake, "127.0.0.1:6881".parse().unwrap(), info_hash).unwrap_err();

      assert!(matches!(
         error,
         PeerActorError::HandshakeMagicMismatch { .. }
      ));
   }

   #[test]
   fn validate_handshake_protocol_when_protocol_is_invalid_then_returns_magic_mismatch() {
      let info_hash = Arc::new(Hash::new([1u8; 20]));
      let mut handshake = Handshake::new(info_hash, PeerId::new());
      handshake.protocol = "not bittorrent".into();

      let error =
         validate_handshake_protocol(&handshake, "127.0.0.1:6881".parse().unwrap()).unwrap_err();

      assert!(matches!(
         error,
         PeerActorError::HandshakeMagicMismatch { .. }
      ));
   }

   #[test]
   fn validate_handshake_when_info_hash_differs_then_returns_info_hash_mismatch() {
      let expected_info_hash = Arc::new(Hash::new([1u8; 20]));
      let received_info_hash = Arc::new(Hash::new([2u8; 20]));
      let handshake = Handshake::new(received_info_hash, PeerId::new());

      let error = validate_handshake(
         &handshake,
         "127.0.0.1:6881".parse().unwrap(),
         expected_info_hash,
      )
      .unwrap_err();

      assert!(matches!(
         error,
         PeerActorError::HandshakeInfoHashMismatch { .. }
      ));
   }
}
