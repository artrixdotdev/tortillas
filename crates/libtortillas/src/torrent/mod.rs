mod actor;
mod messages;
mod piece_manager;
use std::path::PathBuf;

pub use actor::*;
use bytes::Bytes;
use kameo::actor::ActorRef;
pub(crate) use messages::*;
use tokio::sync::mpsc;
use tracing::error;

pub mod util;

use crate::hashes::InfoHash;

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

/// The specified method for getting the pieces for a given torrent.
///
/// # Variants
///
/// - [`Self::Folder(PathBuf)`]: The specified output folder
/// - [`Self::Stream`]: When specified, all pieces will be sent through a
///   message channel
#[allow(dead_code)]
pub(super) enum OutputStrategy {
   /// The specified output folder.
   ///
   /// One output strategy must be configured before starting the torrent
   /// (either a folder or streaming). See
   /// [`Torrent::with_output_folder`] and [`Torrent::with_output_stream`].
   Folder(PathBuf),

   /// Tells the [`TorrentActor`](crate::torrent::TorrentActor)
   /// to send all received pieces through a message channel instead of
   /// directly writing them to disk.
   Stream,
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
   /// This function or [`Self::with_output_stream`] is strictly required to be
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
      let strategy = OutputStrategy::Folder(folder.into());
      self
         .actor()
         .ask(TorrentRequest::OutputStrategy(strategy))
         .await
         .expect("Failed to set output folder");
   }

   /// Configures this torrent to stream its output instead of writing it to
   /// disk.
   ///
   /// When using this mode, all downloaded pieces are sent through a channel
   /// rather than being persisted to the filesystem. This allows you to consume
   /// the torrent data in real time (e.g. for streaming, playing, custom
   /// storage backends, etc.).
   ///
   /// # Returns
   ///
   /// A [`mpsc::Receiver`] that yields a [`StreamedPiece`] on each iteration as
   /// they become available.
   ///
   /// # Responsibilities
   /// When using this mode, you are responsible for:
   /// - Continuously polling the returned receiver to avoid backpressure.
   /// - Handling piece ordering.
   ///
   /// # Notes
   ///
   /// - Pieces sent through this channel are **not** guaranteed to be in order.
   /// - You are **required** to use [`PieceStorageStrategy::Disk`] when using
   ///   this mode.
   ///
   /// # Example
   ///
   /// ```no_run
   /// use libtortillas::{prelude::*, torrent::PieceStorageStrategy};
   ///
   /// #[tokio::main]
   /// async fn main() {
   ///    let engine = Engine::default();
   ///    let torrent = engine
   ///       .add_torrent("https://example.com/file.torrent")
   ///       .await
   ///       .expect("Failed to add torrent");
   ///
   ///    // Required if you want to use a custom output stream
   ///    torrent
   ///       .set_piece_storage(PieceStorageStrategy::Disk("path/to/output".into()))
   ///       .await;
   ///
   ///    let mut receiver = torrent.with_output_stream().await;
   ///
   ///    tokio::spawn(async move {
   ///       while let Some(piece) = receiver.recv().await {
   ///          println!(
   ///             "Received piece {} ({} bytes)",
   ///             piece.index,
   ///             piece.data.len()
   ///          );
   ///          // Handle piece (e.g. write to memory, forward to player, etc.)
   ///       }
   ///    });
   /// }
   /// ```
   pub async fn with_output_stream(&self) -> mpsc::Receiver<StreamedPiece> {
      let strategy = OutputStrategy::Stream;
      let res = self
         .actor()
         .ask(TorrentRequest::OutputStrategy(strategy))
         .await
         .expect("Failed to request output stream");

      match res {
         TorrentResponse::OutputStrategy(Some(receiver)) => receiver,
         _ => unreachable!(),
      }
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
      let msg = TorrentMessage::Start;

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
      let msg = TorrentRequest::State;

      match self
         .actor()
         .ask(msg)
         .await
         .expect("Failed to send request for state")
      {
         TorrentResponse::State(state) => state,
         _ => unreachable!(),
      }
   }
}
