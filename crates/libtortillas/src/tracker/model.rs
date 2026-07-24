use std::{fmt, net::SocketAddr};

use anyhow::Result;
use async_trait::async_trait;
use num_enum::TryFromPrimitive;
use serde::{
   Deserialize, Serialize,
   de::{self, Visitor},
};
use serde_repr::{Deserialize_repr, Serialize_repr};

use super::{
   TrackerStats,
   http::HttpTracker,
   udp::{UdpServer, UdpTracker},
};
use crate::{
   hashes::InfoHash,
   peer::{Peer, PeerId},
   settings::TrackerSettings,
};

/// An Announce URI from a torrent file or magnet URI.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize)]
#[serde(untagged)]
pub enum Tracker {
   /// HTTP Spec: <https://www.bittorrent.org/beps/bep_0003.html>
   Http(String),
   /// UDP Spec: <https://www.bittorrent.org/beps/bep_0015.html>
   Udp(String),
   Websocket(String),
}

impl Tracker {
   /// Creates a new tracker instance based on the tracker type.
   #[deprecated(note = "Use `to_instance` instead")]
   pub async fn to_base(
      &self, info_hash: InfoHash, peer_id: PeerId, port: u16, server: UdpServer,
   ) -> Result<Box<dyn TrackerBase>> {
      self
         .to_base_with_settings(info_hash, peer_id, port, server, TrackerSettings::default())
         .await
   }

   pub async fn to_base_with_settings(
      &self, info_hash: InfoHash, peer_id: PeerId, port: u16, server: UdpServer,
      settings: TrackerSettings,
   ) -> Result<Box<dyn TrackerBase>> {
      let socket_addr = SocketAddr::from(([0, 0, 0, 0], port));
      match self {
         Self::Http(uri) => {
            let tracker = HttpTracker::new_with_settings(
               uri.clone(),
               info_hash,
               Some(peer_id),
               Some(socket_addr),
               settings,
            );
            Ok(Box::new(tracker))
         }
         Self::Udp(uri) => {
            let tracker = UdpTracker::new_with_settings(
               uri.clone(),
               Some(server),
               info_hash,
               (peer_id, socket_addr),
               settings,
            )
            .await?;
            Ok(Box::new(tracker))
         }
         Self::Websocket(uri) => anyhow::bail!("websocket trackers are not supported: {uri}"),
      }
   }

   pub async fn to_instance(
      &self, info_hash: InfoHash, peer_id: PeerId, port: u16, server: UdpServer,
   ) -> Result<TrackerInstance> {
      self
         .to_instance_with_settings(info_hash, peer_id, port, server, TrackerSettings::default())
         .await
   }

   pub async fn to_instance_with_settings(
      &self, info_hash: InfoHash, peer_id: PeerId, port: u16, server: UdpServer,
      settings: TrackerSettings,
   ) -> Result<TrackerInstance> {
      let socket_addr = SocketAddr::from(([0, 0, 0, 0], port));
      match self {
         Self::Http(uri) => {
            let tracker = HttpTracker::new_with_settings(
               uri.clone(),
               info_hash,
               Some(peer_id),
               Some(socket_addr),
               settings,
            );
            Ok(TrackerInstance::Http(tracker))
         }
         Self::Udp(uri) => {
            let tracker = UdpTracker::new_with_settings(
               uri.clone(),
               Some(server),
               info_hash,
               (peer_id, socket_addr),
               settings,
            )
            .await?;
            Ok(TrackerInstance::Udp(tracker))
         }
         Self::Websocket(uri) => anyhow::bail!("websocket trackers are not supported: {uri}"),
      }
   }

   pub fn uri(&self) -> String {
      match self {
         Tracker::Http(uri) | Tracker::Udp(uri) | Tracker::Websocket(uri) => uri.clone(),
      }
   }

   /// Returns a credential-free endpoint label for frontend events.
   pub(crate) fn frontend_endpoint(&self) -> String {
      let uri = self.uri();
      let Ok(mut url) = reqwest::Url::parse(&uri) else {
         return self.scheme().to_string();
      };

      let _ = url.set_username("");
      let _ = url.set_password(None);
      url.set_path("");
      url.set_query(None);
      url.set_fragment(None);
      url.to_string()
   }

   fn scheme(&self) -> &'static str {
      match self {
         Self::Http(_) => "http",
         Self::Udp(_) => "udp",
         Self::Websocket(_) => "websocket",
      }
   }
}

