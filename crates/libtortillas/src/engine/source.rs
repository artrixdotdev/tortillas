use std::path::PathBuf;

use bytes::Bytes;
use reqwest::Url;
use tracing::error;

use crate::{
   errors::EngineError,
   metainfo::{MagnetUri, MetaInfo, TorrentFile},
};

/// Explicit source type for adding a torrent to an [`Engine`](super::Engine).
///
/// Frontends should map user input into one of these variants before calling
/// [`Engine::add_torrent`](super::Engine::add_torrent). That keeps UI intent
/// separate from parsing and avoids guessing whether a string is a URL, path,
/// or magnet URI.
///
/// ```
/// use std::path::PathBuf;
///
/// use libtortillas::prelude::TorrentSource;
///
/// enum UserTorrentInput {
///    PastedMagnet(String),
///    PickedTorrentFile(PathBuf),
///    DroppedTorrentFile(Vec<u8>),
///    RemoteTorrentUrl(String),
/// }
///
/// fn source_for(input: UserTorrentInput) -> TorrentSource {
///    match input {
///       UserTorrentInput::PastedMagnet(uri) => TorrentSource::magnet(uri),
///       UserTorrentInput::PickedTorrentFile(path) => TorrentSource::torrent_file_path(path),
///       UserTorrentInput::DroppedTorrentFile(bytes) => TorrentSource::torrent_file_bytes(bytes),
///       UserTorrentInput::RemoteTorrentUrl(url) => TorrentSource::remote_torrent_url(url),
///    }
/// }
/// ```
#[derive(Debug, Clone)]
pub enum TorrentSource {
   /// A `magnet:` URI entered or pasted by the user.
   Magnet(String),
   /// A local `.torrent` file path selected by the user.
   TorrentFilePath(PathBuf),
   /// Raw bytes of a `.torrent` file already loaded by the frontend.
   TorrentFileBytes(Bytes),
   /// An HTTP or HTTPS URL pointing to a remote `.torrent` file.
   RemoteTorrentUrl(String),
}

impl TorrentSource {
   /// Creates a source from a `magnet:` URI.
   #[must_use]
   pub fn magnet(uri: impl Into<String>) -> Self {
      Self::Magnet(uri.into())
   }

   /// Creates a source from a local `.torrent` file path.
   #[must_use]
   pub fn torrent_file_path(path: impl Into<PathBuf>) -> Self {
      Self::TorrentFilePath(path.into())
   }

   /// Creates a source from already-loaded `.torrent` file bytes.
   #[must_use]
   pub fn torrent_file_bytes(bytes: impl Into<Bytes>) -> Self {
      Self::TorrentFileBytes(bytes.into())
   }

   /// Creates a source from an HTTP or HTTPS URL to a remote `.torrent` file.
   #[must_use]
   pub fn remote_torrent_url(url: impl Into<String>) -> Self {
      Self::RemoteTorrentUrl(url.into())
   }

   pub(crate) async fn into_metainfo(self) -> Result<MetaInfo, EngineError> {
      match self {
         Self::Magnet(uri) => metainfo_from_magnet(uri),
         Self::TorrentFilePath(path) => metainfo_from_path(path).await,
         Self::TorrentFileBytes(bytes) => metainfo_from_bytes(&bytes),
         Self::RemoteTorrentUrl(url) => metainfo_from_remote_url(url).await,
      }
   }
}

fn metainfo_from_magnet(uri: String) -> Result<MetaInfo, EngineError> {
   if !uri.starts_with("magnet:") {
      return Err(EngineError::InvalidTorrentSource {
         source_type: "magnet URI",
         reason: "expected a source beginning with `magnet:`".to_string(),
      });
   }

   MagnetUri::parse(uri.clone()).map_err(|error| {
      error!(magnet_uri = uri, %error);
      EngineError::MetaInfoDeserializeError
   })
}

