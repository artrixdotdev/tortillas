mod actor;
mod messages;
mod piece_manager;
use std::{path::PathBuf, sync::atomic::AtomicU8};

pub use actor::*;
use bitvec::vec::BitVec;
use bytes::Bytes;
use kameo::actor::ActorRef;
pub(crate) use messages::*;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::error;

pub mod util;
pub use piece_manager::PieceManager;

use crate::{
   hashes::InfoHash,
   prelude::{Info, MetaInfo},
};

/// A piece that we have received from a peer. [BEP 0003](https://www.bittorrent.org/beps/bep_0003.html)
/// describes the piece message. One more field is added in this struct: that
/// being [Self::name]. This is added so that the end developer (probably you!)
/// can differentiate between pieces in separate files or folders.
#[derive(Debug, Clone)]
pub struct StreamedPiece {
   /// The name of the file the piece belongs to
   pub name: String,

   /// The index of the piece
   pub index: usize,

   /// The offset of the piece
   pub offset: usize,

   // Note: libtortillas should never clone this struct. The `Clone` derive exists
   // only in case the end developer chooses to clone this.
   /// The raw bytes of the piece
   pub data: Bytes,
}

/// A handle to a torrent managed by the engine.
///
/// This struct acts as the primary interface for controlling and configuring
/// a torrent after it has been added to the [`Engine`](crate::engine::Engine).
///
/// Internally, it wraps around our Actor model using [kameo](https://github.com/tqwewe/kameo) which
/// performs the actual torrent logic.
///
/// # Examples
///
/// ```no_run
/// use libtortillas::prelude::*;
///
/// #[tokio::main]
/// async fn main() {
///    let engine = Engine::default();
///    let torrent = engine
///       .add_torrent("https://example.com/file.torrent")
///       .await
///       .expect("Failed to add torrent");
///
///    // Configure output
///    torrent.with_output_folder("downloads/").await;
///
///    // Start downloading
///    torrent.start().await;
/// }
/// ```
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Torrent(InfoHash, ActorRef<TorrentActor>);

impl Torrent {
   /// Creates a new [`Torrent`] handle from an [`InfoHash`] and a reference
   /// to its underlying [`TorrentActor`].
   ///
   /// This is typically only used internally by the engine.
   pub(crate) fn new(info_hash: InfoHash, actor_ref: ActorRef<TorrentActor>) -> Self {
      Torrent(info_hash, actor_ref)
   }

   /// Returns a reference to the underlying [`ActorRef`] for this torrent.
   ///
   /// This is primarily useful for internal communication with the
   /// [`TorrentActor`].
   pub(crate) fn actor(&self) -> &ActorRef<TorrentActor> {
      &self.1
   }

   /// Returns the [`InfoHash`] that uniquely identifies this torrent.
   pub fn info_hash(&self) -> InfoHash {
      self.0
   }

   /// Alias for [`Self::info_hash`].
   pub fn key(&self) -> InfoHash {
      self.info_hash()
   }

   /// Sets the piece storage strategy for this torrent.
   ///
   /// This determines how pieces are stored (e.g. in memory, on disk, etc.).
   ///
   /// If using [PieceStorageStrategy::Disk], the path *must* be set.
   ///
   /// # Panics
   ///
   /// Panics if:
   /// - The message could not be sent to the actor.
   /// - The torrent isn't in a [`TorrentState::Inactive`] state.
   pub async fn set_piece_storage(&self, piece_storage: PieceStorageStrategy) {
      self
         .actor()
         .tell(TorrentMessage::PieceStorage(piece_storage))
         .await
         .expect("Failed to set piece storage");
   }

   /// Specifies the output folder that each file will eventually be written to.
   ///
   /// This function or [`Self::with_piece_manager`] is strictly required to be
   /// set before the download begins.
   ///
   /// # Examples
   ///
   /// ```no_run
   /// use libtortillas::prelude::*;
   ///
   /// #[tokio::main]
   /// async fn main() {
   ///    let engine = Engine::default();
   ///    let torrent = engine
   ///       .add_torrent("https://example.com/file.torrent")
   ///       .await
   ///       .expect("Failed to add torrent");
   ///
   ///    torrent.with_output_folder("~/awesome-folder/").await;
   /// }
   /// ```
   pub async fn with_output_folder(&self, folder: impl Into<PathBuf>) {
      self
         .actor()
         .ask(TorrentMessage::SetOutputPath(folder.into()))
         .await
         .expect("Failed to set output folder");
   }

