# libtortillas Architecture

The library is organized around a small actor hierarchy:

- `EngineActor` owns global listeners, the shared UDP tracker socket, and the torrent registry.
- `TorrentActor` owns per-torrent state and coordinates peers, trackers, piece progress, and exports.
- `PeerActor` owns one peer connection and peer-local protocol state.
- `TrackerActor` owns one tracker announce loop and forwards discovered peers to its torrent supervisor.

Module facades should export stable public types while keeping actor internals private to the crate.
Domain types such as torrent state, storage strategy, exported snapshots, tracker model types, and tracker stats live outside actor files so actors can focus on orchestration.

## Choking

`TorrentActor` owns the BEP 3 choking scheduler for its swarm. Active torrents run a rechoke round every 10 seconds, collect peer-local transfer stats from `PeerActor`, and keep at most four interested peers unchoked.

While downloading, regular upload slots are assigned to interested peers with the highest recent download rate. While seeding, regular slots are assigned by recent upload rate. When more interested peers exist than upload slots, one slot is reserved for an optimistic unchoke and rotates every third rechoke round.

`PeerActor` remains responsible for wire-level enforcement: it sends `Choke` and `Unchoke` messages when the torrent scheduler changes state, ignores piece requests from choked peers, and records uploaded bytes when serving piece data.
