use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use anyhow::{Result, bail};

use super::NodeId;

const COMPACT_NODE_LEN: usize = 26;
const COMPACT_PEER_LEN: usize = 6;

/// A DHT routing contact and its UDP endpoint.
///
/// BEP 5 uses the compact form to keep UDP replies small enough to avoid
/// fragmentation. See [Compact node info].
///
/// [Compact node info]: https://www.bittorrent.org/beps/bep_0005.html#compact-node-info
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Contact {
   pub id: NodeId,
   pub addr: SocketAddr,
}

impl Contact {
   pub const fn new(id: NodeId, addr: SocketAddr) -> Self {
      Self { id, addr }
   }
}

/// Encodes IPv4 DHT contacts using BEP 5's 26-byte compact node format.
pub fn encode_nodes(contacts: impl IntoIterator<Item = Contact>) -> Vec<u8> {
   let mut bytes = Vec::new();
   for contact in contacts {
      let IpAddr::V4(ip) = contact.addr.ip() else {
         continue;
      };
      bytes.extend_from_slice(contact.id.as_bytes());
      bytes.extend_from_slice(&ip.octets());
      bytes.extend_from_slice(&contact.addr.port().to_be_bytes());
   }
   bytes
}

/// Decodes BEP 5 compact IPv4 DHT contacts.
pub fn decode_nodes(bytes: &[u8]) -> Result<Vec<Contact>> {
   if !bytes.len().is_multiple_of(COMPACT_NODE_LEN) {
      bail!("compact DHT nodes must be a multiple of {COMPACT_NODE_LEN} bytes");
   }

   bytes
      .chunks_exact(COMPACT_NODE_LEN)
      .map(|node| {
         let id = NodeId::try_from(&node[..20])?;
         let ip = Ipv4Addr::new(node[20], node[21], node[22], node[23]);
         let port = u16::from_be_bytes([node[24], node[25]]);
         Ok(Contact::new(id, SocketAddr::from((ip, port))))
      })
      .collect()
}

/// Encodes IPv4 peers using BEP 5's six-byte compact peer format.
pub fn encode_peers(peers: impl IntoIterator<Item = SocketAddr>) -> Vec<Vec<u8>> {
   peers
      .into_iter()
      .filter_map(|peer| {
         let IpAddr::V4(ip) = peer.ip() else {
            return None;
         };
         let mut bytes = Vec::with_capacity(COMPACT_PEER_LEN);
         bytes.extend_from_slice(&ip.octets());
         bytes.extend_from_slice(&peer.port().to_be_bytes());
         Some(bytes)
      })
      .collect()
}

/// Decodes one BEP 5 compact IPv4 peer endpoint.
pub fn decode_peers(values: impl IntoIterator<Item = Vec<u8>>) -> Result<Vec<SocketAddr>> {
   values
      .into_iter()
      .map(|peer| {
         if peer.len() != COMPACT_PEER_LEN {
            bail!("compact DHT peers must contain exactly {COMPACT_PEER_LEN} bytes");
         }
         let ip = Ipv4Addr::new(peer[0], peer[1], peer[2], peer[3]);
         let port = u16::from_be_bytes([peer[4], peer[5]]);
         Ok(SocketAddr::from((ip, port)))
      })
      .collect()
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn compact_nodes_when_ipv4_contacts_are_encoded_then_round_trips() {
      let contacts = vec![
         Contact::new(
            NodeId::from_bytes([1; 20]),
            "127.0.0.1:6881".parse().unwrap(),
         ),
         Contact::new(
            NodeId::from_bytes([2; 20]),
            "10.0.0.2:51413".parse().unwrap(),
         ),
      ];

      assert_eq!(
         decode_nodes(&encode_nodes(contacts.clone())).unwrap(),
         contacts
      );
   }

   #[test]
   fn compact_nodes_when_payload_is_truncated_then_rejects_value() {
      assert!(decode_nodes(&[0; COMPACT_NODE_LEN - 1]).is_err());
   }

   #[test]
   fn compact_peers_when_ipv4_endpoints_are_encoded_then_round_trips() {
      let peers = vec![
         "192.0.2.1:6881".parse().unwrap(),
         "198.51.100.2:1".parse().unwrap(),
      ];

      assert_eq!(decode_peers(encode_peers(peers.clone())).unwrap(), peers);
   }
}
