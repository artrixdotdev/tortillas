mod actor;
use std::net::SocketAddr;

pub(crate) use actor::*;
use bon;
use kameo::{Actor, actor::ActorRef};
use tracing::error;

use crate::{
   errors::EngineError,
   metainfo::{MetaInfo, TorrentFile},
   torrent::Torrent,
};

pub struct Engine(ActorRef<EngineActor>);
#[bon::bon]
impl Engine {
   #[builder(on(SocketAddr, into))]
   fn new(
      /// The address to listen for TCP peers on.
      tcp_addr: Option<SocketAddr>,
      /// The address to listen for uTP peers on.
      utp_addr: Option<SocketAddr>,
      /// Address to connect to UDP [trackers](crate::tracker::Tracker).
      udp_addr: Option<SocketAddr>,
   ) -> Self {
      let addrs = (tcp_addr, utp_addr, udp_addr);

      let actor = EngineActor::spawn(addrs);

      Engine(actor)
   }

   /// Just a helper function so we don't have to write `&self.0` all the time.
   fn actor(&self) -> &ActorRef<EngineActor> {
      &self.0
   }

   /// Starts the torrenting process for a given torrent. This function
   /// automatically contacts trackers and connects to peers. The spawned
   /// [Torrent Actor](Torrent) will be controlled by the [Engine].
   ///
   /// This function accepts the following as input:
   /// - A remote URL to a torrent file over HTTP/HTTPS
   /// - The path, either absolute or relative, to a local torrent file
   /// - A magnet URI
   ///
   /// If the inputted value is a remote url to a torrent file, this function
   /// requests the bytes and deserializes them into a [TorrentFile]. If
   /// it isn't, we assume that it is either a magnet URI or a path to a
   /// torrent file, and pass the string to [MetaInfo::new].
   ///
   ///
   /// # Examples
   ///
   /// With a remote torrent file
   /// ```no_run
   /// use libtortillas::Engine;
   ///
   /// #[tokio::main]
   /// async fn main() {
   ///    let mut engine = Engine::new(None);
   ///    let torrent_link = "https://example.com/example.torrent";
   ///    let torrent = engine
   ///       .add_torrent(torrent_link)
   ///       .await
   ///       .expect("Failed to add torrent");
   ///
   ///    println!("Started Torrenting: {}", torrent.key());
   /// }
   /// ```
   ///
   /// With a magnet URI
   /// ```no_run
   /// use libtortillas::Engine;
   ///
   /// #[tokio::main]
   /// async fn main() {
   ///    let mut engine = Engine::new(None);
   ///    let magnet_uri = "magnet:?xt=?????";
   ///    let torrent = engine
   ///       .add_torrent(magnet_uri)
   ///       .await
   ///       .expect("Failed to add torrent");
   ///
   ///    println!("Started Torrenting: {}", torrent.key());
   /// }
   /// ```
   pub async fn add_torrent(&self, metainfo: impl ToString) -> Result<Torrent, EngineError> {
      let metainfo = metainfo.to_string();
      // File paths should either start with "/" or "./", and magnet URIs start
      // with "magnet:", so a check like this should be entirely appropriate.
      let metainfo = if metainfo.starts_with("http") {
         let torrent_file_bytes = reqwest::get(&metainfo)
            .await
            .map_err(EngineError::MetaInfoFetchError)?
            .bytes()
            .await
            .map_err(EngineError::MetaInfoFetchError)?;
         let torrent_file = TorrentFile::parse(&torrent_file_bytes);
         torrent_file.map_err(|_| {
            error!(remote_url = metainfo);
            EngineError::MetaInfoDeserializeError
         })?
      } else {
         MetaInfo::new(metainfo.clone()).await.map_err(|_| {
            error!(magnet_uri_or_file = metainfo);
            EngineError::MetaInfoDeserializeError
         })?
      };

      let info_hash = metainfo.info_hash().expect("Failed to fetch info hash");

      let torrent_ref = match self
         .actor()
         .ask(EngineRequest::Torrent(Box::new(metainfo)))
         .await
         .expect("Failed to add torrent")
      {
         EngineResponse::Torrent(torrent_ref) => torrent_ref,
         #[allow(unreachable_patterns)]
         _ => unreachable!(),
      };

      Ok(Torrent::new(info_hash, torrent_ref))
      // We don't need to assign link or insert the ref here because its already
      // done by the engine actor
   }
}
