//! Async BitTorrent library for downloading and seeding files.
//!
//! # Getting started
//!
//! A basic downloader only needs an [`engine::Engine`] and a
//! [`engine::TorrentSource`]. The live frontend API is optional.
//!
//! Add the library and its Tokio runtime to a binary crate:
//!
//! ```text
//! cargo add libtortillas
//! cargo add tokio --features full
//! ```
//!
//! The following complete program loads `example.torrent`, writes its payload
//! to `downloads/`, and continues downloading or seeding until Ctrl-C is
//! pressed:
//!
//! ```no_run
//! use libtortillas::prelude::{Engine, TorrentSource};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!    let engine = Engine::builder().output_path("downloads").build();
//!    let torrent = engine
//!       .add_torrent(TorrentSource::torrent_file_path("example.torrent"))
//!       .await?;
//!
//!    println!("torrenting {} — press Ctrl-C to stop", torrent.info_hash());
//!    tokio::signal::ctrl_c().await?;
//!
//!    engine.shutdown().await?;
//!    Ok(())
//! }
//! ```
//!
//! That is enough to start torrenting. By default, a newly added torrent starts
//! automatically after it discovers enough peers. The [`torrent::Torrent`]
//! returned by [`engine::Engine::add_torrent`] is a lightweight handle for
//! controlling that download.
//!
//! ## Torrent sources
//!
//! Use the constructor that matches the input your application already has:
//!
//! - [`engine::TorrentSource::torrent_file_path`] for a local `.torrent` file.
//! - [`engine::TorrentSource::magnet`] for a magnet URI.
//! - [`engine::TorrentSource::torrent_file_bytes`] for bytes already in memory.
//! - [`engine::TorrentSource::remote_torrent_url`] for an HTTP or HTTPS URL.
//!
//! Every source is passed to [`engine::Engine::add_torrent`] in the same way.
//! There is no frontend-specific setup.
//!
//! For example, downloading from a magnet link only changes the source:
//!
//! ```no_run
//! use libtortillas::prelude::{Engine, TorrentSource};
//!
//! async fn add_magnet(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
//!    let torrent = engine
//!       .add_torrent(TorrentSource::magnet(
//!          "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
//!       ))
//!       .await?;
//!
//!    println!("torrenting {}", torrent.info_hash());
//!    Ok(())
//! }
//! ```
//!
//! ## Basic control
//!
//! The returned [`torrent::Torrent`] can be
//! [`paused`](torrent::Torrent::pause),
//! [`resumed`](torrent::Torrent::resume), or inspected for its
//! [`state`](torrent::Torrent::state). An engine can manage multiple torrents;
//! call [`engine::Engine::remove_torrent`] to remove one and
//! [`engine::Engine::shutdown`] before exiting cleanly.
//!
//! ```no_run
//! use libtortillas::prelude::Torrent;
//!
//! async fn pause_and_resume(torrent: &Torrent) -> Result<(), Box<dyn std::error::Error>> {
//!    torrent.pause().await?;
//!    println!("state after pausing: {:?}", torrent.state().await?);
//!
//!    torrent.resume().await?;
//!    Ok(())
//! }
//! ```
//!
//! ## Multiple torrents
//!
//! One engine can download and seed many torrents:
//!
//! ```no_run
//! use libtortillas::prelude::{Engine, TorrentSource};
//!
//! async fn add_downloads(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
//!    let sources = [
//!       TorrentSource::torrent_file_path("first.torrent"),
//!       TorrentSource::torrent_file_path("second.torrent"),
//!    ];
//!
//!    for source in sources {
//!       let torrent = engine.add_torrent(source).await?;
//!       println!("added {}", torrent.info_hash());
//!    }
//!
//!    Ok(())
//! }
//! ```
//!
//! ## More examples
//!
//! Browse the repository's
//! [examples directory](https://github.com/artrixdotdev/tortillas/tree/main/crates/libtortillas/examples)
//! for complete runnable programs, including live frontend integration.
//!
//! # Live updates are optional
//!
//! Applications that only need to download and seed files do not need
//! [`frontend`] listeners, events, views, or metrics. Those APIs exist for
//! applications that want to display live progress or forward state through a
//! terminal, web server, website, or desktop application.
//!
//! When live updates are useful, start with [`frontend::EventListener`] and the
//! current view exposed by [`frontend::EventListener::view`]. The
//! [`frontend`] module documents the complete transport-agnostic model.
//!
//! This helper waits for changes and prints verified payload progress until the
//! torrent finishes downloading:
//!
//! ```no_run
//! use libtortillas::prelude::{Torrent, TorrentState};
//!
//! async fn show_progress(torrent: &Torrent) -> Result<(), Box<dyn std::error::Error>> {
//!    let mut listener = torrent.listener();
//!
//!    loop {
//!       if let Some(view) = listener.view() {
//!          let progress = &view.metrics.progress;
//!          if let Some(total) = progress.total_bytes {
//!             println!("{} / {} bytes verified", progress.verified_bytes.0, total.0);
//!          }
//!          if view.state == TorrentState::Seeding {
//!             break;
//!          }
//!       }
//!
//!       listener.recv().await?;
//!    }
//!
//!    Ok(())
//! }
//! ```
//!
//! # Runtime and advanced APIs
//!
//! `libtortillas` is intentionally a Tokio-based library. Public handles such
//! as [`engine::Engine`] and [`torrent::Torrent`] expose async methods that
//! must be driven inside a Tokio runtime, and the crate uses Tokio tasks,
//! sockets, timers, channels, and filesystem APIs internally.
//!
//! Frontends should create one application-level Tokio runtime and keep the
//! engine plus all torrent handles on work scheduled by that runtime. The crate
//! does not promise runtime independence, HTTP client injection, clock
//! injection, listener injection, or storage runtime abstraction.
//! Synchronous adapter work should communicate with async engine tasks through
//! channels or a dedicated adapter thread. [`tokio::task::spawn_blocking`] is
//! appropriate for bounded blocking work, but not for a permanent input loop:
//! a blocking task cannot be aborted after it starts and can delay shutdown.
//!
//! An application can use `#[tokio::main]`, as in the example above, or create
//! an explicit Tokio runtime before initializing `Engine`.
//!
//! The lower-level [`engine`], [`torrent`], [`metainfo`], [`peer`],
//! [`tracker`], [`pieces`], and [`protocol`] modules remain public for advanced
//! integrations, tests, and protocol-level work. Applications should avoid
//! depending on actor messages, raw peer streams, tracker clients, or storage
//! internals when an equivalent [`facade`] type exists. [`prelude`] re-exports
//! the types most applications need.
//!
//! Engine and torrent handles expose listeners for live UI updates. Persistence
//! snapshots are intentionally separate and should not be polled for display
//! changes.
//!
//! # Internal architecture
//!
//! Most applications do not need these implementation details. They are
//! documented here for contributors and advanced integrations.
//!
//! The runtime is organized as a supervised actor tree:
//!
//! ```text
//! EngineActor
//! ├── DhtActor (one shared instance)
//! └── TorrentActor (one per torrent)
//!     ├── TrackerActor (one per tracker)
//!     └── PeerActor (one per connected peer)
//!
//! DhtActor ───── discovered peers ────> TorrentActor
//! TrackerActor ── discovered peers ────> TorrentActor
//! ```
//!
//! Actors own operational protocol state. Public applications interact through
//! [`engine::Engine`], [`torrent::Torrent`], and the transport-agnostic
//! [`frontend`] views and event streams. Durable state is represented by
//! [`engine::EngineSnapshot`] and [`torrent::TorrentSnapshot`], never by live
//! presentation views.
//!
//! Stable public types are exported by module facades while actor messages and
//! coordination details remain crate-private. Domain values such as lifecycle
//! state, storage strategy, metrics, and snapshots live outside actor files so
//! actors can focus on orchestration.
//!
//! See [`frontend`] for the source-of-truth, publication, lifecycle, and lock
//! invariants. See [`torrent`] for transfer scheduling and persistence
//! semantics.

