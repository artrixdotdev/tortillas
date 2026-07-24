//! # Engine
//!
//! The [`Engine`] is the central controller for multiple torrenting operations.
//! It manages communication with trackers, incoming peers, and spawns
//! individual [`Torrent`] actors to handle torrent
//! sessions.
//!
//! ## Overview
//!
//! - The `Engine` is backed by an `EngineActor` that manages torrent state
//!   and peer connections (based on [kameo actors](https://github.com/tqwewe/kameo)).
//! - It provides a high-level API for adding torrents from different sources
//!   (remote `.torrent` files, local files, or magnet URIs).
//! - Each torrent is represented by a [`Torrent`] handle, which can be used to
//!   interact with the torrent session.
//!
//! ## Runtime
//!
//! The engine is Tokio-only. Construct and use [`Engine`] from tasks running on
//! a Tokio runtime, such as a TUI binary with `#[tokio::main]`. `Engine` starts
//! actor tasks, binds Tokio TCP and uTP sockets, uses Tokio timers, and
//! performs async filesystem and HTTP work through the same runtime.
//!
//! ## Example
//!
//! ```no_run
//! use libtortillas::prelude::*;
//!
//! #[tokio::main]
//! async fn main() {
//!    // Create a new engine listening on default addresses
//!    let engine = Engine::default();
//!
//!    // Add a torrent from a magnet URI
//!    let torrent = engine
//!       .add_torrent(TorrentSource::magnet("magnet:?xt=urn:btih:..."))
//!       .await
//!       .expect("Failed to add torrent");
//!
//!    println!("Started torrenting: {}", torrent.key());
//! }
//! ```

mod actor;
mod messages;
mod snapshot;
mod source;

use std::{net::SocketAddr, path::PathBuf};

pub(crate) use actor::*;
use bon;
use kameo::{
   actor::{ActorRef, Spawn},
   error::SendError,
};
pub(crate) use messages::*;
pub use source::TorrentSource;

use self::commands::{CreateTorrent, GetTorrent, RemoveTorrent, SnapshotEngine, StartAll};
pub use self::snapshot::{ENGINE_SNAPSHOT_VERSION, EngineSnapshot, EngineStatus};
use crate::{
   errors::EngineError,
   frontend::{
      CoreCommand, CoreCommandResult, EngineListener, EngineView, EventSubscription,
      FrontendPublisher, TorrentCommand,
   },
   hashes::InfoHash,
   peer::PeerId,
   settings::Settings,
   torrent::{PieceStorageStrategy, Torrent},
};

/// The main entry point for managing torrents.
///
/// The [`Engine`] is responsible for:
/// - Spawning and supervising the lower level `EngineActor`
/// - Adding new [torrents](Torrent) from different sources
/// - Managing peer connections and tracker communication
///
/// `Engine` must be created and used from a Tokio runtime. Applications should
/// create one runtime at the frontend boundary and run all engine and torrent
/// operations on that runtime.
///
/// Typically, you create a single `Engine` instance per application and attach
/// multiple [`Torrent`] instances to it.
///
/// # Example
/// ```no_run
/// use std::net::SocketAddr;
///
/// use libtortillas::prelude::*;
///
/// #[tokio::main]
/// async fn main() {
///    // Create an engine with no explicit addresses
///    let engine = Engine::builder()
///       // Optionally provide addresses for our sockets to listen on
///       .tcp_addr("127.0.0.1:6881".parse::<SocketAddr>().unwrap())
///       .utp_addr("127.0.0.1:6882".parse::<SocketAddr>().unwrap())
///       .udp_addr("127.0.0.1:6883".parse::<SocketAddr>().unwrap())
///       .build();
/// }
/// ```
/// Or with all default settings
/// ```no_run
/// use libtortillas::prelude::*;
///
/// #[tokio::main]
/// async fn main() {
///    let engine = Engine::default();
/// }
/// ```
#[derive(Debug, Clone)]
pub struct Engine {
   actor: ActorRef<EngineActor>,
   frontend: FrontendPublisher,
}

