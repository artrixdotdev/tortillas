use std::{env, path::PathBuf, process, time::Duration};

use libtortillas::{
   engine::{Engine, TorrentSource},
   metainfo::{MetaInfo, TorrentFile},
   settings::Settings,
};
use rand::random;
use tokio::{fs, time::timeout};

const ARCH_LINUX_TORRENT: &str = "archlinux-2026.07.01-x86_64.iso.torrent";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires internet access and the public BitTorrent DHT"]
async fn arch_linux_torrent_when_public_dht_is_available_then_downloads_data() {
   let torrent_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
      .join("tests/torrents")
      .join(ARCH_LINUX_TORRENT);
   let torrent_bytes = fs::read(torrent_path).await.unwrap();
   let MetaInfo::Torrent(mut torrent_file) = TorrentFile::parse(&torrent_bytes).unwrap() else {
      panic!("Arch Linux fixture must be a torrent file");
   };
   torrent_file.url_list = None;
   let dht_only_torrent = serde_bencode::to_bytes(&torrent_file).unwrap();
   let output_root = env::temp_dir().join(format!(
      "tortillas-arch-dht-{}-{}",
      process::id(),
      random::<u64>()
   ));
   fs::create_dir_all(&output_root).await.unwrap();

   let engine = Engine::builder()
      .settings(Settings::default())
      .autostart(true)
      .sufficient_peers(1)
      .output_path(output_root.clone())
      .build();
   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_bytes(dht_only_torrent))
      .await
      .unwrap();
   let mut listener = torrent.listener();

   let download = timeout(DOWNLOAD_TIMEOUT, async {
      loop {
         let view = listener.view().unwrap();
         if view.progress.downloaded_bytes > 0 {
            return view;
         }
         timeout(POLL_INTERVAL, listener.recv()).await.ok();
      }
   })
   .await;

   engine.shutdown().await.unwrap();
   fs::remove_dir_all(&output_root).await.unwrap();

   let view = download.expect("Arch Linux did not download data through DHT in time");
   assert!(view.has_metadata);
   assert!(view.peer_count > 0);
}
