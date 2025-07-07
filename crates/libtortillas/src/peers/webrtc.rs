use anyhow::Result;
use async_trait::async_trait;
use std::{collections::HashMap, net::IpAddr, str::FromStr, sync::Arc};
use tokio::sync::Mutex;
use webrtc::{
   api::{
      interceptor_registry::register_default_interceptors, media_engine::MediaEngine, APIBuilder,
   },
   data_channel::RTCDataChannel,
   ice_transport::ice_server::RTCIceServer,
   interceptor::registry::Registry,
   peer_connection::{
      configuration::RTCConfiguration, offer_answer_options::RTCOfferOptions, RTCPeerConnection,
   },
};

use crate::{
   errors::PeerTransportError,
   hashes::{Hash, InfoHash},
};

use super::{Peer, PeerKey, TransportProtocol};

#[derive(Clone)]
pub struct WebRTCProtocol {
   pub connection: Arc<RTCPeerConnection>,
   pub peers: HashMap<PeerKey, Arc<Mutex<(Peer, RTCDataChannel)>>>,
}

impl WebRTCProtocol {
   pub async fn new() -> Self {
      let mut m = MediaEngine::default();

      // Register default codecs
      m.register_default_codecs().unwrap();

      // Create a InterceptorRegistry. This is the user configurable RTP/RTCP Pipeline.
      // This provides NACKs, RTCP Reports and other features. If you use `webrtc.NewPeerConnection`
      // this is enabled by default. If you are manually managing You MUST create a InterceptorRegistry
      // for each PeerConnection.
      let mut registry = Registry::new();

      // Use the default set of Interceptors
      registry = register_default_interceptors(registry, &mut m).unwrap();

      // Create the API object with the MediaEngine
      let api = APIBuilder::new()
         .with_media_engine(m)
         .with_interceptor_registry(registry)
         .build();

      let config = RTCConfiguration {
         ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
         }],
         ..Default::default()
      };
      let peer_connection = Arc::new(api.new_peer_connection(config).await.unwrap());
      WebRTCProtocol {
         connection: peer_connection,
         peers: HashMap::new(),
      }
   }
}

#[async_trait]
#[allow(unused_variables)]
impl TransportProtocol for WebRTCProtocol {
   /// Some helpful information:
   /// <https://w3c.github.io/webrtc-pc/#dom-rtcdatachannelinit>
   /// <https://webrtc.org/getting-started/peer-connections-advanced>
   async fn connect_peer(
      &mut self,
      peer: &mut Peer,
      id: Arc<Hash<20>>,
      info_hash: Arc<InfoHash>,
   ) -> Result<PeerKey, PeerTransportError> {
      // let options = Some(RTCDataChannelInit {
      //    ordered: Some(true),
      //    ..Default::default()
      // });
      // let data_channel = self
      //    .connection
      //    .create_data_channel("data", options)
      //    .await
      //    .map_err(|e| error!("Failed to create data channel!"))
      //    .unwrap();

      // let offer_options = Some(RTCOfferOptions {
      //    voice_activity_detection: false,
      //    ice_restart: true,
      // });
      //
      // let sdp_description = self.connection.create_offer(offer_options).await.unwrap();
      // self
      //    .connection
      //    .set_local_description(sdp_description)
      //    .await
      //    .unwrap();
      Ok(PeerKey::new(IpAddr::from_str("192.168.1.1").unwrap(), 1234))
   }
   async fn send_data(&mut self, to: PeerKey, data: Vec<u8>) -> Result<(), PeerTransportError> {
      Ok(())
   }
   async fn receive_data(
      &mut self,
      info_hash: Arc<InfoHash>,
      id: Arc<Hash<20>>,
   ) -> Result<(PeerKey, Vec<u8>), PeerTransportError> {
      Ok((
         PeerKey::new(IpAddr::from_str("192.168.1.0").unwrap(), 1234),
         vec![0],
      ))
   }
   fn close_connection(&mut self, peer_key: PeerKey) -> Result<()> {
      Ok(())
   }
   fn is_peer_connected(&self, peer_key: PeerKey) -> bool {
      true
   }
   async fn get_connected_peer(&self, peer_key: PeerKey) -> Option<Peer> {
      Some(Peer::new(IpAddr::from_str("192.168.1.0").unwrap(), 1234))
   }
}
