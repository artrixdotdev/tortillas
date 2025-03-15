/// See https://www.bittorrent.org/beps/bep_0003.html
use std::net::Ipv4Addr;

use rand::{
   distr::{Alphanumeric, SampleString},
   random, RngCore,
};
use reqwest::Url;
use serde::{de, Deserialize, Serialize};

use super::{PeerAddr, TrackerTrait};

#[derive(Debug, Deserialize)]
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
struct HttpTracker {
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

/// Fetches peers from tracker over HTTP and returns a stream of [PeerAddr](PeerAddr)
impl TrackerTrait for HttpTracker {
   async fn stream_peers(
      &mut self,
      info_hash: String,
   ) -> anyhow::Result<impl tokio_stream::Stream<Item = PeerAddr>> {
      let url_params = format!(
         "{}{}",
         serde_qs::to_string(&self.params).unwrap(),
         serde_qs::to_string(&self.info_hash).unwrap()
      );
      let uri = format!("{}{}", self.uri, url_params);
      let response = reqwest::get(uri).await?.text().await?;
      let response = serde_bencode::from_str(&response)?;
      panic!("{}", response);
      Ok()
   }
}

fn deserialize_peers<'de, D>(deserializer: D) -> anyhow::Result<Vec<PeerAddr>, D::Error>
where
   D: serde::Deserializer<'de>,
{
   let bytes = Vec::<u8>::deserialize(deserializer).expect("Invalid bytes");

   let mut peers = Vec::new();
   for chunk in bytes.chunks(6) {
      if chunk.len() != 6 {
         return Err(de::Error::custom("Invalid peer chunk length"));
      }
      let ip = Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]);

      let port = u16::from_be_bytes([chunk[4], chunk[5]]);
      peers.push(PeerAddr { ip, port });
   }

   Ok(peers)
}

#[cfg(test)]
mod tests {
   use crate::{
      parser::{MagnetUri, MetaInfo},
      tracker::TrackerTrait,
   };

   use super::{HttpTracker, TrackerRequest};

   #[tokio::test]
   async fn test_stream_peers_with_http_tracker() {
      let path = std::env::current_dir()
         .unwrap()
         .join("tests/magneturis/big-buck-bunny.txt");
      let contents = tokio::fs::read_to_string(path).await.unwrap();
      let metainfo = MagnetUri::parse(contents).await.unwrap();
      match metainfo {
         MetaInfo::MagnetUri(magnet) => {
            let announce_list = magnet.announce_list.unwrap();
            let announce_uri = announce_list[0].uri();
            let info_hash = magnet.info_hash;
            let mut http_tracker = HttpTracker::new(announce_uri, info_hash);
            HttpTracker::stream_peers(&mut http_tracker, "none".into())
               .await
               .unwrap();
         }
         _ => panic!("Expected Torrent"),
      }
   }
}
