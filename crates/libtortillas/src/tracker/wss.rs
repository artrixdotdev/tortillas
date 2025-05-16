use std::net::SocketAddr;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{SplitSink, SplitStream, StreamExt};
use futures::SinkExt;
use serde::{Deserialize, Serialize};
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

/// This is primarily used for serializing offers. Try torrenting a file with
/// <https://instant.webtorrent.dev/> and check out how the offers are "shaped" in the network tab.
#[derive(Serialize, Deserialize)]
struct WssOffer {
   #[serde(rename(deserialize = "type"))]
   offer_type: String,
   sdp: String,
   offer_id: String,
}

impl WssOffer {
   pub fn new(sdp: String) -> Self {
      let mut offer_id_bytes = [0u8; 20];
      rand::fill(&mut offer_id_bytes);
      let offer_id = Hash::new(offer_id_bytes);
      WssOffer {
         offer_type: "offer".into(),
         sdp,
         offer_id: hash_to_utf8(offer_id),
      }
   }
}

/// Again, please try torrenting using a site like <https://instant.webtorrent.dev/> and examine
/// the format that offers are sent in. We need to serialize offers in a format like this:
/// [
///    {
///      "offer": {
///         ...
///      }
///    }
///    {
///      "offer": {
///         ...
///      }
///    }
/// ]
/// Hence, the easiest thing to do is use a wrapper.
#[derive(Serialize, Deserialize)]
struct WssOfferWrapper {
   offer: WssOffer,
}

impl WssOfferWrapper {
   pub fn new(sdp: String) -> Self {
      WssOfferWrapper {
         offer: WssOffer::new(sdp),
      }
   }
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

   /// Prototype. Headers will be changed.
   pub async fn recv_peers(&mut self) -> bool {
      true
   }
}

#[async_trait]
impl TrackerTrait for WssTracker {
   /// It should be noted that WebSockets are intended to communicate in JSON. (This makes our
   /// lives very easy though)
   ///
   /// This does not initially return a list of peers, so to speak. Instead, it sends an SDP offer
   /// to the tracker, and the tracker forwards that SDP offer to relevant peers. Those peers then
   /// return an SDP answer to the tracker, which forwards the answer to us.
   async fn get_peers(&mut self) -> Result<Vec<Peer>> {
      let mut tracker_request_as_json = serde_json::to_string(&self.params).unwrap();
      trace!("Generated request parameters");

      // Generate offers
      let numwant = 5;
      let mut offers = vec![];
      let timestamp = UNIX_EPOCH.elapsed()?.as_secs();
      let raw_sdp_offer = format!(
         "{{\"offer\":\"\
         v=0\
         o=- {} {} IN IP4 127.0.0.1\
         s=-\
         \"}}",
         timestamp, timestamp
      );
      for _i in 0..numwant {
         let offer = WssOffer::new(raw_sdp_offer.clone());
         offers.push(offer);
      }

      // {tracker_request_as_json,info_hash:"xyz",peer_id:"abc",numwant:5}
      tracker_request_as_json.pop();
      let request = format!(
         "{},\"info_hash\":\"{}\",\"peer_id\":\"{}\",\"action\":\"announce\",\"numwant\":{}, \"offer\": {} }}",
         tracker_request_as_json,
         hash_to_utf8(self.info_hash),
         hash_to_utf8(self.peer_id),
         numwant,
         serde_json::to_string(&offers)?
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

      // SDP offer & answer
      // See <https://www.rfc-editor.org/rfc/rfc8866.html#name-sdp-specification> for more
      // information

      // ???

      // tmp
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
