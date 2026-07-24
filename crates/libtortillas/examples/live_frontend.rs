use std::path::PathBuf;

use libtortillas::prelude::{
   CoreEventKind, Engine, EventStreamError, TorrentEventKind, TorrentSource, TorrentState,
};
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
   let mut args = std::env::args_os().skip(1).map(PathBuf::from);
   let Some(torrent_path) = args.next() else {
      error!("pass a .torrent file path and optional session path to run the example");
      return Ok(());
   };
   let session_path = args.next();

   let engine = Engine::default();
   let mut listener = engine.listener();
   let frontend = tokio::spawn(async move {
      loop {
         match listener.recv().await {
            Ok(event) => {
               let view = listener.view();
               info!(
                  sequence = event.sequence,
                  torrent_count = view.torrent_count,
                  ?event.kind,
                  "frontend received a live engine event"
               );
               if matches!(event.kind, CoreEventKind::Shutdown(_)) {
                  break;
               }
            }
            Err(EventStreamError::Lagged(events)) => {
               let view = listener.view();
               warn!(
                  events,
                  torrent_count = view.torrent_count,
                  "redrawing live state after lag"
               );
            }
            Err(EventStreamError::Closed) => {
               info!("frontend event stream closed");
               break;
            }
         }
      }
   });

   let torrent = engine
      .add_torrent(TorrentSource::torrent_file_path(torrent_path))
      .await?;

   let mut torrent_listener = torrent.listener();
   torrent.pause().await?;
   let paused = loop {
      let event = torrent_listener.recv().await?;
      if matches!(
         event.kind,
         TorrentEventKind::StateChanged {
            current: TorrentState::Paused,
            ..
         }
      ) {
         break event;
      }
   };
   info!(sequence = paused.sequence, ?paused.kind, "torrent paused");
   torrent.start().await?;

   tokio::signal::ctrl_c().await?;
   if let Some(path) = session_path {
      let snapshot = engine.snapshot().await?;
      tokio::fs::write(&path, serde_json::to_vec_pretty(&snapshot)?).await?;
      info!(?path, "saved resumable engine state");
   }
   engine.shutdown().await?;
   frontend.await?;
   Ok(())
}
