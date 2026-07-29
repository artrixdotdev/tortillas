use std::{
   sync::{
      Arc,
      atomic::{AtomicBool, AtomicUsize, Ordering},
   },
   time::Instant,
};

use atomic_time::AtomicOptionInstant;

use super::PeerActor;
#[cfg(feature = "live")]
use crate::metrics::{ByteCount, PeerMetrics, TrafficTotals, TransferMetrics};

/// Runtime state for a connected peer.
///
/// This includes both the state defined in
/// [BEP 0003](https://www.bittorrent.org/beps/bep_0003.html) and local
/// accounting such as total bytes downloaded.
///
/// The general intent of this struct is to make it easier for us to "throw"
/// state across threads -- every field in here is an atomic Arc, which means
/// that it's very easy to do something like this (in an impl of the PeerActor
/// struct):
///
/// ```ignore
/// tokio::spawn(async move {
///    some_fn(self.state.clone());
/// })
/// ```
///
/// `clone()` operations on this struct should be relatively lightweight, seeing
/// that everything is contained in an Arc.
#[derive(Clone)]
pub(crate) struct PeerState {
   /// Whether we are choking the remote peer
   am_choking: Arc<AtomicBool>,
   /// Whether the remote peer is interested in us
   peer_interested: Arc<AtomicBool>,
   /// Whether the remote peer is choking us
   peer_choking: Arc<AtomicBool>,
   /// Our interest status
   am_interested: Arc<AtomicBool>,
   /// Defaults to None. Does not update on initial handshake, initial sending
   /// of bitfield, or initial sending of Interested message.
   last_message_sent: Arc<AtomicOptionInstant>,
   /// Defaults to None. Does not update on initial handshake, initial sending
   /// of bitfield, or initial sending of Interested message.
   last_message_received: Arc<AtomicOptionInstant>,
   /// Total bytes downloaded.
   bytes_downloaded: Arc<AtomicUsize>,
   /// Total bytes uploaded.
   bytes_uploaded: Arc<AtomicUsize>,
}

impl Default for PeerState {
   fn default() -> Self {
      Self::new()
   }
}

impl PeerState {
   pub(crate) fn new() -> Self {
      Self {
         am_choking: Arc::new(true.into()),
         peer_interested: Arc::new(false.into()),
         peer_choking: Arc::new(true.into()),
         am_interested: Arc::new(false.into()),
         last_message_received: Arc::new(AtomicOptionInstant::none()),
         last_message_sent: Arc::new(AtomicOptionInstant::none()),
         bytes_downloaded: Arc::new(0.into()),
         bytes_uploaded: Arc::new(0.into()),
      }
   }

   pub(crate) fn increment_bytes_downloaded(&self, bytes: usize) {
      self.bytes_downloaded.fetch_add(bytes, Ordering::Relaxed);
   }

   pub(crate) fn increment_bytes_uploaded(&self, bytes: usize) {
      self.bytes_uploaded.fetch_add(bytes, Ordering::Relaxed);
   }

   #[cfg(not(feature = "live"))]
   pub(crate) fn bytes_downloaded(&self) -> usize {
      self.bytes_downloaded.load(Ordering::Relaxed)
   }

   #[cfg(not(feature = "live"))]
   pub(crate) fn bytes_uploaded(&self) -> usize {
      self.bytes_uploaded.load(Ordering::Relaxed)
   }
}

#[cfg(feature = "live")]
impl PeerState {
   pub(crate) fn traffic_totals(&self) -> TrafficTotals {
      TrafficTotals {
         downloaded: ByteCount(
            u64::try_from(self.bytes_downloaded.load(Ordering::Relaxed)).unwrap_or(u64::MAX),
         ),
         uploaded: ByteCount(
            u64::try_from(self.bytes_uploaded.load(Ordering::Relaxed)).unwrap_or(u64::MAX),
         ),
      }
   }
}

impl PeerActor {
   pub(crate) fn set_client_choking(&self, is_choked: bool) {
      self.state.am_choking.store(is_choked, Ordering::Release);
   }

   pub(crate) fn set_interested(&self, is_interested: bool) {
      self
         .state
         .peer_interested
         .store(is_interested, Ordering::Release);
   }

   pub(crate) fn set_am_choked(&self, is_choked: bool) {
      self.state.peer_choking.store(is_choked, Ordering::Release);
   }

   pub(crate) fn set_am_interested(&self, is_interested: bool) {
      self
         .state
         .am_interested
         .store(is_interested, Ordering::Release);
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

   pub(crate) fn choked(&self) -> bool {
      self.state.am_choking.load(Ordering::Acquire)
   }

   pub(crate) fn interested(&self) -> bool {
      self.state.peer_interested.load(Ordering::Acquire)
   }

   pub(crate) fn am_choked(&self) -> bool {
      self.state.peer_choking.load(Ordering::Acquire)
   }

   #[cfg(feature = "live")]
   pub(crate) fn am_interested(&self) -> bool {
      self.state.am_interested.load(Ordering::Acquire)
   }

   pub(crate) fn last_message_sent(&self) -> Option<Instant> {
      self.state.last_message_sent.load(Ordering::Acquire)
   }

   pub(crate) fn last_message_received(&self) -> Option<Instant> {
      self.state.last_message_received.load(Ordering::Acquire)
   }

   #[cfg(not(feature = "live"))]
   pub(crate) fn bytes_downloaded(&self) -> usize {
      self.state.bytes_downloaded()
   }

   #[cfg(not(feature = "live"))]
   pub(crate) fn bytes_uploaded(&self) -> usize {
      self.state.bytes_uploaded()
   }
}

#[cfg(feature = "live")]
impl PeerActor {
   pub(crate) fn traffic_totals(&self) -> TrafficTotals {
      self.state.traffic_totals()
   }

   pub(crate) fn metrics(&self) -> PeerMetrics {
      PeerMetrics {
         transfer: TransferMetrics {
            totals: self.traffic_totals(),
            samples: Vec::new(),
         },
         peer_choking: self.am_choked(),
         peer_interested: self.interested(),
         client_choking: self.choked(),
         client_interested: self.am_interested(),
         available_pieces: u64::try_from(self.pieces.count_ones()).unwrap_or(u64::MAX),
      }
   }
}