#[bon::bon]
impl Engine {
   /// Creates a new [`Engine`] instance.
   ///
   /// Use [`Engine::default`] for the default settings.
   #[builder(on(SocketAddr, into))]
   pub fn new(
      /// The address to listen for TCP peers on.
      tcp_addr: Option<SocketAddr>,
      /// The address to listen for uTP peers on.
      utp_addr: Option<SocketAddr>,
      /// Address to connect to UDP [trackers](crate::tracker::Tracker).
      udp_addr: Option<SocketAddr>,
      /// Custom peer ID for peer discovery.
      #[builder(default)]
      custom_id: PeerId,
      /// Strategy for storing pieces of the torrent.
      #[builder(default)]
      piece_storage_strategy: PieceStorageStrategy,
      /// Runtime behavior settings for engine, torrent, peer, and tracker
      /// actors.
      settings: Option<Settings>,
      /// The mailbox size for each torrent instance.
      ///
      /// In simple terms, this is the number of messages that each torrent
      /// instance can have in queue.
      ///
      /// If `Some(0)` is provided, the mailbox will be unbounded (no limit).
      /// If `Some(_)` is provided, it overrides
      /// [`Settings::engine.
      /// torrent_mailbox_size`](crate::settings::EngineSettings::torrent_mailbox_size).
      /// If `None` is provided, the supplied [`Settings`] value is
      /// preserved.
      ///
      /// Higher values increase memory usage but reduce sender backpressure
      /// when the mailbox is busy, which can improve throughput. Lower values
      /// do the inverse.
      ///
      /// Default: `64` through [`Settings::default`] when no custom settings
      /// are supplied.
      mailbox_size: Option<usize>,
      /// If we autostart torrents as soon as we have [`Self::sufficient_peers`]
      /// peers connected.
      ///
      /// If `Some(_)` is provided, it overrides
      /// [`Settings::torrent.
      /// autostart`](crate::settings::TorrentSettings::autostart).
      /// If `None` is provided, the supplied [`Settings`] value is preserved.
      ///
      /// Default: `true` through [`Settings::default`] when no custom settings
      /// are supplied.
      autostart: Option<bool>,
      /// How many peers we need to have before we start downloading.
      ///
      /// Is ignored if [`Self::autostart`] is `false`.
      ///
      /// If `Some(_)` is provided, it overrides
      /// [`Settings::torrent.
      /// sufficient_peers`](crate::settings::TorrentSettings::sufficient_peers).
      /// If `None` is provided, the supplied [`Settings`] value is
      /// preserved.
      ///
      /// Default: `6` through [`Settings::default`] when no custom settings
      /// are supplied.
      sufficient_peers: Option<usize>,
      /// Default base path for torrents
      ///
      /// Default: `std::env::current_dir()`
      #[builder(into)]
      output_path: Option<PathBuf>,
   ) -> Self {
      let mut settings = settings.unwrap_or_default();
      if let Some(mailbox_size) = mailbox_size {
         settings.engine.torrent_mailbox_size = mailbox_size;
      }
      if let Some(autostart) = autostart {
         settings.torrent.autostart = autostart;
      }
      if let Some(sufficient_peers) = sufficient_peers {
         settings.torrent.sufficient_peers = sufficient_peers;
      }

      let output_path = match output_path {
         Some(path) => {
            if path.is_absolute() {
               path
            } else {
               std::env::current_dir()
                  .expect("Failed to get current dir")
                  .join(path)
            }
         }
         None => std::env::current_dir().expect("Failed to get current dir"),
      };

      let frontend = FrontendPublisher::new();
      let args = EngineActorArgs {
         tcp_addr,
         utp_addr,
         udp_addr,
         peer_id: Some(custom_id),
         piece_storage_strategy,
         settings,
         default_base_path: Some(output_path),
         frontend: frontend.clone(),
      };

      let actor = EngineActor::spawn(args);

      Engine { actor, frontend }
   }

   /// Just a helper function so we don't have to write `&self.0` all the time.
   fn actor(&self) -> &ActorRef<EngineActor> {
      &self.actor
   }

