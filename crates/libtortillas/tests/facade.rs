use libtortillas::{
   facade::{EngineSnapshot, TorrentSnapshot},
   prelude::Engine,
};

#[cfg(feature = "live")]
#[test]
fn prelude_exposes_live_facade_types() {
   use libtortillas::prelude::{
      EventSubscription, PeerEventKind, TorrentEventKind, TrackerEventKind,
   };

   fn accepts_torrent_events(_: Option<EventSubscription<TorrentEventKind>>) {}
   fn accepts_peer_events(_: Option<EventSubscription<PeerEventKind>>) {}
   fn accepts_tracker_events(_: Option<EventSubscription<TrackerEventKind>>) {}

   accepts_torrent_events(None);
   accepts_peer_events(None);
   accepts_tracker_events(None);
}

#[test]
fn facade_engine_handle_matches_existing_engine_type() {
   fn accepts_engine_handle(_: Option<Engine>) {}

   accepts_engine_handle(None);
}

#[test]
fn facade_reexports_canonical_snapshot_types() {
   fn accepts_engine_snapshot(_: Option<libtortillas::engine::EngineSnapshot>) {}
   fn accepts_torrent_snapshot(_: Option<libtortillas::torrent::TorrentSnapshot>) {}

   let engine_snapshot: Option<EngineSnapshot> = None;
   let torrent_snapshot: Option<TorrentSnapshot> = None;

   accepts_engine_snapshot(engine_snapshot);
   accepts_torrent_snapshot(torrent_snapshot);
}

#[cfg(not(feature = "live"))]
#[test]
fn actor_only_build_keeps_command_and_query_methods() {
   use libtortillas::prelude::Torrent;

   let _ = Engine::start_all;
   let _ = Engine::torrent;
   let _ = Engine::snapshot;
   let _ = Torrent::state;
   let _ = Torrent::pause;
   let _ = Torrent::snapshot;
}
