use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::trace;

use crate::errors::TrackerError;

/// Tracker for websockets
pub struct WssTracker {
   uri: String,
   pub stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl WssTracker {
   pub async fn new(&self, uri: String) -> Result<Self, TrackerError> {
      let (ws_stream, _) = connect_async(&uri).await.unwrap();
      trace!("Connected to WSS tracker at {}", uri);
      Ok(WssTracker {
         uri,
         stream: ws_stream,
      })
   }
}