   /// Starts the torrenting process for a given torrent. This function
   /// automatically contacts trackers and connects to peers. The spawned
   /// [Torrent Actor](Torrent) will be controlled by the [Engine].
   ///
   /// This function accepts a typed [`TorrentSource`] so frontends can pass
   /// explicit user intent instead of relying on string-prefix detection.
   ///
   ///
   /// # Examples
   ///
   /// With a remote torrent file
   /// ```no_run
   /// use libtortillas::prelude::*;
   ///
   /// #[tokio::main]
   /// async fn main() {
   ///    let engine = Engine::default();
   ///    let torrent_link = "https://example.com/example.torrent";
   ///    let torrent = engine
   ///       .add_torrent(TorrentSource::remote_torrent_url(torrent_link))
   ///       .await
   ///       .expect("Failed to add torrent");
   ///
   ///    println!("Started torrenting: {}", torrent.key());
   /// }
   /// ```
   ///
   /// With a magnet URI
   /// ```no_run
   /// use libtortillas::prelude::*;
   ///
   /// #[tokio::main]
   /// async fn main() {
   ///    let engine = Engine::default();
   ///    let magnet_uri = "magnet:?xt=?????";
   ///    let torrent = engine
   ///       .add_torrent(TorrentSource::magnet(magnet_uri))
   ///       .await
   ///       .expect("Failed to add torrent");
   ///
   ///    println!("Started torrenting: {}", torrent.key());
   /// }
   /// ```
   pub async fn add_torrent(&self, source: TorrentSource) -> Result<Torrent, EngineError> {
      let metainfo = source.into_metainfo().await?;
      let info_hash = metainfo.info_hash()?;

      let torrent_ref = self
         .actor()
         .ask(CreateTorrent {
            metainfo: Box::new(metainfo),
            restore: None,
         })
         .await
         .map_err(|e| EngineError::Other(anyhow::anyhow!(e.to_string())))?;

      Ok(Torrent::new_with_frontend(
         info_hash,
         torrent_ref,
         self.frontend.clone(),
      ))
      // We don't need to assign link or insert the ref here because its already
      // done by the engine actor
   }

   /// Restores one torrent from a Serde-compatible persistence snapshot.
   ///
   /// Torrents that were downloading or seeding when captured resume after
   /// their piece state and storage configuration have been restored.
   pub async fn restore_torrent(
      &self, snapshot: crate::torrent::TorrentSnapshot,
   ) -> Result<Torrent, EngineError> {
      if snapshot.version != crate::torrent::TORRENT_SNAPSHOT_VERSION {
         return Err(
            crate::errors::TorrentError::InvalidSnapshot {
               reason: format!(
                  "unsupported version {}; expected {}",
                  snapshot.version,
                  crate::torrent::TORRENT_SNAPSHOT_VERSION
               ),
            }
            .into(),
         );
      }
      let info_hash = snapshot.info_hash;
      let metainfo_hash = snapshot.metainfo.info_hash()?;
      if metainfo_hash != info_hash {
         return Err(
            crate::errors::TorrentError::InvalidSnapshot {
               reason: "info hash does not match metainfo".to_string(),
            }
            .into(),
         );
      }

      let torrent_ref = match self
         .actor()
         .ask(CreateTorrent {
            metainfo: Box::new(snapshot.metainfo.clone()),
            restore: Some(Box::new(snapshot)),
         })
         .await
      {
         Ok(torrent) => torrent,
         Err(SendError::HandlerError(error)) => return Err(error),
         Err(error) => return Err(EngineError::Other(anyhow::anyhow!(error.to_string()))),
      };

      Ok(Torrent::new_with_frontend(
         info_hash,
         torrent_ref,
         self.frontend.clone(),
      ))
   }
   /// Starts all torrents managed by the engine.
   /// See [`Torrent::start`] for more information.
   pub async fn start_all(&self) -> Result<(), EngineError> {
      self
         .actor()
         .tell(StartAll)
         .await
         .map_err(|e| EngineError::Other(anyhow::anyhow!(e.to_string())))?;
      Ok(())
   }

   /// Returns a public handle for a torrent managed by this engine.
   pub async fn torrent(&self, info_hash: InfoHash) -> Result<Torrent, EngineError> {
      let actor = match self.actor().ask(GetTorrent { info_hash }).await {
         Ok(actor) => actor,
         Err(SendError::HandlerError(err)) => return Err(err),
         Err(err) => return Err(EngineError::Other(anyhow::anyhow!(err.to_string()))),
      };

      Ok(Torrent::new_with_frontend(
         info_hash,
         actor,
         self.frontend.clone(),
      ))
   }

