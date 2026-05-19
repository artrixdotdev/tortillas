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
   errors::TorrentError,
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
/// This struct acts as the primary interface for controlling, configuring, and
/// reading render state for a torrent after it has been added to the
/// [`Engine`](crate::engine::Engine).
///
/// Internally, it wraps around the actor model using
/// [kameo](https://github.com/tqwewe/kameo). Frontends should use the methods
/// on this handle, especially [`Self::snapshot`], instead of depending on actor
/// references or internal message types.
///
/// # Examples
///
/// ```no_run
/// use libtortillas::prelude::*;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///    let engine = Engine::default();
///    let torrent = engine
///       .add_torrent("https://example.com/file.torrent")
///       .await
///       .expect("Failed to add torrent");
///
///    // Configure output
///    torrent.with_output_folder("downloads/").await?;
///
///    // Start downloading
///    torrent.start().await?;
///    Ok(())
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
   /// # Errors
   ///
   /// Returns an error if the torrent actor cannot be reached.
   pub async fn set_piece_storage(
      &self, piece_storage: PieceStorageStrategy,
   ) -> Result<(), TorrentError> {
      self
         .actor()
         .tell(TorrentMessage::PieceStorage(piece_storage))
         .await
         .map_err(|err| TorrentError::ActorCommunicationFailed {
            actor_type: "TorrentActor".to_string(),
            reason: err.to_string(),
         })
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
   /// async fn main() -> Result<(), Box<dyn std::error::Error>> {
   ///    let engine = Engine::default();
   ///    let torrent = engine
   ///       .add_torrent("https://example.com/file.torrent")
   ///       .await
   ///       .expect("Failed to add torrent");
   ///
   ///    torrent.with_output_folder("~/awesome-folder/").await;
   /// }
   /// ```
   pub async fn with_output_folder(&self, folder: impl Into<PathBuf>) -> Result<(), TorrentError> {
      self
         .actor()
         .tell(TorrentMessage::SetOutputPath(folder.into()))
         .await
         .map_err(|err| TorrentError::ActorCommunicationFailed {
            actor_type: "TorrentActor".to_string(),
            reason: err.to_string(),
         })
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
   /// # Errors
   ///
   /// Returns an error if the torrent actor cannot be reached. The actor still
   /// enforces torrent lifecycle invariants internally.
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
   ///       .await?;
   ///
   ///    // Attach our custom piece manager
   ///    torrent.with_piece_manager(LoggingPieceManager).await?;
   ///    Ok(())
   /// }
   /// ```
   pub async fn with_piece_manager<'a>(
      &'a self, piece_manager: impl PieceManager + 'a + 'static,
   ) -> Result<(), TorrentError> {
      self
         .actor()
         .tell(TorrentMessage::PieceManager(Box::new(piece_manager)))
         .await
         .map_err(|err| TorrentError::ActorCommunicationFailed {
            actor_type: "TorrentActor".to_string(),
            reason: err.to_string(),
         })
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
   pub async fn start(&self) -> Result<(), TorrentError> {
      let msg = TorrentMessage::SetState(TorrentState::Downloading);

      self
         .actor()
         .tell(msg)
         .await
         .inspect_err(|e| error!(error = %e, "Failed to start torrent"))
         .map_err(|err| TorrentError::ActorCommunicationFailed {
            actor_type: "TorrentActor".to_string(),
            reason: err.to_string(),
         })?;

      Ok(())
   }

   /// Returns the current state of the torrent. See [`TorrentState`]
   ///
   /// # Errors
   ///
   /// Returns an error if the torrent actor cannot be reached.
   pub async fn state(&self) -> Result<TorrentState, TorrentError> {
      let msg = TorrentRequest::GetState;

      match self
         .actor()
         .ask(msg)
         .await
         .map_err(|err| TorrentError::ActorCommunicationFailed {
            actor_type: "TorrentActor".to_string(),
            reason: err.to_string(),
         })? {
         TorrentResponse::GetState(state) => Ok(state),
         _ => unreachable!(),
      }
   }

   /// Returns a [`TorrentExport`] containing information about the torrent.
   ///
   /// The torrent export is a snapshot of the torrent's state at the time of
   /// the request, this can be used to resume a torrent later on without having
   /// to redownload pieces and/or seeding.
   ///
   /// # Errors
   ///
   /// Returns an error if the torrent actor cannot be reached.
   pub async fn export(&self) -> Result<TorrentExport, TorrentError> {
      let msg = TorrentRequest::Export;

      match self
         .actor()
         .ask(msg)
         .await
         .map_err(|err| TorrentError::ActorCommunicationFailed {
            actor_type: "TorrentActor".to_string(),
            reason: err.to_string(),
         })? {
         TorrentResponse::Export(export) => Ok(*export),
         _ => unreachable!(),
      }
   }

   /// Returns a compact, UI-friendly view of the torrent.
   ///
   /// Snapshots are intended for frontends that need render state without
   /// coupling themselves to actor messages or internal storage structures.
   pub async fn snapshot(&self) -> Result<TorrentSnapshot, TorrentError> {
      self.export().await.map(TorrentSnapshot::from)
   }

   /// If the torrent should automatically start when `sufficient_peers` is
   /// met.
   ///
   /// If this is false, you are expected to poll/wait for the torrent and
   /// manually start it using [`poll_ready`](Self::poll_ready) and
   /// [`start`](Self::start).
   ///
   /// Default: `true`
   pub async fn set_auto_start(&self, auto: bool) -> Result<(), TorrentError> {
      let msg = TorrentMessage::SetAutoStart(auto);
      self
         .actor()
         .tell(msg)
         .await
         .map_err(|err| TorrentError::ActorCommunicationFailed {
            actor_type: "TorrentActor".to_string(),
            reason: err.to_string(),
         })
   }

   /// Sets the number of peers we need to have before we start downloading.
   ///
   /// Default: `6`
   pub async fn set_sufficient_peers(&self, peers: usize) -> Result<(), TorrentError> {
      let msg = TorrentMessage::SetSufficientPeers(peers);
      self
         .actor()
         .tell(msg)
         .await
         .map_err(|err| TorrentError::ActorCommunicationFailed {
            actor_type: "TorrentActor".to_string(),
            reason: err.to_string(),
         })
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
   pub async fn poll_ready(&self) -> Result<(), TorrentError> {
      let (hook, hook_rx) = oneshot::channel();
      let msg = TorrentMessage::ReadyHook(hook);
      // We don't care about the response, we just want to make sure the
      // actor is alive
      self
         .actor()
         .tell(msg)
         .await
         .map_err(|err| TorrentError::ActorCommunicationFailed {
            actor_type: "TorrentActor".to_string(),
            reason: err.to_string(),
         })?;
      // This will block until the hook is called
      hook_rx
         .await
         .map_err(|err| TorrentError::ActorCommunicationFailed {
            actor_type: "TorrentActor".to_string(),
            reason: err.to_string(),
         })?;

      Ok(())
   }
}

/// A small, serializable torrent view for UI rendering and API consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentSnapshot {
   pub info_hash: InfoHash,
   pub name: Option<String>,
   pub state: TorrentState,
   pub progress: f32,
   pub completed_pieces: usize,
   pub total_pieces: usize,
   pub total_bytes: Option<usize>,
   pub auto_start: bool,
   pub sufficient_peers: usize,
   pub output_path: Option<PathBuf>,
}

impl From<TorrentExport> for TorrentSnapshot {
   fn from(export: TorrentExport) -> Self {
      let completed_pieces = export.bitfield.count_ones();
      let total_pieces = export.bitfield.len();
      let progress = if total_pieces == 0 {
         0.0
      } else {
         completed_pieces as f32 / total_pieces as f32
      };
      let name = export.info_dict.as_ref().map(|info| info.name.clone());
      let total_bytes = export.info_dict.as_ref().map(Info::total_length);

      Self {
         info_hash: export.info_hash,
         name,
         state: export.state,
         progress,
         completed_pieces,
         total_pieces,
         total_bytes,
         auto_start: export.auto_start,
         sufficient_peers: export.sufficient_peers,
         output_path: export.output_path,
      }
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

#[cfg(test)]
mod tests {
   use bitvec::vec::BitVec;

   use super::*;
   use crate::metainfo::TorrentFile;

   #[test]
   fn torrent_snapshot_summarizes_export() {
      let MetaInfo::Torrent(metainfo) = TorrentFile::parse(include_bytes!(
         "../../tests/torrents/big-buck-bunny.torrent"
      ))
      .unwrap() else {
         panic!("expected torrent metainfo");
      };
      let info_hash = metainfo.info.hash().unwrap();
      let info = metainfo.info.clone();
      let mut bitfield = BitVec::repeat(false, info.piece_count());
      bitfield.set(0, true);
      bitfield.set(1, true);

      let export = TorrentExport {
         info_hash,
         state: TorrentState::Inactive,
         auto_start: true,
         sufficient_peers: 6,
         output_path: Some("downloads".into()),
         metainfo: MetaInfo::Torrent(metainfo),
         piece_storage: PieceStorageStrategy::InFile,
         info_dict: Some(info.clone()),
         bitfield,
         block_map: BlockMap::new(),
      };

      let snapshot = TorrentSnapshot::from(export);

      assert_eq!(snapshot.info_hash, info_hash);
      assert_eq!(snapshot.name, Some(info.name.clone()));
      assert_eq!(snapshot.completed_pieces, 2);
      assert_eq!(snapshot.total_pieces, info.piece_count());
      assert_eq!(snapshot.total_bytes, Some(info.total_length()));
      assert!(snapshot.progress > 0.0);
      assert!(snapshot.progress < 1.0);
   }
}
