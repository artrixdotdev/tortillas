use std::{net::SocketAddr, time::Duration};

/// Runtime settings for client-tunable behavior.
///
/// Wire-format constants and BEP-mandated protocol values intentionally remain
/// near the protocol code that depends on them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Settings {
   /// Engine actor and incoming socket settings.
   pub engine: EngineSettings,
   /// Per-torrent actor settings.
   pub torrent: TorrentSettings,
   /// Per-peer actor settings.
   pub peer: PeerSettings,
   /// Tracker actor and tracker protocol settings.
   pub tracker: TrackerSettings,
}

/// Restart supervision settings for actors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestartPolicySettings {
   /// Number of restarts allowed within [`Self::period`].
   pub limit: u32,
   /// Time window used by the restart limit.
   pub period: Duration,
}

impl RestartPolicySettings {
   pub fn new(limit: u32, period: Duration) -> Self {
      Self { limit, period }
   }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineSettings {
   /// Default TCP bind address for incoming peer connections.
   pub tcp_addr: SocketAddr,
   /// Default uTP bind address for incoming peer connections.
   pub utp_addr: SocketAddr,
   /// Default UDP bind address for tracker traffic.
   pub udp_addr: SocketAddr,
   /// Torrent actor mailbox size. `0` means unbounded.
   pub torrent_mailbox_size: usize,
   /// Maximum time to wait for an incoming peer handshake.
   pub incoming_peer_handshake_timeout: Duration,
   /// Restart supervision policy for torrent actors.
   pub torrent_restart: RestartPolicySettings,
}

impl Default for EngineSettings {
   fn default() -> Self {
      Self {
         tcp_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
         utp_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
         udp_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
         torrent_mailbox_size: 64,
         incoming_peer_handshake_timeout: Duration::from_secs(10),
         torrent_restart: RestartPolicySettings::new(3, Duration::from_secs(60)),
      }
   }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TorrentSettings {
   /// Start downloading automatically once the torrent is ready.
   pub autostart: bool,
   /// Minimum connected peers required before a torrent is ready to start.
   pub sufficient_peers: usize,
   /// Initial number of block requests sent to each peer when downloading
   /// starts.
   pub initial_peer_request_window: usize,
   /// Maximum in-flight block requests filled for a ready peer.
   pub max_in_flight_per_peer: usize,
   /// Peer actor mailbox size. `0` means unbounded.
   pub peer_mailbox_size: usize,
   /// Maximum concurrent sends when broadcasting a message to peers.
   pub peer_broadcast_concurrency: usize,
   /// Maximum concurrent sends when broadcasting a message to trackers.
   pub tracker_broadcast_concurrency: usize,
   /// Restart supervision policy for scheduler actors.
   pub scheduler_restart: RestartPolicySettings,
   /// Restart supervision policy for tracker actors.
   pub tracker_restart: RestartPolicySettings,
   /// Restart supervision policy for piece-store actors.
   pub piece_store_restart: RestartPolicySettings,
   /// Number of upload slots used by the choking scheduler.
   pub upload_slots: usize,
   /// Time between choking scheduler recalculations.
   pub rechoke_interval: Duration,
   /// Rechoke rounds before rotating the optimistic unchoke candidate.
   pub optimistic_unchoke_rounds: usize,
   /// Maximum concurrent peer stats requests during rechoke.
   pub peer_stats_concurrency: usize,
   /// Maximum time to wait for a peer stats response.
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
         scheduler_restart: RestartPolicySettings::new(3, Duration::from_secs(10)),
         tracker_restart: RestartPolicySettings::new(3, Duration::from_secs(60)),
         piece_store_restart: RestartPolicySettings::new(3, Duration::from_secs(10)),
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
   /// Initial capacity for queued non-block peer messages.
   pub pending_message_capacity: usize,
   /// Idle time since the last received message before sending a keepalive.
   pub keepalive_timeout: Duration,
   /// Idle time since the last received message before disconnecting a peer.
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
   /// Peer address sent to trackers when no explicit address is available.
   pub default_peer_addr: SocketAddr,
   /// Delay before the tracker actor's first announce attempt.
   pub initial_announce_delay: Duration,
   /// Lower bound for scheduled tracker announce delays.
   pub minimum_announce_interval: Duration,
   /// Upper bound for scheduled tracker announce delays.
   pub maximum_announce_interval: Duration,
   /// Delay used when a tracker has not supplied a usable announce interval.
   pub fallback_announce_interval: Duration,
   /// Maximum time the tracker actor waits for tracker shutdown.
   pub stop_timeout: Duration,
   /// Maximum time an HTTP tracker waits for its final stopped announce.
   pub http_stop_timeout: Duration,
   /// Whether HTTP trackers request compact peer responses.
   pub http_compact_response: bool,
   /// Maximum time allowed for a UDP tracker connect exchange.
   pub udp_connect_timeout: Duration,
   /// Maximum time allowed for one UDP tracker response.
   pub udp_message_timeout: Duration,
   /// Receive buffer size for the shared UDP tracker socket, in bytes.
   pub udp_receive_buffer_size: usize,
   /// Initial delay for UDP tracker exponential backoff.
   pub udp_retry_initial_delay: Duration,
   /// Multiplication factor for UDP tracker exponential backoff.
   pub udp_retry_factor: u64,
   /// Maximum delay for UDP tracker exponential backoff.
   pub udp_retry_max_delay: Duration,
   /// Number of UDP tracker retry attempts per request.
   pub udp_retry_attempts: usize,
   /// Attempts to reconnect and retry when a UDP tracker rejects a connection
   /// ID.
   pub udp_connection_id_retry_attempts: usize,
   /// Raw IPv4 address field for UDP announces. `0` lets the tracker infer it.
   pub udp_announce_ip_address: u32,
   /// Raw UDP announce key field. `0` preserves the current generated-none
   /// behavior.
   pub udp_announce_key: u32,
   /// UDP announce `num_want` field. `-1` asks the tracker to use its default.
   pub udp_num_want: i32,
}

impl Default for TrackerSettings {
   fn default() -> Self {
      Self {
         default_peer_addr: SocketAddr::from(([0, 0, 0, 0], 6881)),
         initial_announce_delay: Duration::ZERO,
         minimum_announce_interval: Duration::from_secs(1),
         maximum_announce_interval: Duration::from_secs(3600),
         fallback_announce_interval: Duration::from_secs(60),
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
