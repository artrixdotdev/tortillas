use libtortillas::{
   facade::{EngineSnapshot, TorrentSnapshot},
   prelude::{EngineHandle, EventSubscription, PeerEventKind, TorrentEventKind, TrackerEventKind},
};

#[test]
fn prelude_exposes_frontend_facade_types() {
   fn accepts_torrent_events(_: Option<EventSubscription<TorrentEventKind>>) {}
   fn accepts_peer_events(_: Option<EventSubscription<PeerEventKind>>) {}
   fn accepts_tracker_events(_: Option<EventSubscription<TrackerEventKind>>) {}

   accepts_torrent_events(None);
   accepts_peer_events(None);
   accepts_tracker_events(None);
}

#[test]
fn facade_engine_handle_matches_existing_engine_type() {
   fn accepts_engine_handle(_: Option<EngineHandle>) {}

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
