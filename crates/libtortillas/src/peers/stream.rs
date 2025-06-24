use anyhow::Result;
use std::{
   net::SocketAddr,
   pin::Pin,
   str::FromStr,
   task::{Context, Poll},
};

use librqbit_utp::{UtpSocket, UtpStream};
use tokio::{
   io::{self, AsyncRead, AsyncWrite, ReadBuf},
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

impl PeerStream {
   /// Connect to a peer with the given peer_addr (ip & port in the form of a
   /// [SocketAddr](std::net::SocketAddr))
   ///
   /// When connecting to a peer, we attempt to connect over both TCP and uTP, and use whichever
   /// one works. While this may seem "not to spec", this is how the transmission BitTorrent
   /// client does it:
   /// https://github.com/transmission/transmission/discussions/7603
   pub async fn connect(peer_addr: SocketAddr) -> Self {
      // Prework for uTP stream
      //
      // NOTE: This may need to be refactored according to BEP 0003:
      //
      // > The port number this peer is listening on. Common behavior is for a downloader to
      // try to listen on port 6881 and if that port is taken try 6882, then 6883, etc. and
      // give up after 6889.
      let socket_addr = SocketAddr::from_str("0.0.0.0:6881").unwrap();

      trace!(
         "Creating UTP socket for (potential) peer {} at {}",
         peer_addr, socket_addr
      );

      let utp_socket = UtpSocket::new_udp(socket_addr).await.unwrap();

      trace!("Attemping connection to {}", peer_addr);
      tokio::select! {
         stream = utp_socket.connect(peer_addr) => {PeerStream::Utp(stream.unwrap())},
         stream = TcpStream::connect(peer_addr) => {PeerStream::Tcp(stream.unwrap())}
      }
   }

   /// Returns the addr of the connected peer
   pub fn remote_addr(&self) -> Result<SocketAddr> {
      match self {
         PeerStream::Tcp(s) => Ok(s.peer_addr().unwrap()),
         PeerStream::Utp(s) => Ok(s.remote_addr()),
      }
   }
}

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
