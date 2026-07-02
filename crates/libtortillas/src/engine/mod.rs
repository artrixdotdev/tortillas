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
//!       .add_torrent("magnet:?xt=urn:btih:...")
//!       .await
//!       .expect("Failed to add torrent");
//!
//!    println!("Started torrenting: {}", torrent.key());
//! }
//! ```

mod actor;
mod messages;

use std::{net::SocketAddr, path::PathBuf};

pub(crate) use actor::*;
use bon;
use kameo::actor::{ActorRef, Spawn};
pub(crate) use messages::*;
use serde::{Deserialize, Serialize};
use tracing::error;

use self::commands::{CreateTorrent, ExportEngine, StartAll};
use crate::{
   errors::EngineError,
   metainfo::{MetaInfo, TorrentFile},
   peer::PeerId,
   settings::Settings,
   torrent::{PieceStorageStrategy, Torrent, TorrentExport},
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
/// // Create an engine with no explicit addresses
/// let engine = Engine::builder()
///    // Optionally provide addresses for our sockets to listen on
///    .tcp_addr("127.0.0.1:6881".parse::<SocketAddr>().unwrap())
///    .utp_addr("127.0.0.1:6882".parse::<SocketAddr>().unwrap())
///    .udp_addr("127.0.0.1:6883".parse::<SocketAddr>().unwrap())
///    .build();
/// ```
/// Or with all default settings
/// ```no_run
/// use libtortillas::prelude::*;
/// let engine = Engine::default();
/// ```
#[derive(Debug, Clone)]
pub struct Engine(ActorRef<EngineActor>);

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

      let args = EngineActorArgs {
         tcp_addr,
         utp_addr,
         udp_addr,
         peer_id: Some(custom_id),
         piece_storage_strategy,
         settings,
         default_base_path: Some(output_path),
      };

      let actor = EngineActor::spawn(args);

      Engine(actor)
   }

   /// Just a helper function so we don't have to write `&self.0` all the time.
   fn actor(&self) -> &ActorRef<EngineActor> {
      &self.0
   }

   /// Starts the torrenting process for a given torrent. This function
   /// automatically contacts trackers and connects to peers. The spawned
   /// [Torrent Actor](Torrent) will be controlled by the [Engine].
   ///
   /// This function accepts the following as input:
   /// - A remote URL to a torrent file over HTTP/HTTPS
   /// - The path, either absolute or relative, to a local torrent file
   /// - A magnet URI
   ///
   /// If the inputted value is a remote url to a torrent file, this function
   /// requests the bytes and deserializes them into a [TorrentFile]. If
   /// it isn't, we assume that it is either a magnet URI or a path to a
   /// torrent file, and pass the string to [MetaInfo::new].
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
   ///       .add_torrent(torrent_link)
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
   ///       .add_torrent(magnet_uri)
   ///       .await
   ///       .expect("Failed to add torrent");
   ///
   ///    println!("Started torrenting: {}", torrent.key());
   /// }
   /// ```
   pub async fn add_torrent(&self, metainfo: impl ToString) -> Result<Torrent, EngineError> {
      let metainfo = metainfo.to_string();
      // File paths should either start with "/" or "./", and magnet URIs start
      // with "magnet:", so a check like this should be entirely appropriate.
      let metainfo = if metainfo.starts_with("http") {
         let torrent_file_bytes = reqwest::get(&metainfo)
            .await
            .map_err(EngineError::MetaInfoFetchError)?
            .bytes()
            .await
            .map_err(EngineError::MetaInfoFetchError)?;
         let torrent_file = TorrentFile::parse(&torrent_file_bytes);
         torrent_file.map_err(|_| {
            error!(remote_url = metainfo);
            EngineError::MetaInfoDeserializeError
         })?
      } else {
         MetaInfo::new(metainfo.clone()).await.map_err(|_| {
            error!(magnet_uri_or_file = metainfo);
            EngineError::MetaInfoDeserializeError
         })?
      };

      let info_hash = metainfo.info_hash()?;

      let torrent_ref = self
         .actor()
         .ask(CreateTorrent {
            metainfo: Box::new(metainfo),
         })
         .await
         .map_err(|e| EngineError::Other(anyhow::anyhow!(e.to_string())))?;

      Ok(Torrent::new(info_hash, torrent_ref))
      // We don't need to assign link or insert the ref here because its already
      // done by the engine actor
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

   /// Exports the current state of the engine.
   /// See [`Torrent::export`] for more information.
   pub async fn export(&self) -> Result<EngineExport, EngineError> {
      self
         .actor()
         .ask(ExportEngine)
         .await
         .map_err(|e| EngineError::Other(anyhow::anyhow!(e.to_string())))
   }
}

impl Default for Engine {
   fn default() -> Self {
      Self::builder().build()
   }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineExport {
   pub torrents: Vec<TorrentExport>,
}
