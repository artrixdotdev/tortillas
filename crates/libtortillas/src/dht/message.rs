use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use super::NodeId;
#[cfg(test)]
use super::{DHT_ID_LEN, compact::COMPACT_NODE_LEN};

const KRPC_PROTOCOL_ERROR: i64 = 203;

/// A decoded BEP 5 message using the bencoded [KRPC protocol] envelope.
///
/// [KRPC protocol]: https://www.bittorrent.org/beps/bep_0005.html#krpc-protocol
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
   Query {
      transaction_id: Vec<u8>,
      query: Query,
   },
   Response {
      transaction_id: Vec<u8>,
      response: Response,
   },
   Error {
      transaction_id: Vec<u8>,
      error: DhtError,
   },
}

/// Queries defined by BEP 5.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Query {
   Ping {
      id: NodeId,
   },
   FindNode {
      id: NodeId,
      target: NodeId,
   },
   GetPeers {
      id: NodeId,
      info_hash: NodeId,
   },
   AnnouncePeer {
      id: NodeId,
      info_hash: NodeId,
      port: u16,
      token: Vec<u8>,
      implied_port: bool,
   },
}

impl Query {
   pub const fn id(&self) -> NodeId {
      match self {
         Self::Ping { id }
         | Self::FindNode { id, .. }
         | Self::GetPeers { id, .. }
         | Self::AnnouncePeer { id, .. } => *id,
      }
   }
}

/// A BEP 5 response dictionary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Response {
   pub id: NodeId,
   pub nodes: Option<Vec<u8>>,
   pub token: Option<Vec<u8>>,
   pub values: Option<Vec<Vec<u8>>>,
}

impl Response {
   pub const fn pong(id: NodeId) -> Self {
      Self {
         id,
         nodes: None,
         token: None,
         values: None,
      }
   }
}

/// A KRPC protocol error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DhtError {
   pub code: i64,
   pub message: String,
}

impl DhtError {
   pub fn protocol(message: impl Into<String>) -> Self {
      Self {
         code: KRPC_PROTOCOL_ERROR,
         message: message.into(),
      }
   }
}

impl Message {
   pub fn transaction_id(&self) -> &[u8] {
      match self {
         Self::Query { transaction_id, .. }
         | Self::Response { transaction_id, .. }
         | Self::Error { transaction_id, .. } => transaction_id,
      }
   }

   pub fn encode(&self) -> Result<Vec<u8>> {
      serde_bencode::to_bytes(&WireMessage::from(self)).context("failed to encode DHT message")
   }

   pub fn decode(bytes: &[u8]) -> Result<Self> {
      let wire: WireMessage =
         serde_bencode::from_bytes(bytes).context("failed to decode DHT message")?;
      wire.try_into()
   }
}

#[derive(Deserialize, Serialize)]
struct WireMessage {
   #[serde(rename = "t")]
   transaction_id: ByteBuf,
   #[serde(rename = "y")]
   kind: String,
   #[serde(rename = "q", skip_serializing_if = "Option::is_none")]
   query: Option<String>,
   #[serde(rename = "a", skip_serializing_if = "Option::is_none")]
   arguments: Option<WireArguments>,
   #[serde(rename = "r", skip_serializing_if = "Option::is_none")]
   response: Option<WireResponse>,
   #[serde(rename = "e", skip_serializing_if = "Option::is_none")]
   error: Option<(i64, String)>,
}

#[derive(Default, Deserialize, Serialize)]
struct WireArguments {
   id: ByteBuf,
   #[serde(skip_serializing_if = "Option::is_none")]
   target: Option<ByteBuf>,
   #[serde(rename = "info_hash", skip_serializing_if = "Option::is_none")]
   info_hash: Option<ByteBuf>,
   #[serde(skip_serializing_if = "Option::is_none")]
   port: Option<u16>,
   #[serde(skip_serializing_if = "Option::is_none")]
   token: Option<ByteBuf>,
   #[serde(skip_serializing_if = "Option::is_none")]
   implied_port: Option<u8>,
}

#[derive(Deserialize, Serialize)]
struct WireResponse {
   id: ByteBuf,
   #[serde(skip_serializing_if = "Option::is_none")]
   nodes: Option<ByteBuf>,
   #[serde(skip_serializing_if = "Option::is_none")]
   token: Option<ByteBuf>,
   #[serde(skip_serializing_if = "Option::is_none")]
   values: Option<Vec<ByteBuf>>,
}

impl From<&Message> for WireMessage {
   fn from(message: &Message) -> Self {
      match message {
         Message::Query {
            transaction_id,
            query,
         } => {
            let (query, arguments) = match query {
               Query::Ping { id } => ("ping", WireArguments::with_id(*id)),
               Query::FindNode { id, target } => {
                  let mut arguments = WireArguments::with_id(*id);
                  arguments.target = Some(ByteBuf::from(target.as_bytes().to_vec()));
                  ("find_node", arguments)
               }
               Query::GetPeers { id, info_hash } => {
                  let mut arguments = WireArguments::with_id(*id);
                  arguments.info_hash = Some(ByteBuf::from(info_hash.as_bytes().to_vec()));
                  ("get_peers", arguments)
               }
               Query::AnnouncePeer {
                  id,
                  info_hash,
                  port,
                  token,
                  implied_port,
               } => {
                  let mut arguments = WireArguments::with_id(*id);
                  arguments.info_hash = Some(ByteBuf::from(info_hash.as_bytes().to_vec()));
                  arguments.port = Some(*port);
                  arguments.token = Some(ByteBuf::from(token.clone()));
                  arguments.implied_port = Some(u8::from(*implied_port));
                  ("announce_peer", arguments)
               }
            };
            Self {
               transaction_id: ByteBuf::from(transaction_id.clone()),
               kind: "q".to_string(),
               query: Some(query.to_string()),
               arguments: Some(arguments),
               response: None,
               error: None,
            }
         }
         Message::Response {
            transaction_id,
            response,
         } => Self {
            transaction_id: ByteBuf::from(transaction_id.clone()),
            kind: "r".to_string(),
            query: None,
            arguments: None,
            response: Some(WireResponse::from(response)),
            error: None,
         },
         Message::Error {
            transaction_id,
            error,
         } => Self {
            transaction_id: ByteBuf::from(transaction_id.clone()),
            kind: "e".to_string(),
            query: None,
            arguments: None,
            response: None,
            error: Some((error.code, error.message.clone())),
         },
      }
   }
}

