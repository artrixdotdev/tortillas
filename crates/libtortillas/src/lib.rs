//! Async BitTorrent engine for building Tortillas frontends.
//!
//! # Runtime boundary
//!
//! `libtortillas` is intentionally a Tokio-based library. Public handles such
//! as [`engine::Engine`] and [`torrent::Torrent`] expose async methods that
//! must be driven inside a Tokio runtime, and the crate uses Tokio tasks,
//! sockets, timers, channels, and filesystem APIs internally.
//!
//! Frontends should create one application-level Tokio runtime and keep the
//! engine plus all torrent handles on work scheduled by that runtime. The crate
//! does not promise runtime independence, HTTP client injection, clock
//! injection, listener injection, or storage runtime abstraction.
//!
//! A TUI can use `#[tokio::main]` on its binary entry point, or create an
//! explicit Tokio runtime before initializing `Engine`.
//!
//! # Frontend facade
//!
//! Frontends should prefer [`facade`] or [`prelude`] imports. The facade names
//! the stable concepts a TUI or other UI needs: [`facade::EngineHandle`],
//! [`facade::TorrentHandle`], [`facade::TorrentSource`],
//! [`facade::CoreCommand`], [`facade::CoreEvent`], and snapshot types for
//! engine, torrent, peer, and tracker views.
//!
//! ```no_run
//! use libtortillas::prelude::{CoreCommand, EngineHandle, TorrentSource};
//!
//! let _engine = EngineHandle::default();
//! let _command = CoreCommand::AddTorrent {
//!    source: TorrentSource::MagnetUri("magnet:?xt=urn:btih:...".to_string()),
//! };
//! ```
//!
//! # Advanced APIs
//!
//! The lower-level [`engine`], [`torrent`], [`metainfo`], [`peer`],
//! [`tracker`], [`pieces`], and [`protocol`] modules remain public for advanced
//! integrations, tests, and protocol-level work. Frontend code should avoid
//! depending on actor messages, raw peer streams, tracker clients, or storage
//! internals when an equivalent facade type exists.
//!
//! Follow-up work will narrow the prelude and connect more commands, events,
//! snapshots, and typed errors to the facade without requiring frontend callers
//! to import implementation modules.

pub mod engine;
pub mod errors;
pub mod facade;
pub mod hashes;
pub mod metainfo;
pub mod peer;
pub mod pieces;
pub mod protocol;
pub mod settings;
pub mod torrent;
pub mod tracker;

#[cfg(test)]
pub(crate) mod testing {
   use std::{env, net::SocketAddr, path::PathBuf, process, str::FromStr};

   use rand::random_range;
   use tokio::fs::read_to_string;

   use crate::{
      hashes::Hash,
      metainfo::{MagnetUri, MetaInfo, TorrentFile},
      peer::PeerId,
      tracker::udp::UdpServer,
   };

   pub(crate) const BIG_BUCK_BUNNY_MAGNET: &str =
      include_str!("../tests/magneturis/big-buck-bunny.txt");
   pub(crate) const BIG_BUCK_BUNNY_MAGNET_FILE: &str = "big-buck-bunny.txt";
   pub(crate) const BIG_BUCK_BUNNY_NAME: &str = "Big Buck Bunny";
   pub(crate) const BIG_BUCK_BUNNY_INFO_HASH: &str = "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c";
   pub(crate) const BIG_BUCK_BUNNY_TORRENT_FILE: &str = "big-buck-bunny.torrent";
   pub(crate) const KNOPPIX_TORRENT_FILE: &str = "KNOPPIX_V9.1DVD-2021-01-25-EN.torrent";

   pub(crate) fn fixture_path(relative_path: &str) -> PathBuf {
      PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path)
   }

   pub(crate) fn torrent_fixture_path(file_name: &str) -> PathBuf {
      fixture_path(&format!("tests/torrents/{file_name}"))
   }

   pub(crate) fn magnet_fixture_path(file_name: &str) -> PathBuf {
      fixture_path(&format!("tests/magneturis/{file_name}"))
   }

   pub(crate) async fn read_torrent_fixture(file_name: &str) -> MetaInfo {
      TorrentFile::read(torrent_fixture_path(file_name))
         .await
         .unwrap()
   }

   pub(crate) async fn read_magnet_fixture(file_name: &str) -> MetaInfo {
      let contents = read_to_string(magnet_fixture_path(file_name))
         .await
         .unwrap();
      MagnetUri::parse(contents).unwrap()
   }

   pub(crate) fn big_buck_bunny_magnet() -> MetaInfo {
      MagnetUri::parse(BIG_BUCK_BUNNY_MAGNET.to_string()).unwrap()
   }

   pub(crate) fn random_port() -> u16 {
      random_range(1024..65535)
   }

   pub(crate) fn random_socket_addr() -> SocketAddr {
      SocketAddr::from_str(&format!("0.0.0.0:{}", random_port())).unwrap()
   }

   pub(crate) fn ephemeral_socket_addr() -> SocketAddr {
      SocketAddr::from_str("0.0.0.0:0").unwrap()
   }

   pub(crate) fn torrent_temp_path() -> PathBuf {
      env::temp_dir().join(format!(
         "tortillas-{}-{}",
         process::id(),
         rand::random::<u64>()
      ))
   }

   pub(crate) fn peer_id() -> PeerId {
      PeerId::default()
   }

   pub(crate) fn piece_hash(data: &[u8]) -> Hash<20> {
      use sha1::{Digest, Sha1};

      let mut hasher = Sha1::new();
      hasher.update(data);
      Hash::from_bytes(hasher.finalize().into())
   }

   pub(crate) async fn udp_server() -> UdpServer {
      UdpServer::new(None).await.unwrap()
   }

   pub(crate) fn init_tracing() {
      let _ = tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .try_init();
   }
}

/// The prelude for this crate.
///
/// This module re-exports the most commonly used types, traits, and functions
/// so that you can conveniently import them all at once:
///
/// ```
/// use libtortillas::prelude::*;
/// ```
pub mod prelude {
   pub use crate::{
      engine::*,
      errors::*,
      facade::*,
      hashes::InfoHash,
      metainfo::*,
      peer::{Peer, PeerId},
      settings::*,
      torrent::*,
      tracker::Tracker,
   };
}