pub(crate) mod dht;
pub mod engine;
pub mod errors;
pub mod facade;
pub mod frontend;
pub mod hashes;
pub mod metainfo;
pub mod metrics;
pub mod peer;
pub mod pieces;
pub mod protocol;
pub mod settings;
pub mod torrent;
pub mod tracker;

#[cfg(test)]
pub(crate) mod testing {
   use std::{
      env, io,
      net::{IpAddr, Ipv4Addr, SocketAddr},
      path::{Component, Path, PathBuf},
      process,
      str::FromStr,
      sync::Arc,
   };

   use rand::random_range;
   use tokio::{
      fs::{create_dir_all, read_to_string},
      io::{AsyncReadExt, AsyncWriteExt},
      net::{TcpListener, TcpStream},
      sync::Mutex,
      task::{JoinHandle, JoinSet},
   };

   use crate::{
      hashes::{Hash, InfoHash},
      metainfo::{MagnetUri, MetaInfo, TorrentFile},
      peer::{Peer, PeerId},
      protocol::{
         messages::{Handshake, PeerMessages},
         stream::PeerStream,
      },
      tracker::udp::UdpServer,
   };

   pub(crate) const BIG_BUCK_BUNNY_MAGNET: &str =
      include_str!("../tests/magneturis/big-buck-bunny.txt");
   pub(crate) const ARCH_LINUX_NAME: &str = "archlinux-2026.07.01-x86_64.iso";
   pub(crate) const ARCH_LINUX_TORRENT_FILE: &str = "archlinux-2026.07.01-x86_64.iso.torrent";
   pub(crate) const BIG_BUCK_BUNNY_MAGNET_FILE: &str = "big-buck-bunny.txt";
   pub(crate) const BIG_BUCK_BUNNY_NAME: &str = "Big Buck Bunny";
   pub(crate) const BIG_BUCK_BUNNY_INFO_HASH: &str = "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c";
   pub(crate) const BIG_BUCK_BUNNY_TORRENT_FILE: &str = "big-buck-bunny.torrent";
   pub(crate) const KNOPPIX_TORRENT_FILE: &str = "KNOPPIX_V9.1DVD-2021-01-25-EN.torrent";