impl WireArguments {
   fn with_id(id: NodeId) -> Self {
      Self {
         id: ByteBuf::from(id.as_bytes().to_vec()),
         ..Self::default()
      }
   }
}

impl From<&Response> for WireResponse {
   fn from(response: &Response) -> Self {
      Self {
         id: ByteBuf::from(response.id.as_bytes().to_vec()),
         nodes: response.nodes.clone().map(ByteBuf::from),
         token: response.token.clone().map(ByteBuf::from),
         values: response
            .values
            .clone()
            .map(|values| values.into_iter().map(ByteBuf::from).collect()),
      }
   }
}

impl TryFrom<WireMessage> for Message {
   type Error = anyhow::Error;

   fn try_from(wire: WireMessage) -> Result<Self> {
      let transaction_id = wire.transaction_id.into_vec();
      match wire.kind.as_str() {
         "q" => {
            let name = wire
               .query
               .ok_or_else(|| anyhow!("DHT query is missing q"))?;
            let arguments = wire
               .arguments
               .ok_or_else(|| anyhow!("DHT query is missing a"))?;
            let id = NodeId::try_from(arguments.id.as_ref())?;
            let query = match name.as_str() {
               "ping" => Query::Ping { id },
               "find_node" => Query::FindNode {
                  id,
                  target: node_argument(arguments.target, "target")?,
               },
               "get_peers" => Query::GetPeers {
                  id,
                  info_hash: node_argument(arguments.info_hash, "info_hash")?,
               },
               "announce_peer" => Query::AnnouncePeer {
                  id,
                  info_hash: node_argument(arguments.info_hash, "info_hash")?,
                  port: arguments
                     .port
                     .ok_or_else(|| anyhow!("announce_peer is missing port"))?,
                  token: arguments
                     .token
                     .ok_or_else(|| anyhow!("announce_peer is missing token"))?
                     .into_vec(),
                  implied_port: arguments.implied_port.unwrap_or_default() != 0,
               },
               _ => bail!("unknown DHT query {name}"),
            };
            Ok(Self::Query {
               transaction_id,
               query,
            })
         }
         "r" => {
            let response = wire
               .response
               .ok_or_else(|| anyhow!("DHT response is missing r"))?;
            Ok(Self::Response {
               transaction_id,
               response: Response {
                  id: NodeId::try_from(response.id.as_ref())?,
                  nodes: response.nodes.map(ByteBuf::into_vec),
                  token: response.token.map(ByteBuf::into_vec),
                  values: response
                     .values
                     .map(|values| values.into_iter().map(ByteBuf::into_vec).collect()),
               },
            })
         }
         "e" => {
            let (code, message) = wire
               .error
               .ok_or_else(|| anyhow!("DHT error is missing e"))?;
            Ok(Self::Error {
               transaction_id,
               error: DhtError { code, message },
            })
         }
         kind => bail!("unknown DHT message type {kind}"),
      }
   }
}

fn node_argument(value: Option<ByteBuf>, name: &str) -> Result<NodeId> {
   NodeId::try_from(
      value
         .ok_or_else(|| anyhow!("DHT query is missing {name}"))?
         .as_ref(),
   )
}

#[cfg(test)]
mod tests {
   use super::*;

   fn id(value: u8) -> NodeId {
      NodeId::from_bytes([value; DHT_ID_LEN])
   }

   #[test]
   fn message_when_ping_is_encoded_then_round_trips() {
      let message = Message::Query {
         transaction_id: b"aa".to_vec(),
         query: Query::Ping { id: id(1) },
      };

      assert_eq!(
         Message::decode(&message.encode().unwrap()).unwrap(),
         message
      );
   }

   #[test]
   fn message_when_get_peers_response_is_encoded_then_round_trips() {
      let message = Message::Response {
         transaction_id: b"gp".to_vec(),
         response: Response {
            id: id(2),
            nodes: Some(vec![3; COMPACT_NODE_LEN]),
            token: Some(vec![4; 8]),
            values: Some(vec![vec![127, 0, 0, 1, 0x1a, 0xe1]]),
         },
      };

      assert_eq!(
         Message::decode(&message.encode().unwrap()).unwrap(),
         message
      );
   }

   #[test]
   fn message_when_announce_is_encoded_then_round_trips() {
      let message = Message::Query {
         transaction_id: b"ap".to_vec(),
         query: Query::AnnouncePeer {
            id: id(1),
            info_hash: id(9),
            port: 6881,
            token: vec![7; 8],
            implied_port: true,
         },
      };

      assert_eq!(
         Message::decode(&message.encode().unwrap()).unwrap(),
         message
      );
   }
}
