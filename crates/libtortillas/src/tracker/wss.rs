use std::net::SocketAddr;

use anyhow::Result;
use async_trait::async_trait;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::StreamExt;
use tokio::net::TcpStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{debug, trace};

use crate::hashes::Hash;
use crate::{hashes::InfoHash, peers::Peer};

use super::{TrackerRequest, TrackerTrait};

/// Tracker for websockets
pub struct WssTracker {
   uri: String,
   info_hash: InfoHash,
   read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
   write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
   params: TrackerRequest,
   peer_id: Hash<20>,
   interval: u32,
}

impl WssTracker {
   pub async fn new(
      &self,
      uri: String,
      info_hash: InfoHash,
      peer_tracker_addr: Option<SocketAddr>,
   ) -> Self {
      let (stream, _) = connect_async(&uri).await.unwrap();
      let (write, read) = stream.split();
      trace!("Connected to WSS tracker at {}", uri);

      let mut peer_id_bytes = [0u8; 20];
      rand::fill(&mut peer_id_bytes);
      let peer_id = Hash::new(peer_id_bytes);
      debug!(peer_id = %peer_id, "Generated peer ID");

      WssTracker {
         uri,
         info_hash,
         read,
         write,
         params: TrackerRequest::new(peer_tracker_addr),
         peer_id,
         interval: u32::MAX,
      }
   }
}

#[async_trait]
impl TrackerTrait for WssTracker {
   async fn get_peers(&mut self) -> Result<Vec<Peer>> {}
}
