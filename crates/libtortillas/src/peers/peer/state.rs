use std::{
   sync::{
      Arc,
      atomic::{AtomicBool, AtomicU64, Ordering},
   },
   time::Instant,
};

use atomic_time::AtomicOptionInstant;

use crate::peers::Peer;

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
   download_rate: Arc<AtomicU64>,
   /// Upload rate measured in kilobytes per second
   upload_rate: Arc<AtomicU64>,
   /// The remote peer's choke status
   choked: Arc<AtomicBool>,
   /// The remote peer's interest status
   pub(crate) interested: Arc<AtomicBool>,
   /// Our choke status
   pub(crate) am_choked: Arc<AtomicBool>,
   /// Our interest status
   am_interested: Arc<AtomicBool>,
   /// The timestamp of the last time that a peer unchoked us
   last_optimistic_unchoke: Arc<AtomicOptionInstant>,
   /// Defaults to None. Does not update on initial handshake, initial sending
   /// of bitfield, or initial sending of Interested message.
   last_message_sent: Arc<AtomicOptionInstant>,
   /// Defaults to None. Does not update on initial handshake, initial sending
   /// of bitfield, or initial sending of Interested message.
   last_message_received: Arc<AtomicOptionInstant>,
   /// Total bytes downloaded
   bytes_downloaded: Arc<AtomicU64>,
   /// Total bytes uploaded
   bytes_uploaded: Arc<AtomicU64>,
}

impl Default for PeerState {
   fn default() -> Self {
      Self::new()
   }
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

/// A bunch of helper methods (basically getter and setter wrappers)
#[allow(dead_code)]
impl Peer {
   pub(crate) fn set_choked(&self, is_choked: bool) {
      self.state.choked.store(is_choked, Ordering::Release);
   }

   pub(crate) fn set_interested(&self, is_interested: bool) {
      self
         .state
         .interested
         .store(is_interested, Ordering::Release);
   }

   pub(crate) fn set_am_choked(&self, is_choked: bool) {
      self.state.am_choked.store(is_choked, Ordering::Release);
   }

   pub(crate) fn set_am_interested(&self, is_interested: bool) {
      self
         .state
         .am_interested
         .store(is_interested, Ordering::Release);
   }

   pub(crate) fn set_download_rate(&self, rate_kbps: u64) {
      self.state.download_rate.store(rate_kbps, Ordering::Release);
   }

   pub(crate) fn set_upload_rate(&self, rate_kbps: u64) {
      self.state.upload_rate.store(rate_kbps, Ordering::Release);
   }

   pub(crate) fn update_last_optimistic_unchoke(&self) {
      self
         .state
         .last_optimistic_unchoke
         .store(Some(Instant::now()), Ordering::Release);
   }

   pub(crate) fn update_last_message_sent(&self) {
      self
         .state
         .last_message_sent
         .store(Some(Instant::now()), Ordering::Release);
   }

   pub(crate) fn update_last_message_received(&self) {
      self
         .state
         .last_message_received
         .store(Some(Instant::now()), Ordering::Release);
   }

   pub(crate) fn increment_bytes_downloaded(&self, bytes: u64) {
      self
         .state
         .bytes_downloaded
         .fetch_add(bytes, Ordering::Relaxed);
   }

   pub(crate) fn increment_bytes_uploaded(&self, bytes: u64) {
      self
         .state
         .bytes_uploaded
         .fetch_add(bytes, Ordering::Relaxed);
   }

   pub(crate) fn choked(&self) -> bool {
      self.state.choked.load(Ordering::Acquire)
   }

   pub(crate) fn interested(&self) -> bool {
      self.state.interested.load(Ordering::Acquire)
   }

   pub(crate) fn am_choked(&self) -> bool {
      self.state.am_choked.load(Ordering::Acquire)
   }

   pub(crate) fn am_interested(&self) -> bool {
      self.state.am_interested.load(Ordering::Acquire)
   }

   pub fn download_rate(&self) -> u64 {
      self.state.download_rate.load(Ordering::Acquire)
   }

   pub fn upload_rate(&self) -> u64 {
      self.state.upload_rate.load(Ordering::Acquire)
   }

   pub(crate) fn last_optimistic_unchoke(&self) -> Option<Instant> {
      self.state.last_optimistic_unchoke.load(Ordering::Acquire)
   }

   pub fn last_message_sent(&self) -> Option<Instant> {
      self.state.last_message_sent.load(Ordering::Acquire)
   }

   pub fn last_message_received(&self) -> Option<Instant> {
      self.state.last_message_received.load(Ordering::Acquire)
   }

   pub(crate) fn bytes_downloaded(&self) -> u64 {
      self.state.bytes_downloaded.load(Ordering::Relaxed)
   }

   pub(crate) fn bytes_uploaded(&self) -> u64 {
      self.state.bytes_uploaded.load(Ordering::Relaxed)
   }
}
