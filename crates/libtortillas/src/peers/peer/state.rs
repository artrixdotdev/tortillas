use std::sync::{
   Arc,
   atomic::{AtomicBool, AtomicU64},
};

use atomic_time::AtomicOptionInstant;

/// A helper struct for Peer that maintains a given peers state. This state
/// includes both the state defined in [BEP 0003](https://www.bittorrent.org/beps/bep_0003.html) and our own state which we
/// wish to maintain (ex. total bytes downloaded).
///
/// The general intent of this struct is to make it easier for us to "throw"
/// state across threads -- every field in here is an atomic Arc, which means
/// that it's very easy to do something like this (in an impl of the Peer
/// struct):
///
/// ```no_run
/// tokio::spawn(async move {
///    some_fn(self.state.clone());
/// })
/// ```
///
/// `clone()` operations on this struct should be relatively lightweight, seeing
/// that everything is contained in an Arc.
#[derive(Clone)]
pub struct PeerState {
   /// Download rate measured in kilobytes per second
   pub download_rate: Arc<AtomicU64>,
   /// Upload rate measured in kilobytes per second
   pub upload_rate: Arc<AtomicU64>,
   /// The remote peer's choke status
   pub choked: Arc<AtomicBool>,
   /// The remote peer's interest status
   pub interested: Arc<AtomicBool>,
   /// Our choke status
   pub am_choked: Arc<AtomicBool>,
   /// Our interest status
   pub am_interested: Arc<AtomicBool>,
   /// The timestamp of the last time that a peer unchoked us
   pub last_optimistic_unchoke: Arc<AtomicOptionInstant>,
   /// Defaults to None. Does not update on initial handshake, initial sending
   /// of bitfield, or initial sending of Interested message.
   pub last_message_sent: Arc<AtomicOptionInstant>,
   /// Defaults to None. Does not update on initial handshake, initial sending
   /// of bitfield, or initial sending of Interested message.
   pub last_message_received: Arc<AtomicOptionInstant>,
   /// Total bytes downloaded
   pub bytes_downloaded: Arc<AtomicU64>,
   /// Total bytes uploaded
   pub bytes_uploaded: Arc<AtomicU64>,
}

impl PeerState {
   pub fn new() -> Self {
      PeerState {
         choked: Arc::new(true.into()),
         interested: Arc::new(false.into()),
         am_choked: Arc::new(true.into()),
         am_interested: Arc::new(false.into()),
         download_rate: Arc::new(0u64.into()),
         upload_rate: Arc::new(0u64.into()),
         last_optimistic_unchoke: Arc::new(AtomicOptionInstant::none()),
         last_message_received: Arc::new(AtomicOptionInstant::none()),
         last_message_sent: Arc::new(AtomicOptionInstant::none()),
         bytes_downloaded: Arc::new(0u64.into()),
         bytes_uploaded: Arc::new(0u64.into()),
      }
   }
}
