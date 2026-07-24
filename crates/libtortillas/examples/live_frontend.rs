use std::{io, path::PathBuf};

use libtortillas::prelude::{
   CoreCommand, CoreCommandResult, CoreEventKind, Engine, EventStreamError, TorrentCommand,
   TorrentSource, TorrentState,
};
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
   let Some(torrent_path) = std::env::args_os().nth(1).map(PathBuf::from) else {
      error!("pass a .torrent file path to run the live frontend example");
      return Ok(());
   };

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

   let result = engine
      .send(CoreCommand::AddTorrent {
         source: TorrentSource::torrent_file_path(torrent_path),
      })
      .await?;
   let CoreCommandResult::TorrentAdded(torrent) = result else {
      return Err(io::Error::other("add command did not return a torrent handle").into());
   };

   let mut torrent_listener = torrent.listener();
   torrent.send(TorrentCommand::Pause).await?;
   let paused = loop {
      let event = torrent_listener.recv().await?;
      if matches!(
         event.kind,
         CoreEventKind::TorrentStateChanged {
            current: TorrentState::Paused,
            ..
         }
      ) {
         break event;
      }
   };
   info!(sequence = paused.sequence, ?paused.kind, "torrent paused");
   torrent.send(TorrentCommand::Start).await?;

   tokio::signal::ctrl_c().await?;
   let _ = engine.send(CoreCommand::Shutdown).await?;
   frontend.await?;
   Ok(())
}
