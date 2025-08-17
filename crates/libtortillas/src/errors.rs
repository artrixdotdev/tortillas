//! # Tortillas Error System
//! This module contains refactored error types specifically designed for the
//! actor-based architecture of the Tortillas BitTorrent client. The error
//! system is organized around the main actor types and their specific failure
//! modes.
//!
//! ## Error Hierarchy
//!
//! - `TorrentEngineError`: High-level engine errors
//! - `PeerActorError`: All peer communication and protocol errors
//! - `TrackerActorError`: All tracker communication and protocol errors
//! - `TorrentError`: General torrent management errors
//!
//! ## Design Principles
//!
//! 1. **Actor-Specific**: Each actor type has its own error enum with relevant
//!    variants
//! 2. **Detailed Context**: Error variants include structured data for better
//!    debugging
//! 3. **Protocol-Aware**: Errors reflect BitTorrent protocol semantics
//! 4. **Composable**: Errors can be converted between types as needed

use std::net::AddrParseError;

use thiserror::Error;

use crate::peer::PeerId;

#[derive(Error, Debug)]
pub enum TorrentEngineError {
   /// No peers were provided by any tracker
   #[error("No peers were provided by trackers")]
   InsufficientPeers,

   /// Initial handshake with peers failed
   #[error("Initial handshake with peers failed")]
   InitialHandshakeFailed,

   /// Network setup failed with a specific reason
   #[error("Network setup failed: {0}")]
   NetworkSetupFailed(String),

   /// Any other engine-level error wrapped in [`anyhow::Error`]
   #[error(transparent)]
   Other(#[from] anyhow::Error),
}

#[derive(Error, Debug)]
pub enum PeerActorError {
   /// Peer connection failed (TCP/UTP setup issues)
   #[error("Connection failed: {0}")]
   ConnectionFailed(String),

   /// Handshake failed with a reason
   #[error("Handshake failed: {reason}")]
   HandshakeFailed { reason: String },

   /// Info hash mismatch during handshake
   #[error("Invalid handshake: expected info hash {expected}, received {received}")]
   HandshakeInfoHashMismatch { expected: String, received: String },

   /// Protocol magic string mismatch during handshake
   #[error("Invalid handshake: expected magic string {expected}, received {received}")]
   HandshakeMagicMismatch { expected: String, received: String },

   /// Invalid peer ID received
   #[error("Invalid peer ID: {0}")]
   InvalidPeerId(String),

   /// Failed to parse a peer message
   #[error("Message parsing failed: {reason}")]
   MessageParsingFailed { reason: String },

   /// Message too short compared to expected size
   #[error("Message too short: expected at least {expected} bytes, received {received}")]
   MessageTooShort { expected: usize, received: usize },

   /// Invalid payload for a given message type
   #[error("Invalid message payload: {message_type}")]
   InvalidMessagePayload { message_type: String },

   /// General BitTorrent protocol violation
   #[error("Protocol violation: {0}")]
   ProtocolViolation(String),

   /// Peer disconnected unexpectedly
   #[error("Peer disconnected unexpectedly")]
   UnexpectedDisconnection,

   /// Metadata request failed
   #[error("Metadata request failed: {0}")]
   MetadataRequestFailed(String),

   /// Piece validation failed at a given index/offset
   #[error("Piece validation failed: index {index}, offset {offset}")]
   PieceValidationFailed { index: usize, offset: usize },

   /// Failed to send data to peer
   #[error("Send operation failed: {0}")]
   SendFailed(String),

   /// Failed to receive data from peer
   #[error("Receive operation failed: {0}")]
   ReceiveFailed(String),

   /// Peer timed out due to inactivity
   #[error("Peer timeout: no activity for {seconds} seconds")]
   PeerTimeout { seconds: u64 },

   /// Buffer underflow during message parsing
   #[error("Buffer underflow during message parsing")]
   BufferUnderflow,

   /// Communication with supervisor actor failed
   #[error("Supervisor communication failed: {0}")]
   SupervisorCommunicationFailed(String),

