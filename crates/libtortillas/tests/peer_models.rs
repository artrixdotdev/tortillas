use std::{
   collections::HashSet,
   net::{IpAddr, Ipv4Addr, SocketAddr},
};

use libtortillas::peer::{PeerId, WirePeer};

#[test]
fn wire_peer_contains_discovery_identity() {
   let address = SocketAddr::from((Ipv4Addr::LOCALHOST, 6881));
   let mut peer = WirePeer::from_socket_addr(address);

   assert_eq!(peer.ip, IpAddr::V4(Ipv4Addr::LOCALHOST));
   assert_eq!(peer.port, 6881);
   assert_eq!(peer.socket_addr(), address);
   assert!(peer.id.is_none());

   peer.id = Some(PeerId::Unknown([1; 20]));
   assert_eq!(peer.id, Some(PeerId::Unknown([1; 20])));
}

#[test]
fn wire_peer_identity_is_keyed_by_address() {
   let mut first = WirePeer::from_ipv4(Ipv4Addr::LOCALHOST, 6881);
   first.id = Some(PeerId::Unknown([1; 20]));
   let mut second = first.clone();
   second.id = Some(PeerId::Unknown([2; 20]));

   let peers = HashSet::from([first, second]);

   assert_eq!(peers.len(), 1);
}

#[cfg(feature = "live")]
#[test]
fn frontend_peer_view_contains_observable_state() {
   use libtortillas::{live::PeerView, metrics::PeerMetrics};

   let peer = PeerView {
      address: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 6881))),
      client: Some("Transmission".to_string()),
      connected: true,
      metrics: PeerMetrics {
         available_pieces: 4,
         ..PeerMetrics::default()
      },
   };

   assert!(peer.connected);
   assert_eq!(peer.client.as_deref(), Some("Transmission"));
   assert_eq!(peer.metrics.available_pieces, 4);
}
