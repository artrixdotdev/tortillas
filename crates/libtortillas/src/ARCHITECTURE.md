# libtortillas Architecture

The library is organized around a small actor hierarchy:

- `EngineActor` owns global listeners, the shared UDP tracker socket, the torrent registry, and one optional `DhtActor`.
- `DhtActor` owns the engine-wide DHT socket, routing table, transaction matching, announce tokens, and scheduled torrent lookups.
- `TorrentActor` owns per-torrent state and coordinates peers, trackers, piece progress, and exports.
- `PeerActor` owns one peer connection and peer-local protocol state.
- `TrackerActor` owns one tracker announce loop and forwards discovered peers to its torrent supervisor.

```text
EngineActor
├── DhtActor (one shared instance)
└── TorrentActor (one per torrent)
    ├── TrackerActor (one per tracker)
    └── PeerActor (one per connected peer)

DhtActor ── discovered peers ──> TorrentActor
TrackerActor ── discovered peers ──> TorrentActor
```

Module facades should export stable public types while keeping actor internals private to the crate.
Domain types such as torrent state, storage strategy, exported snapshots, tracker model types, and tracker stats live outside actor files so actors can focus on orchestration.

## Runtime Boundary

`libtortillas` is intentionally tied to Tokio. The crate uses Tokio for actor
task execution, TCP and UDP sockets, timers, cancellation, channels, and
filesystem work. HTTP fetching is also part of the library runtime path through
`reqwest`.

Frontend applications should treat Tokio as the runtime boundary. A frontend,
including the planned Tortillas TUI, should create one Tokio runtime at process
startup and run `Engine` plus all torrent handle operations on that runtime. If
the UI layer has blocking terminal rendering or input loops, those should be
isolated from async torrent work with channels and a dedicated UI thread.
`tokio::task::spawn_blocking` is suitable for bounded blocking operations, but
not a long-lived input loop: a blocking task cannot be aborted after it starts
and can delay runtime shutdown.

Runtime independence is not a current API promise. The public facade should not
claim support for custom async runtimes, injected HTTP clients, injected clocks,
custom network listeners, or non-Tokio storage executors unless those extension
points are added explicitly.

## DHT Peer Discovery

`EngineActor` supervises a single `DhtActor` because [BEP 5] defines a DHT node
as a client-wide UDP service, rather than one service per torrent. When a public
torrent is added, the engine registers its info hash and `TorrentActor` with the
DHT actor. Private torrents are not registered because [BEP 27] limits their
peer discovery to declared trackers.

The DHT actor bootstraps its routing table, performs iterative `get_peers`
lookups, and forwards results to the torrent through the same `Announce` event
used by tracker actors. This keeps connection filtering, deduplication, and
`PeerActor` creation in `TorrentActor` regardless of where an endpoint was
discovered. Valid lookup tokens are used to announce the engine's peer port
back to the closest DHT nodes.

`NodeId` and `PeerId` remain distinct types even though both use the shared
`Hash<20>` storage. BEP 5 distinguishes a DHT node, identified for UDP routing,
from a BitTorrent peer, identified on peer-wire connections. Keeping newtypes
prevents accidentally sending a peer ID in a KRPC field while still reusing the
same fixed-size hash implementation.

The DHT actor owns its mutable routing and torrent maps, so ordinary `HashMap`
storage is sufficient there. Outbound lookup tasks share only the UDP transport
and its small synchronized transaction table; they return results to the actor
instead of mutating actor state concurrently.

[BEP 5]: https://www.bittorrent.org/beps/bep_0005.html
[BEP 27]: https://www.bittorrent.org/beps/bep_0027.html

## Torrent Lifecycle

`TorrentState` is the frontend-facing lifecycle contract exported in torrent snapshots.
New torrents start as `Added` when metadata is already available, or `ResolvingMetadata` when a source such as a magnet URI still needs an info dict.
Once metadata and the configured peer threshold are available, a torrent becomes `Ready` if autostart is disabled, or moves directly into `Downloading` when autostart/manual start begins transfer.

Completed downloads transition to `Seeding`.
`Paused` is distinct from `Ready` and is not eligible for autostart, so frontends can intentionally hold a torrent without it being treated as merely inactive.
Shutdown and failure paths report `Stopping`, `Stopped`, or `Failed` instead of collapsing those cases into the same state as a paused or newly added torrent.

## Choking

`TorrentActor` owns the BEP 3 choking scheduler for its swarm. Active torrents run a rechoke round every 10 seconds, collect peer-local transfer stats from `PeerActor`, and keep at most four interested peers unchoked.

While downloading, regular upload slots are assigned to interested peers with the highest recent download rate. While seeding, regular slots are assigned by recent upload rate. When more interested peers exist than upload slots, one slot is reserved for an optimistic unchoke and rotates every third rechoke round.

`PeerActor` remains responsible for wire-level enforcement: it sends `Choke` and `Unchoke` messages when the torrent scheduler changes state, ignores piece requests from choked peers, and records uploaded bytes when serving piece data.