   pub(crate) fn fixture_path(relative_path: &str) -> PathBuf {
      PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path)
   }

   pub(crate) fn torrent_fixture_path(file_name: &str) -> PathBuf {
      fixture_path(&format!("tests/torrents/{file_name}"))
   }

   pub(crate) fn magnet_fixture_path(file_name: &str) -> PathBuf {
      fixture_path(&format!("tests/magneturis/{file_name}"))
   }

   pub(crate) async fn read_torrent_fixture(file_name: &str) -> MetaInfo {
      TorrentFile::read(torrent_fixture_path(file_name))
         .await
         .unwrap()
   }

   pub(crate) async fn read_magnet_fixture(file_name: &str) -> MetaInfo {
      let contents = read_to_string(magnet_fixture_path(file_name))
         .await
         .unwrap();
      MagnetUri::parse(contents).unwrap()
   }

   pub(crate) fn big_buck_bunny_magnet() -> MetaInfo {
      MagnetUri::parse(BIG_BUCK_BUNNY_MAGNET.to_string()).unwrap()
   }

   pub(crate) fn random_port() -> u16 {
      random_range(1024..65535)
   }

   pub(crate) fn random_socket_addr() -> SocketAddr {
      SocketAddr::from_str(&format!("0.0.0.0:{}", random_port())).unwrap()
   }

   pub(crate) fn ephemeral_socket_addr() -> SocketAddr {
      SocketAddr::from_str("0.0.0.0:0").unwrap()
   }

   pub(crate) fn torrent_temp_path() -> PathBuf {
      env::temp_dir().join(format!(
         "tortillas-{}-{}",
         process::id(),
         rand::random::<u64>()
      ))
   }

   pub(crate) fn peer_id() -> PeerId {
      PeerId::default()
   }

   pub(crate) fn piece_hash(data: &[u8]) -> Hash<20> {
      use sha1::{Digest, Sha1};

      let mut hasher = Sha1::new();
      hasher.update(data);
      Hash::from_bytes(hasher.finalize().into())
   }

   /// Creates an isolated temporary storage root and removes it on drop.
   pub(crate) async fn storage_fixture(prefix: &str) -> io::Result<StorageFixture> {
      let mut components = Path::new(prefix).components();
      let Some(Component::Normal(prefix)) = components.next() else {
         return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "storage fixture prefix must be a single path component",
         ));
      };
      if components.next().is_some() {
         return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "storage fixture prefix must be a single path component",
         ));
      }

      let root = torrent_temp_path().join(prefix);
      create_dir_all(&root).await?;
      Ok(StorageFixture { root })
   }

   pub(crate) struct StorageFixture {
      root: PathBuf,
   }

   impl StorageFixture {
      pub(crate) fn path(&self) -> &Path {
         &self.root
      }

      pub(crate) fn child(&self, relative_path: &str) -> PathBuf {
         self.root.join(relative_path)
      }
   }

   impl Drop for StorageFixture {
      fn drop(&mut self) {
         let _ = std::fs::remove_dir_all(&self.root);
      }
   }

   /// Minimal self-hosted HTTP tracker for deterministic announce tests.
   pub(crate) struct LocalHttpTracker {
      addr: SocketAddr,
      requests: Arc<Mutex<Vec<String>>>,
      task: JoinHandle<()>,
   }

   impl LocalHttpTracker {
      /// Starts a tracker that returns the given IPv4 peers in compact form.
      pub(crate) async fn start(peers: impl IntoIterator<Item = Peer>) -> io::Result<Self> {
         let peers = Arc::new(peers.into_iter().collect::<Vec<_>>());
         let requests = Arc::new(Mutex::new(Vec::new()));
         let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
         let addr = listener.local_addr()?;
         let task = tokio::spawn(run_http_tracker(listener, peers, requests.clone()));

         Ok(Self {
            addr,
            requests,
            task,
         })
      }

      pub(crate) fn uri(&self) -> String {
         format!("http://{}", self.addr)
      }

      pub(crate) async fn requests(&self) -> Vec<String> {
         self.requests.lock().await.clone()
      }
   }

   impl Drop for LocalHttpTracker {
      fn drop(&mut self) {
         self.task.abort();
      }
   }

   async fn run_http_tracker(
      listener: TcpListener, peers: Arc<Vec<Peer>>, requests: Arc<Mutex<Vec<String>>>,
   ) {
      let mut connections = JoinSet::new();
      loop {
         tokio::select! {
            result = listener.accept() => {
               let Ok((stream, _)) = result else {
                  break;
               };
               let peers = peers.clone();
               let requests = requests.clone();
               connections.spawn(async move {
                  let _ = handle_http_tracker_connection(stream, peers, requests).await;
               });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
         }
      }
   }

   async fn handle_http_tracker_connection(
      mut stream: TcpStream, peers: Arc<Vec<Peer>>, requests: Arc<Mutex<Vec<String>>>,
   ) -> io::Result<()> {
      let mut buf = Vec::new();
      let mut chunk = [0; 1024];

      loop {
         let read = stream.read(&mut chunk).await?;
         if read == 0 {
            break;
         }
         buf.extend_from_slice(&chunk[..read]);
         if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
         }
      }

      if let Some(path) = request_path(&buf) {
         requests.lock().await.push(path);
      }

      let body = tracker_response_body(&peers);
      let response = format!(
         "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
         body.len()
      );

      stream.write_all(response.as_bytes()).await?;
      stream.write_all(&body).await?;
      Ok(())
   }

   fn request_path(request: &[u8]) -> Option<String> {
      let request = String::from_utf8_lossy(request);
      let line = request.lines().next()?;
      let mut parts = line.split_whitespace();
      let method = parts.next()?;
      if method != "GET" {
         return None;
      }
      parts.next().map(ToOwned::to_owned)
   }

   fn tracker_response_body(peers: &[Peer]) -> Vec<u8> {
      let compact_peers = compact_peer_bytes(peers);
      let mut response = format!("d8:intervali1800e5:peers{}:", compact_peers.len()).into_bytes();
      response.extend_from_slice(&compact_peers);
      response.extend_from_slice(b"e");
      response
   }

   fn compact_peer_bytes(peers: &[Peer]) -> Vec<u8> {
      let mut bytes = Vec::with_capacity(peers.len() * 6);
      for peer in peers {
         let IpAddr::V4(ip) = peer.ip else {
            continue;
         };
         bytes.extend_from_slice(&ip.octets());
         bytes.extend_from_slice(&peer.port.to_be_bytes());
      }
      bytes
   }

   /// Scriptable TCP peer that performs a BitTorrent handshake and writes
   /// messages.
   pub(crate) struct LocalPeer {
      addr: SocketAddr,
      handshakes: Arc<Mutex<Vec<Handshake>>>,
      task: JoinHandle<()>,
   }

   impl LocalPeer {
      /// Starts a peer that replies with `peer_id` and then sends `messages`.
      pub(crate) async fn start(peer_id: PeerId, messages: Vec<PeerMessages>) -> io::Result<Self> {
         let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
         let addr = listener.local_addr()?;
         let handshakes = Arc::new(Mutex::new(Vec::new()));
         let task = tokio::spawn(run_local_peer(
            listener,
            peer_id,
            Arc::new(messages),
            handshakes.clone(),
         ));

         Ok(Self {
            addr,
            handshakes,
            task,
         })
      }

      pub(crate) fn peer(&self) -> Peer {
         Peer::from_socket_addr(self.addr)
      }

      pub(crate) async fn handshakes(&self) -> Vec<Handshake> {
         self.handshakes.lock().await.clone()
      }
   }

   impl Drop for LocalPeer {
      fn drop(&mut self) {
         self.task.abort();
      }
   }

   async fn run_local_peer(
      listener: TcpListener, peer_id: PeerId, messages: Arc<Vec<PeerMessages>>,
      handshakes: Arc<Mutex<Vec<Handshake>>>,
   ) {
      let mut connections = JoinSet::new();
      loop {
         tokio::select! {
            result = listener.accept() => {
               let Ok((stream, _)) = result else {
                  break;
               };
               let messages = messages.clone();
               let handshakes = handshakes.clone();
               connections.spawn(async move {
                  let _ = handle_local_peer_connection(stream, peer_id, messages, handshakes).await;
               });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
         }
      }
   }

   async fn handle_local_peer_connection(
      stream: TcpStream, peer_id: PeerId, messages: Arc<Vec<PeerMessages>>,
      handshakes: Arc<Mutex<Vec<Handshake>>>,
   ) -> Result<(), crate::errors::PeerActorError> {
      let mut stream = PeerStream::tcp(stream);
      let handshake = stream.recv_handshake_message().await?;
      handshakes.lock().await.push(handshake.clone());

      let response = Handshake::new(handshake.info_hash.clone(), peer_id);
      stream.write_all(&response.to_bytes()).await?;

      for message in messages.iter() {
         stream.write_all(&message.to_bytes()?).await?;
      }

      Ok(())
   }

   pub(crate) fn test_info_hash() -> InfoHash {
      piece_hash(b"deterministic test info hash")
   }

   pub(crate) async fn udp_server() -> UdpServer {
      UdpServer::new(None).await.unwrap()
   }

   pub(crate) fn init_tracing() {
      let _ = tracing_subscriber::fmt()
         .with_target(true)
         .with_env_filter("libtortillas=trace,off")
         .pretty()
         .try_init();
   }

   #[cfg(test)]
   mod tests {
      use std::{net::Ipv4Addr, sync::Arc};

      use tokio::time::{Duration, timeout};

      use super::*;
      use crate::{protocol::stream::PeerRecv, tracker::TrackerBase};

      #[tokio::test]
      async fn local_http_tracker_returns_compact_peers_and_records_requests() {
         let peer = Peer::from_ipv4(Ipv4Addr::LOCALHOST, 6881);
         let tracker = LocalHttpTracker::start([peer.clone()]).await.unwrap();
         let http_tracker =
            crate::tracker::http::HttpTracker::new(tracker.uri(), test_info_hash(), None, None);

         let peers = http_tracker.announce().await.unwrap();

         assert_eq!(peers, vec![peer]);
         let requests = tracker.requests().await;
         assert_eq!(requests.len(), 1);
         assert!(requests[0].contains("info_hash="));
         assert!(requests[0].contains("peer_id="));
      }

      #[tokio::test]
      async fn local_peer_completes_handshake_and_sends_scripted_messages() {
         let remote_peer_id = peer_id();
         let local_peer = LocalPeer::start(remote_peer_id, vec![PeerMessages::Unchoke])
            .await
            .unwrap();

         let mut stream = PeerStream::connect(local_peer.peer().socket_addr(), None)
            .await
            .unwrap();
         let info_hash = Arc::new(test_info_hash());
         stream
            .send_handshake(peer_id(), info_hash.clone())
            .await
            .unwrap();

         let (received_peer_id, _) = stream.recv_handshake().await.unwrap();
         let message = timeout(Duration::from_secs(1), stream.recv())
            .await
            .unwrap();

         assert_eq!(received_peer_id, remote_peer_id);
         assert_eq!(message.unwrap(), PeerMessages::Unchoke);
         assert_eq!(local_peer.handshakes().await[0].info_hash, info_hash);
      }

      #[tokio::test]
      async fn local_peer_drop_closes_idle_connections() {
         let local_peer = LocalPeer::start(peer_id(), Vec::new()).await.unwrap();
         let mut stream = TcpStream::connect(local_peer.peer().socket_addr())
            .await
            .unwrap();
         tokio::task::yield_now().await;

         drop(local_peer);

         let mut byte = [0];
         let read = timeout(Duration::from_secs(1), stream.read(&mut byte))
            .await
            .expect("fixture connection should close before timeout");
         assert!(matches!(read, Ok(0) | Err(_)));
      }

      #[tokio::test]
      async fn storage_fixture_creates_and_removes_temp_directory() {
         let fixture = storage_fixture("piece-store").await.unwrap();
         let child = fixture.child("piece-0");

         tokio::fs::write(&child, b"piece").await.unwrap();
         assert_eq!(tokio::fs::read(&child).await.unwrap(), b"piece");

         let root = fixture.path().to_path_buf();
         drop(fixture);
         assert!(!root.exists());
      }

      #[tokio::test]
      async fn storage_fixture_rejects_prefixes_outside_its_temp_root() {
         let parent_path = storage_fixture("../shared").await.err().unwrap();
         let absolute_path = storage_fixture("/tmp/shared").await.err().unwrap();

         assert_eq!(parent_path.kind(), io::ErrorKind::InvalidInput);
         assert_eq!(absolute_path.kind(), io::ErrorKind::InvalidInput);
      }
   }
}

/// The prelude for this crate.
///
/// This module re-exports the most commonly used types, traits, and functions
/// so that you can conveniently import them all at once:
///
/// ```
/// use libtortillas::prelude::*;
/// ```
pub mod prelude {
   pub use crate::{
      engine::*,
      errors::*,
      facade::*,
      hashes::InfoHash,
      metainfo::*,
      peer::{Peer, PeerId},
      settings::*,
      torrent::*,
      tracker::Tracker,
   };
}
