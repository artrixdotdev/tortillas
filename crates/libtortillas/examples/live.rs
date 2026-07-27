use std::{
   path::PathBuf,
   time::{Duration, Instant},
};

use libtortillas::prelude::{
   Engine, EngineEventKind, EventStreamError, TorrentEventKind, TorrentSource, TorrentState,
};
use tracing::{error, info, warn};

const METRICS_LOG_INTERVAL: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
   tracing_subscriber::fmt()
      .with_env_filter("live=trace,off")
      .init();
   let mut args = std::env::args_os().skip(1).map(PathBuf::from);
   let Some(torrent_path) = args.next() else {
      error!("pass a .torrent file path and optional session path to run the example");
      return Ok(());
   };

   println!("Torrent path: {:?}", torrent_path);
   let session_path = args.next();

   let engine = Engine::default();
   let mut listener = engine.listener();
   let event_task = tokio::spawn(async move {
      let mut last_metrics_log = None;
      loop {
         match listener.recv().await {
            Ok(event) => {
               let view = listener.view();
               match &event.kind {
                  EngineEventKind::Torrent {
                     torrent,
                     event: TorrentEventKind::MetricsChanged(metrics),
                  } => {
                     let complete = metrics
                        .progress
                        .remaining_bytes
                        .is_some_and(|bytes| bytes.0 == 0);
                     let now = Instant::now();
                     if complete
                        || last_metrics_log
                           .is_none_or(|last| now.duration_since(last) >= METRICS_LOG_INTERVAL)
                     {
                        last_metrics_log = Some(now);
                        info!(
                           sequence = event.sequence,
                           torrent_id = %torrent.info_hash(),
                           downloaded_bytes = metrics.traffic.totals.downloaded.0,
                           verified_bytes = metrics.progress.verified_bytes.0,
                           total_bytes = ?metrics.progress.total_bytes.map(|bytes| bytes.0),
                           "received torrent metrics"
                        );
                     }
                  }
                  _ => info!(
                     sequence = event.sequence,
                     torrent_count = view.torrent_count(),
                     ?event.kind,
                     "received an engine event"
                  ),
               }
               if matches!(event.kind, EngineEventKind::Shutdown(_)) {
                  break;
               }
            }
            Err(EventStreamError::Lagged(events)) => {
               let view = listener.view();
               warn!(
                  events,
                  torrent_count = view.torrent_count(),
                  "refreshing current state after lag"
               );
            }
            Err(EventStreamError::Closed) => {
               info!("engine event stream closed");
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
   event_task.await?;
   Ok(())
}
