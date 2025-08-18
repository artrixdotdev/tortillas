use std::{net::SocketAddr, sync::Arc};

use dashmap::DashMap;
use kameo::{
   Actor, Reply,
   actor::{ActorRef, WeakActorRef},
   mailbox::Signal,
   prelude::{Context, MailboxReceiver, Message},
};
use librqbit_utp::UtpSocketUdp;
use tokio::net::TcpListener;
use tracing::error;

use crate::{
   actor_request_response,
   errors::EngineError,
   hashes::InfoHash,
   metainfo::MetaInfo,
   peer::{Peer, PeerId},
   protocol::{
      messages::PeerMessages,
      stream::{PeerRecv, PeerStream},
   },
   torrent::{Torrent, TorrentMessage},
   tracker::udp::UdpServer,
};

pub(crate) enum EngineMessage {
   #[allow(dead_code)]
   /// Creates a new [Torrent](crate::torrent::Torrent) actor.
   Torrent(Box<MetaInfo>),
   /// Handles an incoming peer connection. The peer has been neither handshaked
   /// nor verified at this point.
   IncomingPeer(Box<PeerStream>),
}

actor_request_response!(
   #[allow(dead_code)]
   pub(crate) EngineRequest,
   pub(crate) EngineResponse #[derive(Reply)],
);

/// The "top level" struct for torrenting. Handles all
/// [Torrent](crate::torrent::Torrent) actors. Note that the engine itself also
/// implements the [Actor](kameo::Actor) trait, and consequently behaves like an
/// actor.
pub struct Engine {
   /// Listener to wait for incoming TCP connections from peers
   tcp_socket: TcpListener,
   /// Socket to wait for incoming uTP connections from peers
   utp_socket: Arc<UtpSocketUdp>,
   /// The central UDP server that all UDP trackers use. [UdpServer] implements
   /// clone, which makes it easy to pass to multiple [Tracker
   /// Actors](crate::tracker::TrackerActor).
   udp_server: UdpServer,
   /// A Dashmap of Torrent actors
   torrents: Arc<DashMap<InfoHash, ActorRef<Torrent>>>,
   /// Our peer ID, used for the following actors "below" the engine.
   ///
   /// - [Torrent](crate::torrent::Torrent)
   /// - [PeerActor](crate::peer::PeerActor)
   /// - [TrackerActor](crate::tracker::TrackerActor)
   ///
   /// The peer id is created in the [Engine::on_start] method.
   peer_id: PeerId,
   /// Our actor reference. Created in [Engine::on_start]
   actor_ref: ActorRef<Engine>,
}

impl Actor for Engine {
   /// TCP socket address for incoming peers, uTP socket address for incoming
   /// peers, UDP socket address for UDP trackers.
   ///
   /// If an address is not provided, [on_start](Self::on_start) will use an
   /// unspecified address (`0.0.0.0`) and a dynamically assigned port (`0`).
   type Args = (Option<SocketAddr>, Option<SocketAddr>, Option<SocketAddr>);
   type Error = EngineError;

   /// See Kameo documentation for docs on the
   /// [on_start](kameo::Actor::on_start) function itself.
   ///
   /// Initializes the TCP listener, uTP socket, UDP server, and peer ID.
   async fn on_start(
      args: Self::Args, actor_ref: kameo::prelude::ActorRef<Self>,
   ) -> Result<Self, Self::Error> {
      let (tcp_addr, utp_addr, udp_addr) = args;

      let tcp_addr = tcp_addr.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
      // Should this be port 6881?
      let utp_addr = utp_addr.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
      let udp_addr = udp_addr.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
      let tcp_socket = TcpListener::bind(tcp_addr)
         .await
         .map_err(|e| EngineError::NetworkSetupFailed(format!("tcp bind {}: {}", tcp_addr, e)))?;
      let utp_socket = UtpSocketUdp::new_udp(utp_addr)
         .await
         .map_err(|e| EngineError::NetworkSetupFailed(format!("utp bind {}: {}", utp_addr, e)))?;
      let udp_server = UdpServer::new(Some(udp_addr)).await;

      let peer_id = PeerId::new();
      Ok(Self {
         tcp_socket,
         utp_socket,
         udp_server,
         torrents: Arc::new(DashMap::new()),
         peer_id,
         actor_ref,
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

impl Message<EngineMessage> for Engine {
   type Reply = ();
   async fn handle(
      &mut self, msg: EngineMessage, _: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match msg {
         EngineMessage::IncomingPeer(mut stream) => {
            let msg = stream.recv().await.expect("Can't read from peer stream");
            let peer_addr = stream.remote_addr().expect("Can't get remote address");

            if let PeerMessages::Handshake(handshake) = msg {
               let info_hash = *handshake.info_hash;
               let peer = Peer::from_socket_addr(peer_addr);

               if self.torrents.contains_key(&info_hash) {
                  let torrent = self.torrents.get(&info_hash).unwrap();
                  torrent
                     .tell(TorrentMessage::IncomingPeer(peer, stream))
                     .await
                     .expect("Failed to tell torrent about incoming peer");
               } else {
                  error!(%stream, "Received incoming peer for unknown torrent, killing connection");
                  // Drop *should* kill the peer, if not we need to implement a kill method on the
                  // stream itself
                  drop(stream);
               }
            } else {
               error!("Received unexpected message from peer");
            }
         }
         EngineMessage::Torrent(metainfo) => {
            let info_hash = metainfo
               .info_hash()
               .map_err(|e| {
                  error!(error = %e, "Failed to unwrap info hash");
               })
               .expect("Failed to unwrap info hash");

            if self.torrents.contains_key(&info_hash) {
               error!(
                  ?info_hash,
                  "Torrent already exists; ignoring duplicate EngineMessage::Torrent"
               );
               return;
            }

            let torrent_ref = Torrent::spawn((
               self.peer_id,
               *metainfo,
               self.utp_socket.clone(),
               self.udp_server.clone(),
               None,
            ));

            self.actor_ref.link(&torrent_ref).await;

            self.torrents.insert(info_hash, torrent_ref);
         }
      };
   }
}
