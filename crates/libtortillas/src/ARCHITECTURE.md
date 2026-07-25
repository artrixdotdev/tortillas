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

The frontend coordination boundary is split by owned lifecycle:

```text
frontend/
├── live.rs                 generic state/channel primitive
├── registry.rs             guard-free keyed scope storage
├── handle/
│   ├── mod.rs              generic identity-bearing handle primitive
│   ├── peer.rs             peer identity and public access
│   └── tracker.rs          tracker identity and public access
└── hub/
    ├── mod.rs              ownership root and scope definitions
    ├── engine.rs           root status and derived engine view
    ├── torrent.rs          torrent projection and tree cleanup
    ├── peer.rs             torrent-local peer registry
    └── tracker.rs          torrent-local tracker registry
```

## Architectural Invariants

These rules define the source of truth:

1. Actors own operational domain state.
2. A live scope owns only its frontend projection.
3. Parent views are derived from child scopes; they never keep manually synchronized child-view copies.
4. Every scope has one view-and-event publication entry point.
5. Peer and tracker events do not implicitly rebuild torrent or engine state.
6. A scope closes exactly once, only when it cannot restart.
7. Snapshot schema validation runs once at the authoritative engine restore boundary.
8. Actor and publisher back-references are weak; the ownership graph contains no strong cycle.
9. Synchronous lock order is registry, scope publication/state, then event sender.
10. No actor communication, filesystem operation, arbitrary callback, or `.await` occurs while a synchronous lock is held.

## Frontend Boundary

`Engine` and `Torrent` own the stable application boundary. Their direct
methods are the only public command API, while `listener` combines a bounded
event subscription with current `EngineView` or `TorrentView` state. A shared
frontend hub owns engine lifecycle state and a keyed registry of torrent
scopes. Each torrent scope owns its live torrent publisher plus its peer and
tracker registries. Public handles hold weak back-references to that hub, and
each scope has an irreversible terminal state so actor updates cannot
resurrect removed objects.

`EngineView` is derived on read from engine lifecycle state and current torrent
scopes, sorted by info hash. The engine does not cache a second
`Vec<TorrentView>`. Peer state and metric updates therefore touch only one peer
scope. Peer connection and disconnection are propagated separately as discrete
parent events without cloning unrelated torrent projections.

Engine events project the canonical `TorrentEventKind` hierarchy through
`CoreEventKind::Torrent`; they do not duplicate every torrent, peer, and
tracker event in a second vocabulary.

Live views are intentionally distinct from `EngineSnapshot` and
`TorrentSnapshot`. Views are presentation-oriented and continuously updated by
events. Snapshots are versioned, Serde-compatible persistence records that
capture metadata, storage configuration, lifecycle intent, and piece progress
for later restoration. Frontends must not poll persistence snapshots to render
live state.

Event channels are allocated lazily on first subscription. Their capacities are
configured independently through `FrontendSettings`.

## Metrics

Bytes are the canonical internal unit. Peer state, peer statistics, live peer
views, and torrent aggregation share `TransferMetrics`; projection code never
converts KiB/s to bytes/s. `TrafficTotals` describe wire traffic and remain
separate from verified `ContentProgress`. `None` rates mean no sample exists,
while a present zero rate means a sample measured no transfer. ETA is derived
from remaining verified content and aggregate sampled download rate.

Peer actors publish peer-local samples. `TorrentActor` publishes one coalesced
`TorrentMetrics` update after periodic peer-stat collection.

`PeerEventKind::StateChanged` and `PeerEventKind::MetricsChanged` remain local
to the peer listener. Root propagation is reserved for connection lifecycle,
tracker lifecycle, and coalesced torrent metrics.

## Persistence Boundary

Restoration is ordered as:

```text
schema validation
  -> storage reconciliation
  -> actor-state installation
  -> optional transfer resumption
```

`.torrent` sources store `Info` only inside `MetaInfo`; only resolved magnet
metadata uses `resolved_magnet_info`. `TorrentSnapshot::resolved_info` is the
canonical resolver. Custom piece managers return a typed unsupported error
until a durable descriptor/factory contract exists.

