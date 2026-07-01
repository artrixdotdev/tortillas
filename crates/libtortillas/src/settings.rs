use std::{net::SocketAddr, time::Duration};

/// Runtime settings for client-tunable behavior.
///
/// Wire-format constants and BEP-mandated protocol values intentionally remain
/// near the protocol code that depends on them.
#[derive(Clone, Debug, Eq, PartialEq)]
#[derive(Default)]
pub struct Settings {
   pub engine: EngineSettings,
   pub torrent: TorrentSettings,
   pub peer: PeerSettings,
   pub tracker: TrackerSettings,
}


#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineSettings {
   pub tcp_addr: SocketAddr,
   pub utp_addr: SocketAddr,
   pub udp_addr: SocketAddr,
   pub torrent_mailbox_size: usize,
   pub incoming_peer_handshake_timeout: Duration,
   pub torrent_restart_limit: u32,
   pub torrent_restart_period: Duration,
}

impl Default for EngineSettings {
   fn default() -> Self {
      Self {
         tcp_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
         utp_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
         udp_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
         torrent_mailbox_size: 64,
         incoming_peer_handshake_timeout: Duration::from_secs(10),
         torrent_restart_limit: 3,
         torrent_restart_period: Duration::from_secs(60),
      }
   }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TorrentSettings {
   pub autostart: bool,
   pub sufficient_peers: usize,
   pub initial_peer_request_window: usize,
   pub max_in_flight_per_peer: usize,
   pub peer_mailbox_size: usize,
   pub peer_broadcast_concurrency: usize,
   pub tracker_broadcast_concurrency: usize,
   pub scheduler_restart_limit: u32,
   pub scheduler_restart_period: Duration,
   pub tracker_restart_limit: u32,
   pub tracker_restart_period: Duration,
   pub piece_store_restart_limit: u32,
   pub piece_store_restart_period: Duration,
   pub upload_slots: usize,
   pub rechoke_interval: Duration,
   pub optimistic_unchoke_rounds: usize,
   pub peer_stats_concurrency: usize,
   pub peer_stats_timeout: Duration,
}

impl Default for TorrentSettings {
   fn default() -> Self {
      Self {
         autostart: true,
         sufficient_peers: 6,
         initial_peer_request_window: 32,
         max_in_flight_per_peer: 32,
         peer_mailbox_size: 120,
         peer_broadcast_concurrency: 32,
         tracker_broadcast_concurrency: 8,
         scheduler_restart_limit: 3,
         scheduler_restart_period: Duration::from_secs(10),
         tracker_restart_limit: 3,
         tracker_restart_period: Duration::from_secs(60),
         piece_store_restart_limit: 3,
         piece_store_restart_period: Duration::from_secs(10),
         upload_slots: 4,
         rechoke_interval: Duration::from_secs(10),
         optimistic_unchoke_rounds: 3,
         peer_stats_concurrency: 32,
         peer_stats_timeout: Duration::from_millis(250),
      }
   }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerSettings {
   pub pending_message_capacity: usize,
   pub keepalive_timeout: Duration,
   pub disconnect_timeout: Duration,
}

impl Default for PeerSettings {
   fn default() -> Self {
      Self {
         pending_message_capacity: 8,
         keepalive_timeout: Duration::from_secs(120),
         disconnect_timeout: Duration::from_secs(240),
      }
   }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackerSettings {
   pub default_peer_addr: SocketAddr,
   pub initial_announce_delay: Duration,
   pub minimum_announce_interval: Duration,
   pub stop_timeout: Duration,
   pub http_stop_timeout: Duration,
   pub http_compact_response: bool,
   pub udp_connect_timeout: Duration,
   pub udp_message_timeout: Duration,
   pub udp_receive_buffer_size: usize,
   pub udp_retry_initial_delay: Duration,
   pub udp_retry_factor: u64,
   pub udp_retry_max_delay: Duration,
   pub udp_retry_attempts: usize,
   pub udp_connection_id_retry_attempts: usize,
   pub udp_announce_ip_address: u32,
   pub udp_announce_key: u32,
   pub udp_num_want: i32,
}

impl Default for TrackerSettings {
   fn default() -> Self {
      Self {
         default_peer_addr: SocketAddr::from(([0, 0, 0, 0], 6881)),
         initial_announce_delay: Duration::ZERO,
         minimum_announce_interval: Duration::from_secs(1),
         stop_timeout: Duration::from_secs(5),
         http_stop_timeout: Duration::from_secs(3),
         http_compact_response: false,
         udp_connect_timeout: Duration::from_secs(3),
         udp_message_timeout: Duration::from_millis(300),
         udp_receive_buffer_size: 8192,
         udp_retry_initial_delay: Duration::from_secs(15),
         udp_retry_factor: 2,
         udp_retry_max_delay: Duration::from_secs(3840),
         udp_retry_attempts: 8,
         udp_connection_id_retry_attempts: 2,
         udp_announce_ip_address: 0,
         udp_announce_key: 0,
         udp_num_want: -1,
      }
   }
}