   /// I/O error during peer operations
   #[error(transparent)]
   Io(#[from] std::io::Error),

   /// Any other peer-level error wrapped in `anyhow::Error`
   #[error(transparent)]
   Other(#[from] anyhow::Error),
}

#[derive(Error, Debug)]
pub enum TrackerActorError {
   /// Tracker initialization failed
   #[error("Tracker initialization failed: {tracker_type}")]
   InitializationFailed { tracker_type: String },

   /// Failed to connect to tracker
   #[error("Connection to tracker failed: {url}")]
   ConnectionFailed { url: String },

   /// Announce request failed
   #[error("Announce request failed: {reason}")]
   AnnounceFailed { reason: String },

   /// Scrape request failed
   #[error("Scrape request failed: {reason}")]
   ScrapeFailed { reason: String },

   /// Invalid tracker response
   #[error("Invalid tracker response: {reason}")]
   InvalidResponse { reason: String },

   /// Response too short compared to expected size
   #[error("Response too short: expected at least {expected} bytes, received {actual}")]
   ResponseTooShort { expected: usize, actual: usize },

   /// Protocol-specific error
   #[error("Protocol error: {protocol} - {reason}")]
   ProtocolError { protocol: String, reason: String },

   /// Transaction ID mismatch in UDP tracker protocol
   #[error("Transaction ID mismatch: expected {expected}, received {received}")]
   TransactionMismatch { expected: u32, received: u32 },

   /// Tracker returned an error message
   #[error("Tracker returned error: {message}")]
   TrackerError { message: String },

   /// Request timed out
   #[error("Request timeout after {seconds} seconds")]
   RequestTimeout { seconds: u64 },

   /// Invalid tracker URL
   #[error("Invalid tracker URL: {url}")]
   InvalidUrl { url: String },

   /// Unsupported tracker protocol (e.g., non-HTTP/UDP)
   #[error("Unsupported tracker protocol: {protocol}")]
   UnsupportedProtocol { protocol: String },

   /// Authentication with tracker failed
   #[error("Authentication failed: {reason}")]
   AuthenticationFailed { reason: String },

   /// Tracker rate-limited the client
   #[error("Rate limited by tracker: retry after {seconds} seconds")]
   RateLimited { seconds: u64 },

   /// Failed to parse peer list from tracker
   #[error("Peer list parsing failed: {reason}")]
   PeerListParsingFailed { reason: String },

   /// Invalid action received in UDP tracker protocol
   #[error("Invalid action received: {action}")]
   InvalidAction { action: u32 },

   /// Communication with supervisor actor failed
   #[error("Supervisor communication failed: {0}")]
   SupervisorCommunicationFailed(String),

   /// UDP socket operation failed
   #[error("UDP socket operation failed: {operation}")]
   UdpSocketFailed { operation: String },

   /// Network I/O error
   #[error(transparent)]
   Network(#[from] std::io::Error),

   /// HTTP request error
   #[error(transparent)]
   Http(#[from] reqwest::Error),

   /// Bencode decoding error
   #[error(transparent)]
   BencodeDecoding(#[from] serde_bencode::Error),

   /// Address parsing error
   #[error(transparent)]
   AddressParsing(#[from] AddrParseError),

   /// Hex decoding error
   #[error(transparent)]
   HexDecoding(#[from] hex::FromHexError),

   /// Any other tracker-level error wrapped in `anyhow::Error`
   #[error(transparent)]
   Other(#[from] anyhow::Error),
}

#[derive(Error, Debug)]
pub enum TorrentError {
   /// Invalid info hash in torrent metadata
   #[error("Invalid info hash: {hash}")]
   InvalidInfoHash { hash: String },

   /// Torrent metainfo validation failed
   #[error("Metainfo validation failed: {reason}")]
   MetainfoValidationFailed { reason: String },

   /// Piece verification failed at given index
   #[error("Piece verification failed: index {index}")]
   PieceVerificationFailed { index: usize },

   /// File I/O error during torrent operations
   #[error("File I/O error: {operation} - {reason}")]
   FileIoError { operation: String, reason: String },

   /// Bitfield operation failed
   #[error("Bitfield operation failed: {reason}")]
   BitfieldError { reason: String },

   /// Failed to spawn an actor
   #[error("Actor spawn failed: {actor_type}")]
   ActorSpawnFailed { actor_type: String },

   /// Actor communication failed
   #[error("Actor communication failed: {actor_type} - {reason}")]
   ActorCommunicationFailed { actor_type: String, reason: String },

   /// Torrent entered an invalid state
   #[error("Invalid torrent state: {state}")]
   InvalidState { state: String },

   /// Resource exhaustion (e.g., memory, file handles)
   #[error("Resource exhausted: {resource}")]
   ResourceExhausted { resource: String },

   /// Peer actor error bubbled up
   #[error(transparent)]
   PeerActor(#[from] PeerActorError),

   /// Tracker actor error bubbled up
   #[error(transparent)]
   TrackerActor(#[from] TrackerActorError),

   /// Any other torrent-level error wrapped in `anyhow::Error`
   #[error(transparent)]
   Other(#[from] anyhow::Error),
}
// Conversion implementations for backward compatibility during transition
impl From<num_enum::TryFromPrimitiveError<super::tracker::udp::Action>> for TrackerActorError {
   fn from(err: num_enum::TryFromPrimitiveError<super::tracker::udp::Action>) -> Self {
      TrackerActorError::InvalidAction { action: err.number }
   }
}

/// Helper trait for adding more information to standard error types
pub trait ErrorContext {
   fn with_peer_context(self, peer_id: &PeerId) -> PeerActorError;
   fn with_tracker_context(self, tracker_url: &str) -> TrackerActorError;
}

impl ErrorContext for std::io::Error {
   fn with_peer_context(self, peer_id: &PeerId) -> PeerActorError {
      PeerActorError::ConnectionFailed(format!("Peer {}: {}", peer_id, self))
   }

   fn with_tracker_context(self, tracker_url: &str) -> TrackerActorError {
      TrackerActorError::ConnectionFailed {
         url: tracker_url.to_string(),
      }
   }
}

impl ErrorContext for anyhow::Error {
   fn with_peer_context(self, peer_id: &PeerId) -> PeerActorError {
      PeerActorError::Other(self.context(format!("Peer {}", peer_id)))
   }

   fn with_tracker_context(self, tracker_url: &str) -> TrackerActorError {
      TrackerActorError::Other(self.context(format!("Tracker {}", tracker_url)))
   }
}
