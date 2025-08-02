use std::{
   fmt,
   fmt::Display,
   net::SocketAddr,
   pin::Pin,
   sync::Arc,
   task::{Context, Poll},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use librqbit_utp::{UtpSocketUdp, UtpStream, UtpStreamReadHalf, UtpStreamWriteHalf};
use tokio::{
   io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
   net::{TcpStream, tcp},
};
use tracing::{debug, error, info, instrument, trace, warn};

use super::messages::{Handshake, PeerMessages};
use crate::{
   errors::PeerTransportError,
   hashes::Hash,
   peers::{InfoHash, MAGIC_STRING, PeerId},
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
   async fn send(&mut self, data: PeerMessages) -> Result<(), PeerTransportError> {
      self
         .write_all(&data.to_bytes().unwrap())
         .await
         .map_err(|e| {
            error!(error = %e, "Failed to send message to peer");
            PeerTransportError::MessageFailed
         })
   }
}

#[async_trait]
pub trait PeerRecv: AsyncRead + Unpin {
   /// Receives data from a peers stream. In other words, if you wish to
   /// directly contact a peer, use this function.
   async fn recv(&mut self) -> Result<PeerMessages, PeerTransportError> {
      // First 4 bytes is the big endian encoded length field and the 5th byte is a
      // PeerMessage tag
      let mut buf = vec![0; 4];

      self.read_exact(&mut buf).await.map_err(|e| {
         error!(error = %e, "Failed to read message length from peer");
         PeerTransportError::InvalidPeerResponse("Failed to read message length".into())
      })?;

      let length = u32::from_be_bytes(buf[..4].try_into().unwrap());

      trace!(message_length = length, "Received message length header");

      // Safety check -- BitTorrent docs do not specify if KeepAlive messages have an
      // ID (and I'm pretty sure they don't)
      if length == 0 {
         trace!("Received KeepAlive message");
         return Ok(PeerMessages::KeepAlive);
      }

      let mut message_type = vec![0; 1];
      self.read_exact(&mut message_type).await.map_err(|e| {
         error!(error = %e, "Failed to read message type from peer");
         PeerTransportError::InvalidPeerResponse("Failed to read message type".into())
      })?;

      trace!(
         message_type = message_type[0],
         message_length = length,
         "Received message headers, reading payload"
      );

      buf.extend_from_slice(&message_type);

      // Why do we have to do length - 1? Only a higher power knows.
      let mut rest = vec![0; (length - 1) as usize];

      self.read_exact(&mut rest).await.map_err(|e| {
         error!(error = %e, message_length = length, "Failed to read message payload from peer");
         PeerTransportError::InvalidPeerResponse("Failed to read message payload".into())
      })?;

      let full_length = length + buf.len() as u32;

      trace!(
         message_type = buf[4],
         total_bytes = full_length,
         "Successfully read complete message from peer"
      );
      buf.extend_from_slice(&rest);

      PeerMessages::from_bytes(buf)
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
   pub async fn connect(peer_addr: SocketAddr, utp_socket: Option<Arc<UtpSocketUdp>>) -> Self {
      trace!(peer_addr = %peer_addr, "Attempting connection to peer");
      if let Some(utp_socket) = utp_socket {
         tokio::select! {
             stream = utp_socket.connect(peer_addr) => {
                 debug!(peer_addr = %peer_addr, protocol = "uTP", "Connected to peer");
                 PeerStream::Utp(stream.unwrap())
             },
             stream = TcpStream::connect(peer_addr) => {
                 debug!(peer_addr = %peer_addr, protocol = "TCP", "Connected to peer");
                 PeerStream::Tcp(stream.unwrap())
             }
         }
      } else {
         debug!(peer_addr = %peer_addr, protocol = "TCP", "Connecting to peer");
         PeerStream::Tcp(TcpStream::connect(peer_addr).await.unwrap())
      }
   }

   /// Handshakes with a peer and returns the socket address of the peer. This
   /// socket address is also a [PeerKey](super::PeerKey).
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
      &mut self, our_id: PeerId, info_hash: Arc<InfoHash>,
   ) -> Result<(PeerId, [u8; 8]), PeerTransportError> {
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
         error!(
             error = %e,
             remote_addr = %remote_addr,
             "Failed to read handshake response from peer"
         );
         PeerTransportError::ConnectionFailed(remote_addr.to_string())
      })?;

      let handshake =
         Handshake::from_bytes(&buf).map_err(|e| PeerTransportError::Other(anyhow!("{e}")))?;

      validate_handshake(&handshake, remote_addr, info_hash)?;

      info!(
          remote_addr = %remote_addr,
          peer_id = %handshake.peer_id,
          "Successfully completed handshake with peer"
      );

      Ok((handshake.peer_id, handshake.reserved))
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
      &mut self, info_hash: Arc<InfoHash>, id: PeerId,
   ) -> Result<PeerId, PeerTransportError> {
      // First 4 bytes is the big endian encoded length field and the 5th byte is a
      // PeerMessage tag
      let mut buf = vec![0; 5];

      self.read_exact(&mut buf).await.map_err(|e| {
         error!(error = %e, "Failed to read handshake headers from peer");
         PeerTransportError::InvalidPeerResponse("Failed to read handshake headers".into())
      })?;

      let addr = self.remote_addr().unwrap();

      trace!(
          message_type = buf[4],
          remote_addr = %addr,
          "Received message headers, validating handshake"
      );

      let is_handshake = is_handshake(&buf);

      let length = if is_handshake {
         // This is a handshake.
         // The length of a handshake is always 68 and we already have the
         // first 5 bytes of it, so we need 68 - 5 bytes (the current buffer length)
         68 - buf.len() as u32
      } else {
         // This is not a handshake
         // Non handshake messages have a length field from bytes 0-4
         warn!(
             remote_addr = %addr,
             message_type = buf[4],
             "Received non-handshake message when expecting handshake"
         );
         return Err(PeerTransportError::InvalidPeerResponse(
            "Expected handshake message".into(),
         ));
      };

      let mut rest = vec![0; length as usize];

      self.read_exact(&mut rest).await.map_err(|e| {
         error!(
             error = %e,
             remote_addr = %addr,
             "Failed to read handshake payload from peer"
         );
         PeerTransportError::InvalidPeerResponse("Failed to read handshake payload".into())
      })?;

      let full_length = length + buf.len() as u32;

      trace!(
          message_type = buf[4],
          total_bytes = full_length,
          remote_addr = %addr,
          "Successfully read complete handshake from peer"
      );
      buf.extend_from_slice(&rest);

      // Creates a new handshake and sends it
      let message = PeerMessages::from_bytes(buf)?;
      if let PeerMessages::Handshake(handshake) = message {
         validate_handshake(&handshake, addr, info_hash.clone())?;
         let response = PeerMessages::Handshake(Handshake::new(info_hash, id));
         self.send(response).await?;

         info!(
             remote_addr = %addr,
             peer_id = %handshake.peer_id,
             "Successfully completed incoming handshake with peer"
         );

         Ok(handshake.peer_id.clone())
      } else {
         warn!(
             remote_addr = %addr,
             "Received non-handshake message when expecting handshake"
         );
         Err(PeerTransportError::InvalidPeerResponse(
            "Expected handshake message".to_string(),
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
) -> Result<(), PeerTransportError> {
   // Validate protocol string
   if MAGIC_STRING != received_handshake.protocol {
      error!(
          peer_addr = %peer_addr,
          received_protocol = %String::from_utf8_lossy(&received_handshake.protocol),
          expected_protocol = %String::from_utf8_lossy(MAGIC_STRING),
          "Invalid protocol string received from peer"
      );
      return Err(PeerTransportError::InvalidMagicString {
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
      return Err(PeerTransportError::InvalidInfoHash {
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
      let server_id = PeerId::new();
      let client_id = PeerId::new();

      // Spawn client that sends handshake
      let client_info_hash = info_hash.clone();
      tokio::spawn(async move {
         let mut stream = PeerStream::Tcp(TcpStream::connect(addr).await.unwrap());

         let response = stream
            .send_handshake(client_id, client_info_hash)
            .await
            .unwrap();

         assert_eq!(response.0, server_id);
      });

      // Server side
      let (stream, _) = listener.accept().await.unwrap();
      let mut peer_stream = PeerStream::Tcp(stream);

      let response = peer_stream
         .receive_handshake(info_hash, server_id)
         .await
         .unwrap();

      assert_eq!(response, client_id);
   }
}
