use std::{net::SocketAddr, ops::ControlFlow, path::PathBuf, sync::Arc};

use dashmap::DashMap;
use kameo::{
   Actor,
   actor::{ActorId, ActorRef, Spawn, WeakActorRef},
   error::ActorStopReason,
   mailbox::Signal,
   prelude::MailboxReceiver,
   supervision::{RestartPolicy, SupervisionStrategy},
};
use librqbit_utp::UtpSocketUdp;
use tokio::net::TcpListener;
use tracing::{Span, error, instrument};

use super::commands;
use crate::{
   dht::{DhtActor, DhtActorArgs},
   errors::EngineError,
   frontend::FrontendPublisher,
   hashes::InfoHash,
   peer::PeerId,
   protocol::stream::PeerStream,
   settings::Settings,
   torrent::{PieceStorageStrategy, TorrentActor},
   tracker::udp::UdpServer,
};

/// The "top level" struct for torrenting. Handles all
/// [`Torrent`](crate::torrent::Torrent) actors. Note that the engine itself
/// also implements the [Actor] trait, and consequently behaves like an
/// actor.
pub struct EngineActor {
   /// Live frontend event and view publisher shared with managed torrents.
   pub(super) frontend: FrontendPublisher,
   /// Engine-wide DHT service shared by every torrent.
   pub(super) dht: Option<ActorRef<DhtActor>>,
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
   /// - [`Torrent`](crate::torrent::Torrent)
   /// - [PeerActor](crate::peer::PeerActor)
   /// - [TrackerActor](crate::tracker::TrackerActor)
   ///
   /// The peer id is created in the engine actor's `on_start` method.
   pub peer_id: PeerId,
   /// Our actor reference. Created in the engine actor's `on_start` method.
   pub(crate) actor_ref: ActorRef<EngineActor>,

   pub(crate) default_piece_storage_strategy: PieceStorageStrategy,

   /// Runtime behavior settings shared with torrent, peer, and tracker actors.
   pub(crate) settings: Settings,

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

   /// Runtime behavior settings.
   pub settings: Settings,

   /// Default base path for torrent downloads.
   ///
   /// If not provided, torrents will use their own default paths.
   pub default_base_path: Option<PathBuf>,

   /// Live frontend state shared by the engine handle and actor hierarchy.
   pub(crate) frontend: FrontendPublisher,
}

impl Actor for EngineActor {
   /// TCP socket address for incoming peers, uTP socket address for incoming
   /// peers, UDP socket address for UDP trackers.
   ///
   /// If an address is not provided, [on_start](Self::on_start) will use an
   /// unspecified address (`0.0.0.0`) and a dynamically assigned port (`0`).
   type Args = EngineActorArgs;
   type Error = EngineError;

   fn supervision_strategy() -> SupervisionStrategy {
      SupervisionStrategy::OneForOne
   }

   /// See Kameo documentation for docs on the
   /// [on_start](kameo::Actor::on_start) function itself.
   ///
   /// Initializes the TCP listener, uTP socket, UDP server, and peer ID.
   async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
      let EngineActorArgs {
         tcp_addr,
         utp_addr,
         udp_addr,
         peer_id,
         piece_storage_strategy,
         settings,
         default_base_path,
         frontend,
      } = args;

      let tcp_addr = tcp_addr.unwrap_or(settings.engine.tcp_addr);
      let utp_addr = utp_addr.unwrap_or(settings.engine.utp_addr);
      let udp_addr = udp_addr.unwrap_or(settings.engine.udp_addr);
      let tcp_socket = TcpListener::bind(tcp_addr)
         .await
         .map_err(|e| EngineError::NetworkSetupFailed(format!("tcp bind {tcp_addr}: {e}")))?;
      let utp_socket = UtpSocketUdp::new_udp(utp_addr)
         .await
         .map_err(|e| EngineError::NetworkSetupFailed(format!("utp bind {utp_addr}: {e}")))?;
      let udp_server = UdpServer::new_with_receive_buffer_size(
         Some(udp_addr),
         settings.tracker.udp_receive_buffer_size,
      )
      .await
      .map_err(|e| EngineError::NetworkSetupFailed(format!("udp bind {udp_addr}: {e}")))?;

