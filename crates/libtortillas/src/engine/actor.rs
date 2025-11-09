use std::{net::SocketAddr, path::PathBuf, sync::Arc};

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

   /// Mailbox size for each torrent instance
   pub(crate) mailbox_size: usize,

   /// If we autostart torrents
   pub(crate) autostart: Option<bool>,
   /// How many peers we need to have before we start downloading
   pub(crate) sufficient_peers: Option<usize>,

   pub(crate) default_base_path: Option<PathBuf>,
}

/// Configuration arguments for creating an [`EngineActor`].
///
/// This struct provides a well-documented way to configure the engine actor
/// instead of using an unlabeled tuple. All fields are optional with sensible
/// defaults.
#[derive(Debug, Clone, Default)]
pub struct EngineActorArgs {
   /// TCP socket address for incoming peer connections.
   ///
   /// If not provided, defaults to `0.0.0.0:0` (all interfaces, dynamic port).
   pub tcp_addr: Option<SocketAddr>,

   /// uTP socket address for incoming peer connections.
   ///
   /// If not provided, defaults to `0.0.0.0:0` (all interfaces, dynamic port).
   pub utp_addr: Option<SocketAddr>,

   /// UDP socket address for tracker communication.
   ///
   /// If not provided, defaults to `0.0.0.0:0` (all interfaces, dynamic port).
   pub udp_addr: Option<SocketAddr>,

   /// Peer ID for this client instance.
   ///
   /// If not provided, a new peer ID will be generated using
   /// [`PeerId::default()`].
   pub peer_id: Option<PeerId>,

   /// Default strategy for storing torrent pieces.
   ///
   /// This determines how pieces are stored and accessed for all torrents
   /// managed by this engine.
   pub piece_storage_strategy: PieceStorageStrategy,

   /// Mailbox size for each torrent instance.
   ///
   /// Defaults to 64 if not provided. If set to 0, the mailbox will be
   /// unbounded.
   pub mailbox_size: Option<usize>,

   /// Whether to automatically start torrents when they become ready.
   ///
   /// If not provided, defaults to `None` (use engine-level default).
   pub autostart: Option<bool>,

   /// Minimum number of peers required before starting download.
   ///
   /// If not provided, defaults to `None` (use engine-level default).
   pub sufficient_peers: Option<usize>,

   /// Default base path for torrent downloads.
   ///
   /// If not provided, torrents will use their own default paths.
   pub default_base_path: Option<PathBuf>,
}

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
      let EngineActorArgs {
         tcp_addr,
         utp_addr,
         udp_addr,
         peer_id,
         piece_storage_strategy,
         mailbox_size,
         autostart,
         sufficient_peers,
         default_base_path,
      } = args;

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
         default_piece_storage_strategy: piece_storage_strategy,
         mailbox_size: mailbox_size.unwrap_or(64),
         autostart,
         sufficient_peers,
         default_base_path,
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

               let Some(actor_ref) = actor_ref.upgrade() else {
                  error!("Failed to upgrade weak actor reference");
                  return None;
               };

               Some(Signal::Message {
                  message: Box::new(EngineMessage::IncomingPeer(peer_stream)),
                  actor_ref,
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

               let Some(actor_ref) = actor_ref.upgrade() else {
                  error!("Failed to upgrade weak actor reference");
                  return None;
               };

               Some(Signal::Message {
                  message: Box::new(EngineMessage::IncomingPeer(peer_stream)),
                  actor_ref,
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
