/// See https://www.bittorrent.org/beps/bep_0003.html
use std::net::Ipv4Addr;

use anyhow::Result;
use rand::distr::{Alphanumeric, SampleString};
use serde::{
   Deserialize, Serialize,
   de::{self, Visitor},
};
use tracing::{Level, debug, error, event, span, trace, warn};

use super::{PeerAddr, TrackerTrait};

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // REMOVE SOON
pub struct TrackerResponse {
   pub interval: usize,
   #[serde(deserialize_with = "deserialize_peers")]
   pub peers: Vec<PeerAddr>,
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
}

impl TrackerRequest {
   pub fn new() -> TrackerRequest {
      TrackerRequest {
         ip: None,
         port: 6881,
         uploaded: 0,
         downloaded: 0,
         left: None,
         event: Event::Stopped,
      }
   }
}

/// Struct for handling tracker over HTTP
#[derive(Debug, Deserialize, Serialize)]
pub struct HttpTracker {
   uri: String,
   peer_id: String,
   info_hash: String,
   params: TrackerRequest,
}

impl HttpTracker {
   pub fn new(uri: String, info_hash: String) -> HttpTracker {
      HttpTracker {
         uri,
         peer_id: Alphanumeric.sample_string(&mut rand::rng(), 20),
         params: TrackerRequest::new(),
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
impl TrackerTrait for HttpTracker {
   async fn stream_peers(&mut self) -> Result<Vec<PeerAddr>> {
      // Decode info_hash
      let info_hash = hex::decode(
         self
            .info_hash
            .split("urn:btih:")
            .last()
            .expect("Error when unwrapping info_hash"),
      )
      .expect("Error when unwrapping info_hash")
      .try_into()
      .expect("Error when unwrapping info_hash");

      // Generate params + URL. Specifically using the compact format by adding "compact=1" to
      // params.
      let params = serde_qs::to_string(&self.params).expect("url-encode tracker parameters");
      let info_hash = urlencode(&info_hash);
      trace!("Created info hash: {}", info_hash);
      let uri_params = format!(
         "{}&info_hash={}&peer_id={}&compact=1",
         params, info_hash, &self.peer_id
      );

      let uri = format!("{}?{}", self.uri, &uri_params);
      trace!("Generated uri: {uri}");

      // Make request
      let response = reqwest::get(&uri).await?.bytes().await?;
      debug!("Made GET request to {}", &uri);
      let response: TrackerResponse = serde_bencode::from_bytes(&response)?;
      trace!("Decoded bencode result: {:?}", &response);

      Ok(response.peers)
   }
}

/// Serde related code. Used for deserializing response from HTTP request made in stream_peers
struct PeerVisitor;

impl Visitor<'_> for PeerVisitor {
   type Value = Vec<PeerAddr>;

   fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
      formatter.write_str("a byte array")
   }

   fn visit_bytes<E>(self, bytes: &[u8]) -> Result<Self::Value, E>
   where
      E: de::Error,
   {
      // Decodes response from stream_peers' HTTP request according to BEP 23's compact form: <https://www.bittorrent.org/beps/bep_0023.html>
      let mut peers = Vec::new();

      for chunk in bytes.chunks(6) {
         if chunk.len() != 6 {
            return Err(de::Error::custom("Invalid peer chunk length"));
         }
         let ip = Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]);

         let port = u16::from_be_bytes([chunk[4], chunk[5]]);
         peers.push(PeerAddr { ip, port });
         trace!("Added PeerAddr {ip}:{port} to peers vec");
      }

      Ok(peers)
   }
}

/// Serde related code. Reference their documentation: <https://serde.rs/impl-deserialize.html>
fn deserialize_peers<'de, D>(deserializer: D) -> Result<Vec<PeerAddr>, D::Error>
where
   D: serde::Deserializer<'de>,
{
   deserializer.deserialize_bytes(PeerVisitor)
}

#[cfg(test)]
mod tests {
   use crate::{
      parser::{MagnetUri, MetaInfo},
      tracker::TrackerTrait,
   };

   use super::HttpTracker;

   #[tokio::test]
   async fn test_stream_peers_with_http_tracker() {
      tracing_subscriber::fmt::init();
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/zenshuu.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();
      let metainfo = MagnetUri::parse(contents).await.unwrap();
      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let announce_list = magnet.announce_list.unwrap();
            let announce_uri = announce_list[0].uri();
            let info_hash = magnet.info_hash;
            let mut http_tracker = HttpTracker::new(announce_uri, info_hash);

            // Make request
            let res = HttpTracker::stream_peers(&mut http_tracker)
               .await
               .expect("Issue when unwrapping result of stream_peers");

            assert!(!res[0].ip.is_private());
         }
         _ => panic!("Expected Torrent"),
      }
   }
}
