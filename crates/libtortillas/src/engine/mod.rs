use std::{net::SocketAddr, str::FromStr, sync::Arc};

use dashmap::DashMap;
use kameo::{Actor, actor::ActorRef};
use librqbit_utp::UtpSocketUdp;
use tokio::net::TcpListener;

use crate::{errors::EngineError, hashes::InfoHash, peer::PeerId, torrent::Torrent};

pub struct Engine {
   tcp_socket: TcpListener,
   utp_socket: Arc<UtpSocketUdp>,
   torrents: Arc<DashMap<InfoHash, ActorRef<Torrent>>>,
   peer_id: PeerId,
}

impl Actor for Engine {
   /// TCP socket address, uTP socket address
   type Args = (Option<SocketAddr>, Option<SocketAddr>);
   type Error = EngineError;

   async fn on_start(
      args: Self::Args, actor_ref: kameo::prelude::ActorRef<Self>,
   ) -> Result<Self, Self::Error> {
      let (tcp_addr, utp_addr) = args;
      let tcp_addr = tcp_addr.unwrap_or(SocketAddr::from_str("0.0.0.0:0").unwrap());

      // Should this be port 6881?
      let utp_addr = utp_addr.unwrap_or(SocketAddr::from_str("0.0.0.0:0").unwrap());

      let tcp_socket = TcpListener::bind(tcp_addr).await.unwrap();
      let utp_socket = UtpSocketUdp::new_udp(utp_addr).await.unwrap();

      let peer_id = PeerId::new();
      Ok(Self {
         tcp_socket,
         utp_socket,
         torrents: Arc::new(DashMap::new()),
         peer_id,
      })
   }
}
