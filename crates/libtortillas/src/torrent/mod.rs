mod actor;
mod messages;
use std::path::PathBuf;

pub use actor::*;
use bytes::Bytes;
use kameo::actor::ActorRef;
pub(crate) use messages::*;
use tokio::sync::mpsc;

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
   // only incase the end developer choose to clone thist
   /// The raw bytes of the piece
   pub data: Bytes,
}

/// The specified method for getting the pieces for a given torrent.
///
/// # Variants
///
/// - [`Self::Folder()`]: The specified output folder
/// - [`Self::Stream`]: When specified, all pieces will be sent through a
///   message channel
#[allow(dead_code)]
pub(super) enum OutputStrategy {
   /// The specified output folder.
   ///
   /// This option must be set -- libtortillas will not function properly if the
   /// output directory is not specified. See
   /// [`Torrent::with_output_folder`] for more.
   Folder(PathBuf),
   /// Tells the [`TorrentActor`](crate::torrent::TorrentActor)
   /// to send all received pieces through a message channel instead of
   /// directly writing them to disk.
   Stream,
}

/// Should always be used through the [`Engine`](crate::engine::Engine)
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Torrent(InfoHash, ActorRef<TorrentActor>);

impl Torrent {
   pub(crate) fn new(info_hash: InfoHash, actor_ref: ActorRef<TorrentActor>) -> Self {
      Torrent(info_hash, actor_ref)
   }

   pub(crate) fn actor(&self) -> &ActorRef<TorrentActor> {
      &self.1
   }

   pub fn info_hash(&self) -> InfoHash {
      self.0
   }

   /// Alias for [Self::info_hash]
   pub fn key(&self) -> InfoHash {
      self.info_hash()
   }

   pub fn set_piece_storage(&self, piece_storage: PieceStorageStrategy) -> &Self {
      self
         .actor()
         .tell(TorrentMessage::PieceStorage(piece_storage))
         // Use blocking send here because we want to ensure the message is processed before
         // continuing.
         // Also because we don't want the end user to have to use async for this.
         .blocking_send()
         .expect("Failed to set piece storage");

      self
   }

   /// Specifies the output folder that each file will eventually be written to.
   ///
   /// This function or [Self::with_output_stream] is strictly required to be
   /// set before the download begins.
   ///
   /// # Examples
   ///
   /// ```no_run
   /// use libtortillas::prelude::*;
   /// let engine = Engine::default();
   /// let torrent = engine.add_torrent("https://example.com/file.torrent");
   /// torrent.with_output_folder("~/awesome-folder/");
   /// ```
   pub fn with_output_folder(&self, folder: impl Into<PathBuf>) {
      let strategy = OutputStrategy::Folder(folder.into());
      self
         .actor()
         .ask(TorrentRequest::OutputStrategy(strategy))
         .blocking_send()
         .expect("Failed to set output folder");
   }

   /// Configures this torrent to stream its output instead of writing it to
   /// disk.
   ///
   /// When using this mode, all downlaoded pieces are sent through a channel
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
   /// use libtortillas::prelude::*;
   /// let engine = Engine::default();
   /// let torrent = engine
   ///    .add_torrent("https://example.com/file.torrent")
   ///    // Required if you want to use a custom output stream
   ///    .set_piece_storage(PieceStorageStrategy::Disk("path/to/output").into());
   ///
   /// let stream = torrent.with_output_stream();
   ///
   /// tokio::spawn(async move {
   ///    while let Some(piece) = receiver.recv().await {
   ///       println!(
   ///          "Received piece {} ({} bytes)",
   ///          piece.index,
   ///          piece.data.len()
   ///       );
   ///       // Handle piece (e.g. write to memory, forward to player, etc.)
   ///    }
   /// });
   /// ```
   pub fn with_output_stream(&self) -> mpsc::Receiver<StreamedPiece> {
      let strategy = OutputStrategy::Stream;
      let res = self
         .actor()
         .ask(TorrentRequest::OutputStrategy(strategy))
         .blocking_send()
         .expect("Failed to send request for ");

      match res {
         TorrentResponse::OutputStrategy(Some(receiver)) => receiver,
         _ => unreachable!(),
      }
   }
}