   /// Removes a torrent from the engine and stops its actor gracefully.
   pub async fn remove_torrent(&self, info_hash: InfoHash) -> Result<(), EngineError> {
      let torrent = match self.actor().ask(RemoveTorrent { info_hash }).await {
         Ok(torrent) => torrent,
         Err(SendError::HandlerError(err)) => return Err(err),
         Err(err) => return Err(EngineError::Other(anyhow::anyhow!(err.to_string()))),
      };

      torrent
         .stop_gracefully()
         .await
         .map_err(|e| EngineError::Other(anyhow::anyhow!(e.to_string())))?;
      torrent.wait_for_shutdown().await;
      self.frontend.torrent_removed(info_hash);

      Ok(())
   }

   /// Gracefully shuts down the engine and its managed torrent actors.
   pub async fn shutdown(&self) -> Result<(), EngineError> {
      self
         .actor()
         .stop_gracefully()
         .await
         .map_err(|e| EngineError::Other(anyhow::anyhow!(e.to_string())))?;
      self.actor().wait_for_shutdown().await;

      Ok(())
   }

   /// Exports the current engine state with frontend-ready torrent snapshots.
   pub async fn export(&self) -> Result<EngineSnapshot, EngineError> {
      self.snapshot().await
   }

   /// Snapshots the current engine state with frontend-ready torrent views.
   pub async fn snapshot(&self) -> Result<EngineSnapshot, EngineError> {
      self
         .actor()
         .ask(SnapshotEngine)
         .await
         .map_err(|e| EngineError::Other(anyhow::anyhow!(e.to_string())))
   }

   /// Sends a typed frontend command to the engine or one of its torrents.
   pub async fn send(&self, command: CoreCommand) -> Result<CoreCommandResult, EngineError> {
      match command {
         CoreCommand::AddTorrent { source } => self
            .add_torrent(source)
            .await
            .map(CoreCommandResult::TorrentAdded),
         CoreCommand::StartAll => {
            self.start_all().await?;
            Ok(CoreCommandResult::Applied)
         }
         CoreCommand::StartTorrent { torrent } => {
            self
               .torrent(torrent)
               .await?
               .send(TorrentCommand::Start)
               .await?;
            Ok(CoreCommandResult::Applied)
         }
         CoreCommand::ResumeTorrent { torrent } => {
            self
               .torrent(torrent)
               .await?
               .send(TorrentCommand::Resume)
               .await?;
            Ok(CoreCommandResult::Applied)
         }
         CoreCommand::PauseTorrent { torrent } => {
            self
               .torrent(torrent)
               .await?
               .send(TorrentCommand::Pause)
               .await?;
            Ok(CoreCommandResult::Applied)
         }
         CoreCommand::StopTorrent { torrent } => {
            self
               .torrent(torrent)
               .await?
               .send(TorrentCommand::Stop)
               .await?;
            Ok(CoreCommandResult::Applied)
         }
         CoreCommand::RemoveTorrent { torrent } => {
            self.remove_torrent(torrent).await?;
            Ok(CoreCommandResult::Applied)
         }
         CoreCommand::Shutdown => {
            self.shutdown().await?;
            Ok(CoreCommandResult::Applied)
         }
         CoreCommand::SetTorrentOutputPath { torrent, path } => {
            self
               .torrent(torrent)
               .await?
               .send(TorrentCommand::SetOutputPath(path))
               .await?;
            Ok(CoreCommandResult::Applied)
         }
         CoreCommand::SetAutostart { torrent, enabled } => {
            self
               .torrent(torrent)
               .await?
               .send(TorrentCommand::SetAutostart(enabled))
               .await?;
            Ok(CoreCommandResult::Applied)
         }
         CoreCommand::SetSufficientPeers { torrent, peers } => {
            self
               .torrent(torrent)
               .await?
               .send(TorrentCommand::SetSufficientPeers(peers))
               .await?;
            Ok(CoreCommandResult::Applied)
         }
      }
   }

