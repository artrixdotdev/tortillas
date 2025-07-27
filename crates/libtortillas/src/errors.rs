use std::net::AddrParseError;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum TorrentEngineError {
   #[error("No peers were provided by trackers.")]
   InsufficientPeers,

   #[error("Initial handshake with peers failed.")]
   InitialHandshakeFailed,

   #[error(transparent)]
   Other(#[from] anyhow::Error),
}

#[derive(Error, Debug)]
pub enum TrackerError {
   #[error("Network error: {0}")]
   Network(#[from] std::io::Error),

   #[error("HTTP request error: {0}")]
   HttpRequest(#[from] reqwest::Error),

   #[error("Invalid response format: {0}")]
   InvalidFormat(String),

   #[error("Protocol error: {0}")]
   Protocol(String),

   #[error("Tracker not ready: {0}")]
   NotReady(String),

   #[error("Transaction ID mismatch")]
   TransactionMismatch,

   #[error("Tracker error: {0}")]
   TrackerMessage(String),

   #[error("Bencode parsing error: {0}")]
   BencodeError(#[from] serde_bencode::Error),

   #[error("Address parsing error: {0}")]
   AddrParse(#[from] AddrParseError),

   #[error("Hex decoding error: {0}")]
   HexDecode(#[from] hex::FromHexError),

   #[error("Invalid action: {0}")]
   InvalidAction(u32),

   #[error("Response too short: expected at least {expected} bytes, got {actual}")]
   ResponseTooShort { expected: usize, actual: usize },
}

#[derive(Error, Debug)]
pub enum UdpTrackerError {
   #[error("Invalid response: {0}")]
   InvalidResponse(String),

   #[error("Connection failed: {0}")]
   ConnectionFailed(String),

   #[error("Response too short: expected at least {expected} bytes, got {actual}")]
   ResponseTooShort { expected: usize, actual: usize },

   #[error("Tracker error: {0}")]
   TrackerMessage(String),

   #[error(transparent)]
   Tracker(#[from] TrackerError),

   #[error(transparent)]
   Other(#[from] anyhow::Error),
}

#[derive(Error, Debug)]
pub enum HttpTrackerError {
   #[error("Invalid info hash: {0}")]
   InvalidInfoHash(String),

   #[error("Failed to encode parameters: {0}")]
   ParameterEncoding(String),

   #[error("Invalid peer data: {0}")]
   InvalidPeerData(String),

   #[error(transparent)]
   Tracker(#[from] TrackerError),

   #[error(transparent)]
   Request(#[from] reqwest::Error),

   #[error(transparent)]
   Other(#[from] anyhow::Error),
}

#[derive(Error, Debug)]
pub enum PeerTransportError {
   #[error("Invalid response: {0}")]
   InvalidPeerResponse(String),

   #[error("Message failed")]
   MessageFailed,

   #[error("Connection failed: {0}")]
   ConnectionFailed(String),

   #[error("Response too short: expected at least {expected} bytes, got {received}")]
   ResponseTooShort { expected: usize, received: usize },

   #[error("Invalid magic string: expected {expected}, got {received}")]
   InvalidMagicString { expected: String, received: String },

   #[error("Invalid peer ID: {0}")]
   InvalidId(String),

   #[error("Received info hash {received} expected {expected}")]
   InvalidInfoHash { received: String, expected: String },

   #[error("Peer {0} not found")]
   NotFound(String),

   #[error("Failed to deserialize handshake")]
   DeserializationFailed,

   #[error("Message was too short")]
   MessageTooShort,

   #[error(transparent)]
   Other(#[from] anyhow::Error),
}

impl From<num_enum::TryFromPrimitiveError<super::tracker::udp::Action>> for TrackerError {
   fn from(err: num_enum::TryFromPrimitiveError<super::tracker::udp::Action>) -> Self {
      TrackerError::InvalidAction(err.number)
   }
}
