/// See https://www.bittorrent.org/beps/bep_0003.html
use std::net::Ipv4Addr;

use anyhow::Ok;
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

#[derive(Debug, Deserialize, Serialize)]
pub enum Event {
   Started,
   Completed,
   Stopped,
   Empty,
}

#[derive(Debug, Deserialize, Serialize)]
struct TrackerRequest {
   peer_id: String,
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
         peer_id: Alphanumeric.sample_string(&mut rand::rng(), 20),
         ip: None,
         port: 6881,
         uploaded: 0,
         downloaded: 0,
         left: None,
         event: Event::Stopped,
      }
   }
}

#[derive(Debug, Deserialize, Serialize)]
struct HttpTracker {
   uri: String,
   peer_id: String,
   params: TrackerRequest,
}

impl HttpTracker {
   pub fn new(uri: String) -> HttpTracker {
      let tracker_request = TrackerRequest::new();
      HttpTracker {
         uri,
         peer_id: tracker_request.peer_id.to_string(),
         params: tracker_request,
      }
   }
}

impl TrackerTrait for HttpTracker {
   async fn stream_peers(
      &mut self,
      info_hash: String,
   ) -> anyhow::Result<impl tokio_stream::Stream<Item = PeerAddr>> {
      let url_params = serde_qs::to_string(&self).unwrap();
      let uri = format!("{}{}", self.uri, url_params);
      let response = reqwest::get(uri).await?.text().await?;
      let response = serde_bencode::from_str(&response)?;
      panic!("{}", response);
      Ok()
   }
}

fn deserialize_peers<'de, D>(deserializer: D) -> Result<Vec<PeerAddr>, D::Error>
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