   /// Subscribes to typed engine and torrent events as they happen.
   ///
   /// The returned stream is bounded. A lagging frontend can read
   /// [`Self::live_view`] to rebuild its display state and then continue
   /// receiving events.
   #[must_use]
   pub fn subscribe(&self) -> EventSubscription {
      self.frontend.subscribe()
   }

   /// Creates a live listener with typed events and coherent current display
   /// state.
   #[must_use]
   pub fn listener(&self) -> EngineListener {
      EngineListener::new(self.frontend.clone())
   }

   /// Returns the current display-oriented engine state maintained by the live
   /// event publisher.
   #[must_use]
   pub fn live_view(&self) -> EngineView {
      self.frontend.view()
   }
}

impl Default for Engine {
   fn default() -> Self {
      Self::builder().build()
   }
}

#[cfg(test)]
mod snapshot_tests {
   use serde_json::{from_str, to_string};

   use super::*;
   use crate::{settings::Settings, testing};

   #[tokio::test]
   async fn engine_when_torrent_is_added_then_snapshots_persistence_state() {
      let mut settings = Settings::default();
      settings.dht.enabled = false;
      let engine = Engine::builder()
         .settings(settings)
         .autostart(false)
         .build();
      let torrent_path = testing::torrent_fixture_path(testing::BIG_BUCK_BUNNY_TORRENT_FILE);

      let torrent = engine
         .add_torrent(TorrentSource::torrent_file_path(torrent_path))
         .await
         .unwrap();
      let snapshot = engine.snapshot().await.unwrap();

      assert_eq!(snapshot.status, EngineStatus::Running);
      assert_eq!(snapshot.version, ENGINE_SNAPSHOT_VERSION);
      assert_eq!(snapshot.torrent_count, 1);
      assert_eq!(snapshot.torrents.len(), 1);
      assert_eq!(snapshot.torrents[0].info_hash, torrent.info_hash());
      assert_eq!(
         snapshot.torrents[0].version,
         crate::torrent::TORRENT_SNAPSHOT_VERSION
      );
      assert!(snapshot.torrents[0].info_dict.is_some());
      assert!(!snapshot.torrents[0].bitfield.is_empty());

      let snapshot_str = to_string(&snapshot).unwrap();
      let from_snapshot: EngineSnapshot = from_str(&snapshot_str).unwrap();

      assert_eq!(snapshot.version, from_snapshot.version);
      assert_eq!(snapshot.status, from_snapshot.status);
      assert_eq!(snapshot.torrent_count, from_snapshot.torrent_count);
      assert_eq!(
         snapshot.torrents[0].info_hash,
         from_snapshot.torrents[0].info_hash
      );
      assert_eq!(
         snapshot.torrents[0].bitfield,
         from_snapshot.torrents[0].bitfield
      );
   }
}

#[cfg(test)]
mod tests {
   use std::time::Duration;

   use kameo::actor::Spawn;
   use tokio::time::{sleep, timeout};

   use crate::{
      dht::{
         DHT_ID_LEN, DhtActor, DhtActorArgs, DhtTransport, NodeId, Query,
         messages::test_commands::LocalAddr,
      },
      engine::{Engine, TorrentSource},
      errors::EngineError,
      settings::{DhtSettings, Settings},
      testing::{
         BIG_BUCK_BUNNY_INFO_HASH, BIG_BUCK_BUNNY_MAGNET, BIG_BUCK_BUNNY_TORRENT_FILE, LocalPeer,
         peer_id, torrent_fixture_path,
      },
   };

   const DHT_TEST_BUFFER_SIZE: usize = 2048;
   const DHT_TEST_POLL_INTERVAL: Duration = Duration::from_millis(10);

   fn deterministic_settings() -> Settings {
      let mut settings = Settings::default();
      settings.dht.enabled = false;
      settings
   }

   #[tokio::test]
   async fn engine_when_torrent_source_is_file_path_then_adds_torrent() {
      let engine = Engine::builder()
         .settings(deterministic_settings())
         .autostart(false)
         .build();
      let source =
         TorrentSource::torrent_file_path(torrent_fixture_path(BIG_BUCK_BUNNY_TORRENT_FILE));

      let torrent = engine.add_torrent(source).await.unwrap();
      let export = engine.export().await.unwrap();

      assert_eq!(torrent.info_hash().to_hex(), BIG_BUCK_BUNNY_INFO_HASH);
      assert_eq!(export.torrents.len(), 1);
   }