Full storage verification is the default. It hashes completed payload, demotes
missing or corrupt pieces, and clears partial-block bits whose referenced bytes
do not exist. `TrustSnapshot` is explicit and unsafe.

Snapshot JSON is a durable contract. Version 1 is migrated during
deserialization to version 2. Version 2 uses `u64` for portable numeric fields,
sorted vectors for keyed scheduler state, and `Vec<bool>` for bitfields instead
of serializing `DashMap`, `usize`, or `BitVec` implementation details. Every
supported version has a golden JSON fixture. Unsupported future versions remain
typed validation errors.

Tracker and torrent actors publish `Restarting` after abnormal supervised
termination and keep their scopes open. Only normal, final ownership teardown
publishes `Stopped` and closes the scope tree.

## Locking and Publication

The lock hierarchy is registry shard, scope publication/state, then event
sender. `ScopeRegistry` wraps `DashMap`, but never exposes shard guards:
registry methods return cloned `Arc` values or owned vectors. Every shard guard
is therefore released before a scope publication lock is acquired. Scope
construction happens before shard entry acquisition, so callbacks do not run
under a registry lock. A scope publication lock serializes its view transition,
scoped event, and corresponding root event. `LivePublisher` then acquires its
state lock before its optional sender lock. No code acquires a registry guard
while holding a child scope lock, and no synchronous lock crosses an `.await`.

## Runtime Boundary

`libtortillas` is intentionally tied to Tokio. The crate uses Tokio for actor
task execution, TCP and UDP sockets, timers, cancellation, channels, and
filesystem work. HTTP fetching is also part of the library runtime path through
`reqwest`.

Frontend applications should treat Tokio as the runtime boundary. Every
application adapter should create one Tokio runtime at process startup and run
`Engine` plus all torrent handle operations on that runtime. Synchronous or
blocking adapter work should be isolated from async torrent work through
channels or an adapter-owned thread. `tokio::task::spawn_blocking` is suitable
for bounded blocking operations, but not long-lived blocking loops: a blocking
task cannot be aborted after it starts and can delay runtime shutdown.

`libtortillas` contains no rendering, input-device, transport-server, or
framework-specific policy. Terminal interfaces, HTTP/WebSocket servers, web
backends, and desktop applications are peer adapters of the same facade. They
consume serializable views and typed event streams and translate user intent
into handle operations outside this crate.

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
used by tracker actors. Its `AnnounceFrom` value retains whether peers came
from DHT or a specific tracker. This keeps connection filtering, deduplication,
and `PeerActor` creation in `TorrentActor` regardless of where an endpoint was
discovered. Valid lookup tokens are used to announce the engine's peer port
back to the closest DHT nodes.

[BEP 5]: https://www.bittorrent.org/beps/bep_0005.html
[BEP 27]: https://www.bittorrent.org/beps/bep_0027.html

## Torrent Lifecycle

`TorrentState` is the frontend-facing lifecycle contract carried by live views and persistence snapshots.
New torrents start as `Added` when metadata is already available, or `ResolvingMetadata` when a source such as a magnet URI still needs an info dict.
Once metadata and the configured peer threshold are available, a torrent becomes `Ready` if autostart is disabled, or moves directly into `Downloading` when autostart/manual start begins transfer.

Completed downloads transition to `Seeding`.
`Paused` is distinct from `Ready` and is not eligible for autostart, so frontends can intentionally hold a torrent without it being treated as merely inactive.
Shutdown and failure paths report `Stopping`, `Stopped`, or `Failed` instead of collapsing those cases into the same state as a paused or newly added torrent.

## Choking

`TorrentActor` owns the BEP 3 choking scheduler for its swarm. Active torrents run a rechoke round every 10 seconds, collect peer-local transfer stats from `PeerActor`, and keep at most four interested peers unchoked.

While downloading, regular upload slots are assigned to interested peers with the highest recent download rate. While seeding, regular slots are assigned by recent upload rate. When more interested peers exist than upload slots, one slot is reserved for an optimistic unchoke and rotates every third rechoke round.

`PeerActor` remains responsible for wire-level enforcement: it sends `Choke` and `Unchoke` messages when the torrent scheduler changes state, ignores piece requests from choked peers, and records uploaded bytes when serving piece data.
