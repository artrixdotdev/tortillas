use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{SplitSink, SplitStream, StreamExt};
use futures::SinkExt;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{debug, error, trace};

use crate::hashes::Hash;
use crate::tracker::hash_to_utf8;
use crate::{hashes::InfoHash, peers::Peer};

use super::{TrackerRequest, TrackerTrait};

/// Tracker for websockets
#[derive(Clone)]
pub struct WssTracker {
   info_hash: InfoHash,
   params: TrackerRequest,
   peer_id: Hash<20>,
   interval: u32,
   write: Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>>,
   read: Arc<Mutex<SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>>>,
}

impl WssTracker {
   pub async fn new(
      uri: String,
      info_hash: InfoHash,
      peer_tracker_addr: Option<SocketAddr>,
   ) -> Self {
      let mut peer_id_bytes = [0u8; 20];
      rand::fill(&mut peer_id_bytes);
      let peer_id = Hash::new(peer_id_bytes);
      debug!(peer_id = %peer_id, "Generated peer ID");

      trace!("Attemping connection to WSS tracker: {}", uri);

      let (stream, _) = connect_async(&uri)
         .await
         .map_err(|e| {
            error!("Error connecting to peer: {}", e);
         })
         .unwrap();
      let (write, read) = stream.split();
      let arc_write = Arc::new(Mutex::new(write));
      let arc_read = Arc::new(Mutex::new(read));
      trace!("Connected to WSS tracker at {}", uri);

      WssTracker {
         info_hash,
         params: TrackerRequest::new(peer_tracker_addr),
         peer_id,
         interval: u32::MAX,
         write: arc_write,
         read: arc_read,
      }
   }
}

#[async_trait]
impl TrackerTrait for WssTracker {
   /// It should be noted that WebSockets are supposed to communicate in JSON. (This makes our
   /// lives very easy though)
   async fn get_peers(&mut self) -> Result<Vec<Peer>> {
      let mut tracker_request_as_json = serde_json::to_string(&self.params).unwrap();
      trace!("Generated request parameters");

      // {tracker_request_as_json,info_hash:"xyz",peer_id:"abc"}
      tracker_request_as_json.pop();
      let request = format!(
         "{},\"info_hash\":\"{}\",\"peer_id\":\"{}\",\"action\":\"announce\"}}",
         tracker_request_as_json,
         hash_to_utf8(self.info_hash),
         hash_to_utf8(self.peer_id)
      );

      trace!("Request json generated: {}", request);
      let message = Message::from(request);

      trace!("Sending message to tracker");
      self
         .write
         .lock()
         .await
         .send(message)
         .await
         .map_err(|e| {
            error!("Error sending message: {e}");
         })
         .unwrap();
      self
         .write
         .lock()
         .await
         .flush()
         .await
         .map_err(|e| {
            error!("Error sending message: {e}");
         })
         .unwrap();

      trace!("Recieving message from tracker");

      // This section of code is completely and utterly scuffed. self.read.collect() refuses to
      // work, so this is what we're stuck with for now.
      let output = self
         .read
         .lock()
         .await
         .next()
         .await
         .unwrap()
         .unwrap()
         .into_text()
         .unwrap()
         .to_string();

      trace!("Message recieved: {}", output);

      // Output should look something like this:
      // {"complete":0,"incomplete":0,"action":"announce","interval":120,"info_hash":"myhash"}
      let res_json: Value = serde_json::from_str(&output).unwrap();

      // Check for "failure_reason" key (response failed)
      let res_json = res_json.as_object().unwrap();
      if res_json.contains_key("failure reason") {
         panic!("Error: {}", res_json.get("failure reason").unwrap());
      }

      self.interval = res_json.get("interval").unwrap().as_u64().unwrap() as u32;

      // let arr = res_json.as_array().unwrap();
      // let mut res = vec![];
      // for peer in arr {
      //    let ip = IpAddr::from_str(peer["ip"].as_str().unwrap()).unwrap();
      //
      //    let port = peer["port"].as_u64().unwrap();
      //    let peer = Peer::new(ip, port.try_into().unwrap());
      //    res.push(peer);
      // }
      // Ok(res)
      let res: Vec<Peer> = vec![];
      Ok(res)
   }

   fn get_interval(&self) -> u32 {
      self.interval
   }
}

#[cfg(test)]
mod tests {

   use crate::{parser::TorrentFile, tracker::TrackerTrait};
   use tracing_test::traced_test;

   use crate::{parser::MetaInfo, tracker::wss::WssTracker};

   #[tokio::test]
   #[traced_test]
   async fn test_get_peers_with_wss_tracker() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/torrents/sintel.torrent");

      let metainfo = TorrentFile::parse(path).await.unwrap();
      match metainfo {
         MetaInfo::Torrent(torrent) => {
            let info_hash = torrent.info.hash();
            let uri = "wss://tracker.btorrent.xyz".into();

            let mut wss_tracker = WssTracker::new(uri, info_hash.unwrap(), None).await;

            // Spawn a task to re-fetch the latest list of peers at a given interval
            let mut rx = wss_tracker.stream_peers().await.unwrap();

            let peers = rx.recv().await.unwrap();

            let peer = &peers[0];
            assert!(peer.ip.is_ipv4());
         }
         _ => panic!("Expected Torrent"),
      }
   }

   #[tokio::test]
   #[traced_test]
   async fn test_get_peers_with_ws_tracker() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/torrents/big-buck-bunny.torrent");

      let metainfo = TorrentFile::parse(path).await.unwrap();
      match metainfo {
         MetaInfo::Torrent(torrent) => {
            let info_hash = torrent.info.hash();
            // From https://github.com/ngosang/trackerslist/blob/master/trackers_all_ws.txt. May
            // not be consistently present, as this repo is automatically updated/changed
            let uri = "ws://tracker.files.fm:7072/announce".into();

            let mut wss_tracker = WssTracker::new(uri, info_hash.unwrap(), None).await;

            // Spawn a task to re-fetch the latest list of peers at a given interval
            let mut rx = wss_tracker.stream_peers().await.unwrap();

            let peers = rx.recv().await.unwrap();

            let peer = &peers[0];
            assert!(peer.ip.is_ipv4());
         }
         _ => panic!("Expected Torrent"),
      }
   }
}