async fn metainfo_from_path(path: PathBuf) -> Result<MetaInfo, EngineError> {
   TorrentFile::read(path.clone()).await.map_err(|error| {
      error!(torrent_file_path = %path.display(), %error);
      EngineError::MetaInfoDeserializeError
   })
}

fn metainfo_from_bytes(bytes: &[u8]) -> Result<MetaInfo, EngineError> {
   TorrentFile::parse(bytes).map_err(|error| {
      error!(%error);
      EngineError::MetaInfoDeserializeError
   })
}

async fn metainfo_from_remote_url(url: String) -> Result<MetaInfo, EngineError> {
   validate_remote_torrent_url(&url)?;

   let torrent_file_bytes = reqwest::get(&url)
      .await
      .map_err(EngineError::MetaInfoFetchError)?
      .bytes()
      .await
      .map_err(EngineError::MetaInfoFetchError)?;

   metainfo_from_bytes(&torrent_file_bytes).inspect_err(|_| {
      error!(remote_url = url);
   })
}

fn validate_remote_torrent_url(url: &str) -> Result<(), EngineError> {
   let parsed_url = Url::parse(url).map_err(|error| EngineError::InvalidTorrentSource {
      source_type: "remote URL",
      reason: error.to_string(),
   })?;

   match parsed_url.scheme() {
      "http" | "https" => Ok(()),
      scheme => Err(EngineError::InvalidTorrentSource {
         source_type: "remote URL",
         reason: format!("expected http or https URL, got `{scheme}`"),
      }),
   }
}

#[cfg(test)]
mod tests {
   use tokio::fs;

   use super::*;
   use crate::{
      metainfo::MetaInfo,
      testing::{BIG_BUCK_BUNNY_INFO_HASH, BIG_BUCK_BUNNY_TORRENT_FILE, torrent_fixture_path},
   };

   #[tokio::test]
   async fn torrent_source_when_source_is_torrent_file_path_then_loads_metainfo() {
      let source =
         TorrentSource::torrent_file_path(torrent_fixture_path(BIG_BUCK_BUNNY_TORRENT_FILE));

      let metainfo = source.into_metainfo().await.unwrap();

      assert_info_hash(metainfo);
   }

   #[tokio::test]
   async fn torrent_source_when_source_is_torrent_file_bytes_then_loads_metainfo() {
      let bytes = fs::read(torrent_fixture_path(BIG_BUCK_BUNNY_TORRENT_FILE))
         .await
         .unwrap();
      let source = TorrentSource::torrent_file_bytes(bytes);

      let metainfo = source.into_metainfo().await.unwrap();

      assert_info_hash(metainfo);
   }

   #[tokio::test]
   async fn torrent_source_when_source_is_magnet_uri_then_loads_metainfo() {
      let source = TorrentSource::magnet(crate::testing::BIG_BUCK_BUNNY_MAGNET);

      let metainfo = source.into_metainfo().await.unwrap();

      assert_info_hash(metainfo);
   }

   #[tokio::test]
   async fn torrent_source_when_magnet_variant_has_path_then_returns_typed_error() {
      let source = TorrentSource::magnet("./tests/torrents/big-buck-bunny.torrent");

      let error = source.into_metainfo().await.unwrap_err();

      assert!(matches!(
         error,
         EngineError::InvalidTorrentSource {
            source_type: "magnet URI",
            ..
         }
      ));
   }

   #[tokio::test]
   async fn torrent_source_when_remote_url_is_not_http_then_returns_typed_error() {
      let source = TorrentSource::remote_torrent_url("file:///tmp/example.torrent");

      let error = source.into_metainfo().await.unwrap_err();

      assert!(matches!(
         error,
         EngineError::InvalidTorrentSource {
            source_type: "remote URL",
            ..
         }
      ));
   }

   fn assert_info_hash(metainfo: MetaInfo) {
      let info_hash = metainfo.info_hash().unwrap();

      assert_eq!(info_hash.to_hex(), BIG_BUCK_BUNNY_INFO_HASH);
   }
}
