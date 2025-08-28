use std::{net::SocketAddr, sync::Arc};

use dashmap::DashMap;
use kameo::{
   Actor,
   actor::{ActorRef, WeakActorRef},
   mailbox::Signal,
   prelude::MailboxReceiver,
};
use librqbit_utp::UtpSocketUdp;
use tokio::net::TcpListener;
use tracing::error;

use super::EngineMessage;
use crate::{
   errors::EngineError,
   hashes::InfoHash,
   peer::PeerId,
   protocol::stream::PeerStream,
   torrent::{PieceStorageStrategy, TorrentActor},
   tracker::udp::UdpServer,
};

/// The "top level" struct for torrenting. Handles all
/// [Torrent] actors. Note that the engine itself also
/// implements the [Actor] trait, and consequently behaves like an
/// actor.
pub struct EngineActor {
   /// Listener to wait for incoming TCP connections from peers
   pub(super) tcp_socket: TcpListener,
   /// Socket to wait for incoming uTP connections from peers
   pub(super) utp_socket: Arc<UtpSocketUdp>,
   /// The central UDP server that all UDP trackers use. [UdpServer] implements
   /// clone, which makes it easy to pass to multiple [Tracker
   /// Actors](crate::tracker::TrackerActor).
   pub(super) udp_server: UdpServer,
   /// A Dashmap of Torrent actors
   pub(super) torrents: Arc<DashMap<InfoHash, ActorRef<TorrentActor>>>,
   /// Our peer ID, used for the following actors "below" the engine.
   ///
   /// - [Torrent]
   /// - [PeerActor](crate::peer::PeerActor)
   /// - [TrackerActor](crate::tracker::TrackerActor)
   ///
   /// The peer id is created in the [Engine::on_start] method.
   pub peer_id: PeerId,
   /// Our actor reference. Created in [Engine::on_start]
   pub(crate) actor_ref: ActorRef<EngineActor>,

   pub(crate) default_piece_storage_strategy: PieceStorageStrategy,
}

pub(crate) type EngineActorArgs = (
   // TCP Addr
   Option<SocketAddr>,
   // uTP Addr
   Option<SocketAddr>,
   // UDP Addr
   Option<SocketAddr>,
   Option<PeerId>,
   // Strategy for storing pieces of the torrent.
   PieceStorageStrategy,
);

impl Actor for EngineActor {
   /// TCP socket address for incoming peers, uTP socket address for incoming
   /// peers, UDP socket address for UDP trackers.
   ///
   /// If an address is not provided, [on_start](Self::on_start) will use an
   /// unspecified address (`0.0.0.0`) and a dynamically assigned port (`0`).
   type Args = EngineActorArgs;
   type Error = EngineError;

   /// See Kameo documentation for docs on the
   /// [on_start](kameo::Actor::on_start) function itself.
   ///
   /// Initializes the TCP listener, uTP socket, UDP server, and peer ID.
   async fn on_start(
      args: Self::Args, actor_ref: kameo::prelude::ActorRef<Self>,
   ) -> Result<Self, Self::Error> {
      let (tcp_addr, utp_addr, udp_addr, peer_id, default_piece_storage_strategy) = args;

      let tcp_addr = tcp_addr.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
      // Should this be port 6881?
      let utp_addr = utp_addr.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
      let udp_addr = udp_addr.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
      let tcp_socket = TcpListener::bind(tcp_addr)
         .await
         .map_err(|e| EngineError::NetworkSetupFailed(format!("tcp bind {tcp_addr}: {e}")))?;
      let utp_socket = UtpSocketUdp::new_udp(utp_addr)
         .await
         .map_err(|e| EngineError::NetworkSetupFailed(format!("utp bind {utp_addr}: {e}")))?;
      let udp_server = UdpServer::new(Some(udp_addr)).await;

      let peer_id = peer_id.unwrap_or_default();

      Ok(Self {
         tcp_socket,
         utp_socket,
         udp_server,
         torrents: Arc::new(DashMap::new()),
         peer_id,
         actor_ref,
         default_piece_storage_strategy,
      })
   }

   async fn next(
      &mut self, actor_ref: WeakActorRef<Self>, mailbox_rx: &mut MailboxReceiver<Self>,
   ) -> Option<Signal<Self>> {
      tokio::select! {
         signal = mailbox_rx.recv() => signal,
         peer_stream = self.tcp_socket.accept() => match peer_stream {
            Ok((stream, _)) => {
               let peer_stream = Box::new(PeerStream::Tcp(stream));
               Some(Signal::Message {
                  message: Box::new(EngineMessage::IncomingPeer(peer_stream)),
                  actor_ref: actor_ref.upgrade().unwrap(),
                  reply: None,
                  sent_within_actor: true,
               })
            }
            Err(err) => {
               error!("Failed to accept incoming peer: {}", err);
               None
            }
         },
         peer_stream = self.utp_socket.accept() => match peer_stream {
            Ok(stream) => {
               let peer_stream = Box::new(PeerStream::Utp(stream));
               Some(Signal::Message {
                  message: Box::new(EngineMessage::IncomingPeer(peer_stream)),
                  actor_ref: actor_ref.upgrade().unwrap(),
                  reply: None,
                  sent_within_actor: true,
               })
            }
            Err(err) => {
               error!("Failed to accept incoming peer: {}", err);
               None
            }
         },
      }
   }
}
