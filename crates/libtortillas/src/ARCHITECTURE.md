# libtortillas Architecture

The library is organized around a small actor hierarchy:

- `EngineActor` owns global listeners, the shared UDP tracker socket, and the torrent registry.
- `TorrentActor` owns per-torrent state and coordinates peers, trackers, piece progress, and exports.
- `PeerActor` owns one peer connection and peer-local protocol state.
- `TrackerActor` owns one tracker announce loop and forwards discovered peers to its torrent supervisor.

Module facades should export stable public types while keeping actor internals private to the crate.
Domain types such as torrent state, storage strategy, exported snapshots, tracker model types, and tracker stats live outside actor files so actors can focus on orchestration.