/// Trait for HTTP and UDP trackers.
#[async_trait]
pub trait TrackerBase: Send + Sync {
   async fn initialize(&self) -> Result<()>;
   async fn announce(&self) -> Result<Vec<Peer>>;
   async fn update(&self, update: TrackerUpdate) -> Result<()>;
   fn stats(&self) -> TrackerStats;
   fn interval(&self) -> usize;
   async fn stop(&self) -> Result<()>;
}

/// Enum for the different tracker variants that implement [`TrackerBase`].
#[derive(Clone)]
pub enum TrackerInstance {
   Udp(UdpTracker),
   Http(HttpTracker),
}

#[async_trait]
impl TrackerBase for TrackerInstance {
   async fn initialize(&self) -> Result<()> {
      match self {
         TrackerInstance::Udp(tracker) => tracker.initialize().await,
         TrackerInstance::Http(tracker) => tracker.initialize().await,
      }
   }

   async fn announce(&self) -> Result<Vec<Peer>> {
      match self {
         TrackerInstance::Udp(tracker) => tracker.announce().await,
         TrackerInstance::Http(tracker) => tracker.announce().await,
      }
   }

   async fn update(&self, update: TrackerUpdate) -> Result<()> {
      match self {
         TrackerInstance::Udp(tracker) => tracker.update(update).await,
         TrackerInstance::Http(tracker) => tracker.update(update).await,
      }
   }

   async fn stop(&self) -> Result<()> {
      match self {
         TrackerInstance::Udp(tracker) => tracker.stop().await,
         TrackerInstance::Http(tracker) => tracker.stop().await,
      }
   }

   fn interval(&self) -> usize {
      match self {
         TrackerInstance::Udp(tracker) => tracker.interval() as usize,
         TrackerInstance::Http(tracker) => tracker.interval(),
      }
   }

   fn stats(&self) -> TrackerStats {
      match self {
         TrackerInstance::Udp(tracker) => tracker.stats(),
         TrackerInstance::Http(tracker) => tracker.stats(),
      }
   }
}

/// Updates the tracker's announce fields.
#[derive(Debug, Clone)]
pub enum TrackerUpdate {
   Uploaded(usize),
   Downloaded(usize),
   Left(usize),
   Event(Event),
}

/// Event. See <https://www.bittorrent.org/beps/bep_0003.html> @ trackers.
#[derive(
   Debug,
   Default,
   Serialize_repr,
   Deserialize_repr,
   Clone,
   Copy,
   PartialEq,
   Eq,
   TryFromPrimitive
)]
#[repr(u32)]
pub enum Event {
   #[default]
   Empty = 0,
   Started = 1,
   Completed = 2,
   Stopped = 3,
}

struct TrackerVisitor;

impl<'de> Deserialize<'de> for Tracker {
   fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
   where
      D: serde::Deserializer<'de>,
   {
      deserializer.deserialize_string(TrackerVisitor)
   }
}

impl Visitor<'_> for TrackerVisitor {
   type Value = Tracker;

   fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
      formatter.write_str("a tracker URI string")
   }

   fn visit_string<E>(self, s: String) -> Result<Self::Value, E>
   where
      E: de::Error,
   {
      tracker_from_uri(s).map_err(E::custom)
   }

   fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
   where
      E: de::Error,
   {
      tracker_from_uri(s.to_string()).map_err(E::custom)
   }
}

fn tracker_from_uri(uri: String) -> Result<Tracker, String> {
   let Some((scheme, _)) = uri.split_once("://") else {
      return Err(format!("tracker URI is missing a scheme: {uri}"));
   };

   match scheme {
      "http" | "https" => Ok(Tracker::Http(uri)),
      "udp" => Ok(Tracker::Udp(uri)),
      "ws" | "wss" => Ok(Tracker::Websocket(uri)),
      _ => Err(format!("unsupported tracker scheme: {scheme}")),
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn frontend_endpoint_removes_tracker_credentials_and_paths() {
      let tracker = Tracker::Http(
         "https://alice:password@tracker.example/secret-passkey/announce?token=secret".to_string(),
      );

      let endpoint = tracker.frontend_endpoint();

      assert_eq!(endpoint, "https://tracker.example/");
      assert!(!endpoint.contains("alice"));
      assert!(!endpoint.contains("password"));
      assert!(!endpoint.contains("passkey"));
      assert!(!endpoint.contains("token"));
   }

   #[test]
   fn invalid_tracker_endpoint_falls_back_to_protocol_only() {
      let tracker = Tracker::Udp("udp://[invalid".to_string());

      assert_eq!(tracker.frontend_endpoint(), "udp");
   }
}
