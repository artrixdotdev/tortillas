use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::StreamExt;
use futures::stream::{SplitSink, SplitStream, TryStreamExt};
use futures::SinkExt;
use serde_json::Value;
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
   /// It should be noted that WebSockets are supposed to communicate in JSON. (This makes our
   /// lives very easy though)
   async fn get_peers(&mut self) -> Result<Vec<Peer>> {
      trace!("Generated request parameters");
      let mut tracker_request_as_json = serde_json::to_string(&self.params).unwrap();

      // {tracker_request_as_json,info_hash:"xyz",peer_id:"abc"}
      tracker_request_as_json.pop();
      let request = format!(
         "{},\"info_hash\":\"{}\",\"peer_id\":\"{}\"}}",
         tracker_request_as_json, self.info_hash, self.peer_id
      );

      trace!("Request json generated: {}", request);
      let message = Message::text(tracker_request_as_json);

      trace!("Sending message to tracker");
      self.write.send(message).await;

      trace!("Recieving message from tracker");

      // This section of code is completely and utterly scuffed. self.read.collect() refuses to
      // work, so this is what we're stuck with for now.
      let mut output = "".to_string();
      while let Some(message) = self.read.next().await {
         let data = message.unwrap().into_text().unwrap().to_string();
         output += &data;
      }

      // Output should be a vec of peers
      let res_json = serde_json::from_str(&output);
      match res_json.unwrap() {
         Value::Array(arr) => {
            let mut res = vec![];
            for i in 0..(arr.len()) {
               let ip = IpAddr::from_str(arr[i]["ip"].as_str().unwrap()).unwrap();

               let port = arr[i]["port"].as_u64().unwrap();
               let peer = Peer::new(ip, port.try_into().unwrap());
               res.push(peer);
            }
            Ok(res)
         }
         _ => {
            panic!("Something went wrong with the peer response");
         }
      }
   }
}
