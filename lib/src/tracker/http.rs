use super::{Peer, TrackerTrait};
use crate::{
   errors::{HttpTrackerError, TrackerError},
   hashes::InfoHash,
};
use async_trait::async_trait;
use rand::distr::{Alphanumeric, SampleString};
use serde::{
   Deserialize, Serialize,
   de::{self, Visitor},
};
/// See https://www.bittorrent.org/beps/bep_0003.html
use std::{
   net::{Ipv4Addr, SocketAddr},
   str::FromStr,
};

use tracing::{debug, error, info, instrument, trace, warn};

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // REMOVE SOON
pub struct TrackerResponse {
   pub interval: usize,
   #[serde(deserialize_with = "deserialize_peers")]
   pub peers: Vec<Peer>,
}

/// Event. See <https://www.bittorrent.org/beps/bep_0003.html> @ trackers
#[derive(Debug, Deserialize, Serialize)]
pub enum Event {
   Started,
   Completed,
   Stopped,
   Empty,
}

/// Tracker request. See <https://www.bittorrent.org/beps/bep_0003.html> @ trackers
#[derive(Debug, Deserialize, Serialize)]
struct TrackerRequest {
   ip: Option<Ipv4Addr>,
   port: u16,
   uploaded: u8,
   downloaded: u8,
   left: Option<u8>,
   event: Event,
   peer_tracker_addr: SocketAddr,
}

impl TrackerRequest {
   pub fn new(peer_tracker_addr: Option<SocketAddr>) -> TrackerRequest {
      TrackerRequest {
         ip: None,
         port: 6881,
         uploaded: 0,
         downloaded: 0,
         left: None,
         event: Event::Stopped,
         peer_tracker_addr: peer_tracker_addr
            .unwrap_or(SocketAddr::from_str("0.0.0.0:6881").unwrap()),
      }
   }
}

/// Struct for handling tracker over HTTP
#[derive(Debug, Deserialize, Serialize)]
pub struct HttpTracker {
   uri: String,
   peer_id: String,
   info_hash: InfoHash,
   params: TrackerRequest,
}

impl HttpTracker {
   #[instrument(skip(info_hash), fields(uri = %uri))]
   pub fn new(
      uri: String,
      info_hash: InfoHash,
      peer_tracker_addr: Option<SocketAddr>,
   ) -> HttpTracker {
      let peer_id = Alphanumeric.sample_string(&mut rand::rng(), 20);
      debug!(peer_id = %peer_id, "Generated peer ID");

      HttpTracker {
         uri,
         peer_id,
         params: TrackerRequest::new(peer_tracker_addr),
         info_hash,
      }
   }
}

fn urlencode(t: &[u8; 20]) -> String {
   let mut encoded = String::with_capacity(3 * t.len());

   for &byte in t {
      encoded.push('%');

      let byte = hex::encode([byte]);
      encoded.push_str(&byte);
   }

   encoded
}

/// Fetches peers from tracker over HTTP and returns a stream of [PeerAddr](PeerAddr)
#[async_trait]
impl TrackerTrait for HttpTracker {
   #[instrument(skip(self))]
   async fn stream_peers(&mut self) -> anyhow::Result<Vec<Peer>> {
      // Decode info_hash
      debug!("Decoding info hash");

      // Generate params + URL. Specifically using the compact format by adding "compact=1" to
      // params.
      debug!("Generating request parameters");
      let params = serde_qs::to_string(&self.params)
         .map_err(|e| HttpTrackerError::ParameterEncoding(e.to_string()))?;

      let info_hash_encoded = urlencode(self.info_hash.as_bytes());
      trace!(encoded_hash = %info_hash_encoded, "URL-encoded info hash");

      let uri_params = format!(
         "{}&info_hash={}&peer_id={}",
         params, info_hash_encoded, &self.peer_id
      );

      let uri = format!("{}?{}", self.uri, &uri_params);
      debug!(request_uri = %uri, "Generated tracker request URI");

      // Make request
      info!("Sending HTTP request to tracker");
      let response = reqwest::get(&uri)
         .await
         .map_err(|e| {
            error!(error = %e, "HTTP request to tracker failed");
            HttpTrackerError::Request(e)
         })?
         .bytes()
         .await
         .map_err(|e| {
            error!(error = %e, "Failed to read tracker response body");
            HttpTrackerError::Request(e)
         })?;

      debug!(response_size = response.len(), "Received tracker response");

      let response: TrackerResponse = serde_bencode::from_bytes(&response).map_err(|e| {
         error!(error = %e, "Failed to decode bencode response");
         HttpTrackerError::Tracker(TrackerError::BencodeError(e))
      })?;

      trace!("Successfully decoded bencode response");
      info!(
         peers_count = response.peers.len(),
         "Found peers from tracker"
      );

      Ok(response.peers)
   }
}

/// Serde related code. Used for deserializing response from HTTP request made in stream_peers
struct PeerVisitor;

impl Visitor<'_> for PeerVisitor {
   type Value = Vec<Peer>;

   fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
      formatter.write_str("a byte array containing peer information")
   }

   fn visit_bytes<E>(self, bytes: &[u8]) -> Result<Self::Value, E>
   where
      E: de::Error,
   {
      // Decodes response from stream_peers' HTTP request according to BEP 23's compact form: <https://www.bittorrent.org/beps/bep_0023.html>
      let mut peers = Vec::new();
      const PEER_SIZE: usize = 6; // 4 bytes IP + 2 bytes port

      if bytes.len() % PEER_SIZE != 0 {
         return Err(de::Error::custom(format!(
            "Peer data length ({}) is not a multiple of peer size ({})",
            bytes.len(),
            PEER_SIZE
         )));
      }

      for (i, chunk) in bytes.chunks(PEER_SIZE).enumerate() {
         if chunk.len() != PEER_SIZE {
            warn!(
               chunk_index = i,
               chunk_size = chunk.len(),
               "Invalid peer chunk length, skipping"
            );
            continue;
         }

         let ip = Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]);
         let port = u16::from_be_bytes([chunk[4], chunk[5]]);

         trace!(
            peer_index = i,
            ip = %ip,
            port = port,
            "Parsed peer address"
         );

         peers.push(Peer::from_ipv4(ip, port));
      }

      Ok(peers)
   }
}

/// Serde related code. Reference their documentation: <https://serde.rs/impl-deserialize.html>
fn deserialize_peers<'de, D>(deserializer: D) -> Result<Vec<Peer>, D::Error>
where
   D: serde::Deserializer<'de>,
{
   deserializer.deserialize_bytes(PeerVisitor)
}

#[cfg(test)]
mod tests {
   use tracing_test::traced_test;

   use crate::{
      parser::{MagnetUri, MetaInfo},
      tracker::TrackerTrait,
   };

   use super::HttpTracker;

   #[tokio::test]
   #[traced_test]
   async fn test_stream_peers_with_http_tracker() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/zenshuu.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();
      let metainfo = MagnetUri::parse(contents).await.unwrap();
      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let info_hash = magnet.info_hash();
            let announce_list = magnet.announce_list.unwrap();
            let announce_uri = announce_list[0].uri();
            let mut http_tracker = HttpTracker::new(announce_uri, info_hash.unwrap(), None);

            // Make request
            let res = HttpTracker::stream_peers(&mut http_tracker)
               .await
               .expect("Issue when unwrapping result of stream_peers");

            assert!(res[0].ip.is_ipv4());
         }
         _ => panic!("Expected Torrent"),
      }
   }
}
