use serde::{Deserialize, Serialize};
mod file;

pub use file::*;
/// An Announce URI from a torrent file or magnet URI.
/// https://www.bittorrent.org/beps/bep_0012.html
/// Example: udp://tracker.opentrackr.org:1337/announce
#[derive(Debug, Deserialize)]
pub struct AnnounceUri(String);

#[derive(Debug, Deserialize)]
pub struct Metainfo {
   /// The primary announce URI for the torrent.
   announce: AnnounceUri,
   /// Secondary announce URIs for different trackers, and protocols. Also can be used as a backup
   #[serde(rename(deserialize = "announce-list"))]
   announce_list: Option<Vec<Vec<AnnounceUri>>>, // Note: This is a list of lists
   comment: Option<String>,
   #[serde(rename(deserialize = "created by"))]
   created_by: Option<String>,
   #[serde(rename(deserialize = "creation date"))]
   creation_date: Option<i64>, // Typically stored as unix timestamp
   encoding: Option<String>,
   info: Info,
   url_list: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Info {
   #[serde(default)] // This will use Vec::default() if missing
   files: Vec<InfoFile>,
   name: String,
   #[serde(rename = "piece length")]
   piece_length: u64,
   /// Binary string of concatenated 20-byte SHA-1 hash values
   /// TODO: Convert this to a custom type
   pieces: String,
   // For single-file torrents, length may be here instead of in files
   #[serde(skip_serializing_if = "Option::is_none")]
   length: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InfoFile {
   length: u64,
   path: Vec<String>,
}