   #[tokio::test]
   async fn engine_when_torrent_source_is_magnet_uri_then_adds_torrent() {
      let engine = Engine::builder()
         .settings(deterministic_settings())
         .autostart(false)
         .build();
      let source = TorrentSource::magnet(BIG_BUCK_BUNNY_MAGNET);

      let torrent = engine.add_torrent(source).await.unwrap();
      let export = engine.export().await.unwrap();

      assert_eq!(torrent.info_hash().to_hex(), BIG_BUCK_BUNNY_INFO_HASH);
      assert_eq!(export.torrents.len(), 1);
   }

   #[tokio::test]
   async fn engine_when_torrent_source_type_does_not_match_then_returns_typed_error() {
      let engine = Engine::builder()
         .settings(deterministic_settings())
         .autostart(false)
         .build();
      let source = TorrentSource::remote_torrent_url("magnet:?xt=urn:btih:not-a-url");

      let error = engine.add_torrent(source).await.unwrap_err();

      assert!(matches!(
         error,
         EngineError::InvalidTorrentSource {
            source_type: "remote URL",
            ..
         }
      ));
   }

   #[tokio::test]
   async fn engine_when_dht_returns_peer_then_connects_torrent_swarm() {
      let info_hash = crate::hashes::InfoHash::from_hex(BIG_BUCK_BUNNY_INFO_HASH).unwrap();
      let dht_id = NodeId::from(info_hash);
      let seed = DhtActor::spawn(DhtActorArgs {
         id: Some(NodeId::from_bytes([1; DHT_ID_LEN])),
         settings: DhtSettings {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            bootstrap_nodes: Vec::new(),
            query_timeout: Duration::from_secs(1),
            receive_buffer_size: DHT_TEST_BUFFER_SIZE,
            ..DhtSettings::default()
         },
      });
      let seed_addr = seed.ask(LocalAddr).await.unwrap();
      let announcer = DhtTransport::bind(
         "127.0.0.1:0".parse().unwrap(),
         Duration::from_secs(1),
         DHT_TEST_BUFFER_SIZE,
      )
      .await
      .unwrap();
      let receiver = announcer.clone();
      let receive_task = tokio::spawn(async move {
         loop {
            let Ok((message, addr)) = receiver.receive().await else {
               break;
            };
            receiver.complete(&message, addr).await;
         }
      });
      let local_peer = LocalPeer::start(peer_id(), Vec::new()).await.unwrap();
      let token = announcer
         .query(
            seed_addr,
            Query::GetPeers {
               id: NodeId::from_bytes([2; DHT_ID_LEN]),
               info_hash: dht_id,
            },
         )
         .await
         .unwrap()
         .token
         .unwrap();
      announcer
         .query(
            seed_addr,
            Query::AnnouncePeer {
               id: NodeId::from_bytes([2; DHT_ID_LEN]),
               info_hash: dht_id,
               port: local_peer.peer().port,
               token,
               implied_port: false,
            },
         )
         .await
         .unwrap();
      let settings = Settings {
         dht: DhtSettings {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            bootstrap_nodes: vec![seed_addr.to_string()],
            query_timeout: Duration::from_secs(1),
            receive_buffer_size: DHT_TEST_BUFFER_SIZE,
            ..DhtSettings::default()
         },
         ..Settings::default()
      };
      let engine = Engine::builder()
         .settings(settings)
         .autostart(false)
         .sufficient_peers(1)
         .build();
      let magnet = format!("magnet:?xt=urn:btih:{BIG_BUCK_BUNNY_INFO_HASH}&dn=dht-test");

      engine
         .add_torrent(TorrentSource::magnet(magnet))
         .await
         .unwrap();

      timeout(Duration::from_secs(2), async {
         loop {
            if !local_peer.handshakes().await.is_empty() {
               break;
            }
            sleep(DHT_TEST_POLL_INTERVAL).await;
         }
      })
      .await
      .unwrap();

      engine.shutdown().await.unwrap();
      receive_task.abort();
      seed.kill();
   }
}
