# Frontend integration

`libtortillas` exposes live frontend behavior directly on `Engine` and
`Torrent`. Applications do not need actor references, protocol messages, or a
polling loop.

## Live listeners

Call `Engine::listener()` before invoking operations. The listener combines two
related capabilities:

- `recv().await` yields sequenced `EngineEvent` values as changes happen.
- `view()` derives the latest presentation-oriented `EngineView` from the live
  scope tree.

Every `Torrent` returned by `Engine::add_torrent()` similarly has `listener()`
and `subscribe()` methods. A torrent listener has its own scope, receives
typed `TorrentEvent` values for that torrent only, and exposes its latest
`TorrentView`. It does not filter the engine's global event stream.

Peers and trackers returned by `Torrent::peers()` and `Torrent::trackers()`
follow the same pattern. Each `PeerHandle` and `TrackerHandle` owns an
independent typed listener and current view, including a terminal disconnected
or stopped view. Engine listeners receive
`EngineEventKind::Torrent { torrent, event }`, where `event` uses the same
`TorrentEventKind` vocabulary as the torrent listener. Peer and tracker
changes carry their public handles inside that nested event, so a frontend can
descend into more detailed streams only when needed.

Event channels are allocated lazily on first subscription. Defaults retain 256
engine or torrent events and 64 peer or tracker events; all four capacities are
configurable through `FrontendSettings`. Slow listeners receive
`EventStreamError::Lagged` instead of causing unbounded memory growth. After
lagging, rebuild adapter state from `listener.view()` and continue calling
`recv()`. Sequence numbers are monotonic within each scope.

Use `subscribe()` when only discrete events are needed. Use `listener()` when
the frontend also needs a coherent current view for initial rendering or lag
recovery.

## Operations

`Engine` and `Torrent` methods are the sole public command API. Call
`engine.add_torrent(...)`, `engine.remove_torrent(...)`, or
`engine.shutdown()` directly; call `torrent.start()`, `torrent.pause()`, and
configuration methods directly on a `Torrent` handle. There is no parallel
command enum or generic `send` method duplicating these operations.

Applications that need to funnel UI actions through a task can put their own
application command type on a Tokio channel and call these methods in its
consumer. That keeps application-specific routing outside the engine without
making the library maintain two representations of every operation.

`resume()` aliases `start()` and `stop()` aliases `pause()` because those pairs
currently produce the same engine transition.

## Reusable live publishers

`LivePublisher<V, E>`, `EventListener<V, E>`, and `EventSubscription<E>` are
generic over the current view and event type. `EventListener` and
`EventSubscription` implement `futures::Stream`, while `recv()` supports the
usual Tokio-style loop. Engine, torrent, peer, and tracker APIs all reuse these
types; future protocols can expose the same behavior without another listener
implementation.

Publisher mutation names state their complete effect:
`replace_view()` changes only the current projection,
`replace_view_and_emit()` performs a coherent view/event transition,
`emit_without_view_change()` emits a discrete event, and
`close_with_terminal_event()` performs the one irreversible close transition.

## Views and persistence snapshots

`EngineView`, `TorrentView`, `PeerView`, and `TrackerView` are live presentation
contracts. They are suitable for rendering, API responses, or transport
serialization and are Serde-compatible.

`Engine::snapshot()` and `Torrent::snapshot()` are not the live frontend path.
Snapshots are the persistence boundary for serializing resumable engine and
torrent state in an application-selected Serde format. Frontends should never
poll snapshots to refresh the UI.

For example, an application can serialize `engine.snapshot().await?` with
`serde_json`, `postcard`, `rmp-serde`, or another format, then deserialize it
and call `engine.restore(snapshot).await?` in a later process. Use
`engine.restore_torrent(snapshot).await?` for one torrent. Snapshot schemas are
versioned so incompatible data returns a typed error.

Engine restore is validated and applied as one engine-actor operation. The
target must be empty when that operation begins, and a failed multi-torrent
restore removes everything created by that operation.

Snapshots retain metadata, lifecycle intent, storage paths and strategy,
verified pieces, and partial blocks. Downloaded bytes remain in the referenced
storage paths; the snapshot does not duplicate payload data into frontend
state.

## Runtime and shutdown

The library is Tokio-based. Keep the engine, torrent handles, application tasks,
and listener tasks on the application runtime. Any blocking adapter work should
run separately from those async tasks.

Call `Engine::shutdown()` and keep the engine
listener alive until it receives `EngineEventKind::Shutdown`. This ensures the
frontend observes the terminal state after managed torrents stop.

See [`live_frontend.rs`](../crates/libtortillas/examples/live_frontend.rs) for a
compiling operation/listener loop.