      let peer_id = peer_id.unwrap_or_default();
      let dht = if settings.dht.enabled {
         Some(
            DhtActor::supervise(
               &actor_ref,
               DhtActorArgs {
                  id: None,
                  settings: settings.dht.clone(),
               },
            )
            .restart_policy(RestartPolicy::Transient)
            .restart_limit(settings.dht.restart.limit, settings.dht.restart.period)
            .spawn()
            .await,
         )
      } else {
         None
      };

      frontend.engine_started();

      Ok(Self {
         frontend,
         dht,
         tcp_socket,
         utp_socket,
         udp_server,
         torrents: Arc::new(DashMap::new()),
         peer_id,
         actor_ref,
         default_piece_storage_strategy: piece_storage_strategy,
         settings,
         default_base_path,
      })
   }

   #[instrument(skip(self), fields(engine_id = ?self.peer_id))]
   async fn on_link_died(
      &mut self, _: WeakActorRef<Self>, id: ActorId, reason: ActorStopReason,
   ) -> Result<ControlFlow<ActorStopReason>, Self::Error> {
      error!(?id, ?reason, "Linked child died");

      Ok(ControlFlow::Continue(()))
   }

   async fn next(
      &mut self, actor_ref: WeakActorRef<Self>, mailbox_rx: &mut MailboxReceiver<Self>,
   ) -> Result<Option<Signal<Self>>, Self::Error> {
      Ok(tokio::select! {
         signal = mailbox_rx.recv() => signal,
         peer_stream = self.tcp_socket.accept() => match peer_stream {
            Ok((stream, _)) => {
               let peer_stream = PeerStream::tcp(stream);

               let Some(actor_ref) = actor_ref.upgrade() else {
                  error!("Failed to upgrade weak actor reference");
                  return Ok(None);
               };

               Some(Signal::Message {
                  message: Box::new(commands::IncomingPeer { stream: peer_stream }),
                  actor_ref,
                   reply: None,
                   sent_within_actor: true,
                   message_name: "IncomingPeer",
                   caller_span: Span::current(),
                })
            }
            Err(err) => {
               error!("Failed to accept incoming peer: {}", err);
               None
            }
         },
         peer_stream = self.utp_socket.accept() => match peer_stream {
            Ok(stream) => {
               let peer_stream = PeerStream::utp(stream);

               let Some(actor_ref) = actor_ref.upgrade() else {
                  error!("Failed to upgrade weak actor reference");
                  return Ok(None);
               };

               Some(Signal::Message {
                  message: Box::new(commands::IncomingPeer { stream: peer_stream }),
                  actor_ref,
                   reply: None,
                   sent_within_actor: true,
                   message_name: "IncomingPeer",
                   caller_span: Span::current(),
                })
            }
            Err(err) => {
               error!("Failed to accept incoming peer: {}", err);
               None
            }
         },
      })
   }

   async fn on_stop(
      &mut self, _: WeakActorRef<Self>, _: ActorStopReason,
   ) -> Result<(), Self::Error> {
      self.frontend.engine_stopping();
      let torrents = self
         .torrents
         .iter()
         .map(|torrent| (*torrent.key(), torrent.value().clone()))
         .collect::<Vec<_>>();

      for (info_hash, torrent) in torrents {
         if let Err(err) = torrent.stop_gracefully().await {
            error!(error = %err, %info_hash, "Failed to stop torrent during engine shutdown");
         }
         torrent.wait_for_shutdown().await;
         self.torrents.remove(&info_hash);
      }

      if let Some(dht) = self.dht.take() {
         dht.kill();
         dht.wait_for_shutdown().await;
      }

      self.frontend.engine_stopped();

      Ok(())
   }
}
