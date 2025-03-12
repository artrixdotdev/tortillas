/// 2 bytes for source port, 2 bytes for destination port, 2 bytes for length, and 2 bytes for checksum
/// https://en.wikipedia.org/wiki/User_Datagram_Protocol
/// https://www.bittorrent.org/beps/bep_0015.html
use anyhow::Result;
use tokio::net::UdpSocket;

pub async fn get(uri: String) -> Result<()> {
   let sock = UdpSocket::bind(uri).await?;
}
