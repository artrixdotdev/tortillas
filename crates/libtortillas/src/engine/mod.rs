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
mod source;

use std::{net::SocketAddr, path::PathBuf};

pub(crate) use actor::*;
use bon;
use kameo::actor::{ActorRef, Spawn};
pub(crate) use messages::*;
use serde::{Deserialize, Serialize};
pub use source::*;

use self::commands::{CreateTorrent, ExportEngine, StartAll};
use crate::{
   errors::EngineError,
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

#[cfg(test)]
mod tests {
   use crate::{
      engine::{Engine, TorrentSource},
      errors::EngineError,
      testing::{BIG_BUCK_BUNNY_INFO_HASH, BIG_BUCK_BUNNY_TORRENT_FILE, torrent_fixture_path},
   };

   #[tokio::test]
   async fn engine_when_torrent_source_is_file_path_then_adds_torrent() {
      let engine = Engine::builder().autostart(false).build();
      let source =
         TorrentSource::torrent_file_path(torrent_fixture_path(BIG_BUCK_BUNNY_TORRENT_FILE));

      let torrent = engine.add_torrent(source).await.unwrap();
      let export = engine.export().await.unwrap();

      assert_eq!(torrent.info_hash().to_hex(), BIG_BUCK_BUNNY_INFO_HASH);
      assert_eq!(export.torrents.len(), 1);
   }

   #[tokio::test]
   async fn engine_when_torrent_source_type_does_not_match_then_returns_typed_error() {
      let engine = Engine::builder().autostart(false).build();
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
}