   /// Attaches a custom [`PieceManager`] to this torrent.
   ///
   /// A [`PieceManager`] defines how downloaded pieces are processed
   /// (e.g., writing them to disk, streaming them into memory, or integrating
   /// with a custom backend). By default, the torrent actor manages pieces
   /// internally, but calling this method allows you to override
   /// that behavior and implement your own piece-handling logic.
   ///
   /// # Requirements
   ///
   /// - You **must** first set the [`PieceStorageStrategy`] to
   ///   [`PieceStorageStrategy::Disk`] (via [`Self::set_piece_storage`]) before
   ///   calling this function. This is required because in order to seed, the
   ///   torrent actor needs some constant location to read previously
   ///   downloaded pieces from.
   ///
   /// # Parameters
   ///
   /// - `piece_manager`: Any type that implements [`PieceManager`].
   ///
   /// # Behavior
   ///
   /// - All subsequent pieces for this torrent will be dispatched to the given
   ///   `PieceManager`.
   /// - Any previously assigned manager will be replaced.
   /// - The `PieceManager` must remain valid for the lifetime of the torrent
   ///   actor.
   ///
   /// # Panics
   /// - If the torrent actor is dead.
   /// - If you attempt to attach a manager after the torrent has started.
   /// - If you attmept to attach a manager without [`PieceStorageStrategy`]
   ///   being set to [`PieceStorageStrategy::Disk`].
   ///
   /// # Example
   ///
   /// Creating a custom [`PieceManager`] that simply logs received pieces:
   ///
   /// ```no_run
   /// use std::path::PathBuf;
   ///
   /// use anyhow::Result;
   /// use async_trait::async_trait;
   /// use bytes::Bytes;
   /// use libtortillas::{
   ///    metainfo::Info,
   ///    prelude::*,
   ///    torrent::{PieceManager, PieceStorageStrategy},
   /// };
   ///
   /// #[derive(Debug)]
   /// struct LoggingPieceManager;
   ///
   /// #[async_trait]
   /// impl PieceManager for LoggingPieceManager {
   ///    fn info(&self) -> Option<&Info> {
   ///       None
   ///    }
   ///
   ///    async fn pre_start(&mut self, _info: Info) -> Result<()> {
   ///       println!("Torrent started with {:?}", _info.name);
   ///       Ok(())
   ///    }
   ///
   ///    async fn recv(&self, index: usize, data: Bytes) -> Result<()> {
   ///       println!("Received piece {index} ({} bytes)", data.len());
   ///       Ok(())
   ///    }
   /// }
   ///
   /// #[tokio::main]
   /// async fn main() {
   ///    let engine = Engine::default();
   ///
   ///    let torrent = engine
   ///       .add_torrent("https://example.com/file.torrent")
   ///       .await
   ///       .expect("failed to add torrent");
   ///
   ///    // Required before attaching a custom manager
   ///    torrent
   ///       .set_piece_storage(PieceStorageStrategy::Disk("path/to/storage".into()))
   ///       .await;
   ///
   ///    // Attach our custom piece manager
   ///    torrent.with_piece_manager(LoggingPieceManager).await;
   /// }
   /// ```
   /// ```
   pub async fn with_piece_manager<'a>(&'a self, piece_manager: impl PieceManager + 'a + 'static) {
      self
         .actor()
         .tell(TorrentMessage::PieceManager(Box::new(piece_manager)))
         .await
         .expect("Failed to request output stream");
   }

   /// Starts the torrent download and begins the download & seeding process.
   ///
   /// If all pieces have been downloaded, it will set the state to
   /// [`TorrentState::Seeding`]. Otherwise, it will set the state to
   /// [`TorrentState::Downloading`].
   ///
   /// # Errors
   ///
   /// Returns an error if the message could not be delivered to the actor.
   pub async fn start(&self) -> Result<(), anyhow::Error> {
      let msg = TorrentMessage::SetState(TorrentState::Downloading);

      self
         .actor()
         .tell(msg)
         .await
         .inspect_err(|e| error!(error = %e, "Failed to start torrent"))?;

      Ok(())
   }

   /// Returns the current state of the torrent. See [`TorrentState`]
   ///
   /// # Panics
   ///
   /// Panics if the message could not be sent to the actor.
   pub async fn state(&self) -> TorrentState {
      let msg = TorrentRequest::GetState;

      match self
         .actor()
         .ask(msg)
         .await
         .expect("Failed to send request for state")
      {
         TorrentResponse::GetState(state) => state,
         _ => unreachable!(),
      }
   }

   /// Returns a [`TorrentExport`] containing information about the torrent.
   ///
   /// The torrent export is a snapshot of the torrent's state at the time of
   /// the request, this can be used to resume a torrent later on without having
   /// to redownload pieces and/or seeding.
   ///
   /// # Panics
   ///
   /// Panics if the message could not be sent to the actor.
   pub async fn export(&self) -> TorrentExport {
      let msg = TorrentRequest::Export;

      match self
         .actor()
         .ask(msg)
         .await
         .expect("Failed to send request for state")
      {
         TorrentResponse::Export(export) => *export,
         _ => unreachable!(),
      }
   }

   /// If the torrent should automatically start when `sufficient_peers` is
   /// met.
   ///
   /// If this is false, you are expected to poll/wait for the torrent and
   /// manually start it using [`poll_ready`](Self::poll_ready) and
   /// [`start`](Self::start).
   ///
   /// Default: `true`
   pub async fn set_auto_start(&self, auto: bool) {
      let msg = TorrentMessage::SetAutoStart(auto);
      self
         .actor()
         .tell(msg)
         .await
         .expect("Failed to set auto start");
   }

   /// Sets the number of peers we need to have before we start downloading.
   ///
   /// Default: `6`
   pub async fn set_sufficient_peers(&self, peers: usize) {
      let msg = TorrentMessage::SetSufficientPeers(peers);
      self
         .actor()
         .tell(msg)
         .await
         .expect("Failed to set sufficient peers");
   }

   /// A blocking function that polls the torrent until it is ready to start
   /// downloading.
   ///
   /// The criteria for when the torrent is ready to start is:
   /// - We have sufficient peers (see [`Self::set_sufficient_peers`]), the
   ///   default is `6`
   /// - We have the info dict either from using a torrent file, or from a peer
   ///
   /// Required if you want to set [`Self::set_auto_start`] to `false`,
   /// otherwise the torrent won't actually download anything.
   ///
   /// # Example
   ///
   /// ```no_run
   /// use libtortillas::prelude::*;
   ///
   /// #[tokio::main]
   /// async fn main() {
   ///    let engine = Engine::default();
   ///    let torrent = engine
   ///       .add_torrent("https://example.com/file.torrent")
   ///       .await
   ///       .expect("Failed to add torrent");
   ///
   ///    torrent.set_auto_start(false).await;
   ///    torrent.poll_ready().await.expect("Failed to poll torrent");
   ///    torrent.start().await.expect("Failed to start torrent");
   /// }
   /// ```
   pub async fn poll_ready(&self) -> Result<(), anyhow::Error> {
      let (hook, hook_rx) = oneshot::channel();
      let msg = TorrentMessage::ReadyHook(hook);
      // We don't care about the response, we just want to make sure the
      // actor is alive
      self.actor().tell(msg).await?;
      // This will block until the hook is called
      hook_rx.await?;

      Ok(())
   }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentExport {
   pub info_hash: InfoHash,
   pub state: TorrentState,
   pub auto_start: bool,
   pub sufficient_peers: usize,
   pub output_path: Option<PathBuf>,
   pub metainfo: MetaInfo,
   pub piece_storage: PieceStorageStrategy,
   pub info_dict: Option<Info>,
   pub bitfield: BitVec<AtomicU8>,
   pub block_map: BlockMap,
}
