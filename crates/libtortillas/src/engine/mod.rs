use std::{net::SocketAddr, str::FromStr, sync::Arc};

use dashmap::DashMap;
use kameo::{Actor, Reply, actor::ActorRef};
use librqbit_utp::UtpSocketUdp;
use tokio::net::TcpListener;

use crate::{
   actor_request_response, errors::EngineError, hashes::InfoHash, metainfo::MetaInfo, peer::PeerId,
   torrent::Torrent, tracker::udp::UdpServer,
};

pub(crate) enum EngineMessage {
   Torrent(MetaInfo),
}

actor_request_response!(
   pub(crate) EngineRequest,
   pub(crate) EngineResponse #[derive(Reply)],
);

pub struct Engine {
   tcp_socket: TcpListener,
   utp_socket: Arc<UtpSocketUdp>,
   udp_server: UdpServer,
   torrents: Arc<DashMap<InfoHash, ActorRef<Torrent>>>,
   peer_id: PeerId,
}

impl Actor for Engine {
   /// TCP socket address for incoming peers, uTP socket address for incoming
   /// peers, UDP socket address for UDP trackers
   type Args = (Option<SocketAddr>, Option<SocketAddr>, Option<SocketAddr>);
   type Error = EngineError;

   async fn on_start(
      args: Self::Args, actor_ref: kameo::prelude::ActorRef<Self>,
   ) -> Result<Self, Self::Error> {
      let (tcp_addr, utp_addr, udp_addr) = args;
      let tcp_addr = tcp_addr.unwrap_or(SocketAddr::from_str("0.0.0.0:0").unwrap());
      // Should this be port 6881?
      let utp_addr = utp_addr.unwrap_or(SocketAddr::from_str("0.0.0.0:0").unwrap());
      let udp_addr = udp_addr.unwrap_or(SocketAddr::from_str("0.0.0.0:0").unwrap());

      let tcp_socket = TcpListener::bind(tcp_addr).await.unwrap();
      let utp_socket = UtpSocketUdp::new_udp(utp_addr).await.unwrap();
      let udp_server = UdpServer::new(Some(udp_addr)).await;

      let peer_id = PeerId::new();
      Ok(Self {
         tcp_socket,
         utp_socket,
         udp_server,
         torrents: Arc::new(DashMap::new()),
         peer_id,
      })
   }
}
