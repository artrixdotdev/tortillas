# Frontend integration

`libtortillas` exposes live frontend behavior directly on `Engine` and
`Torrent`. Applications do not need actor references, protocol messages, or a
polling loop.

## Live listeners

Call `Engine::listener()` before sending commands. The listener combines two
related capabilities:

- `recv().await` yields sequenced `CoreEvent` values as changes happen.
- `view()` returns the latest display-oriented `EngineView` held by the live
  publisher.

Every `Torrent` returned by an add command similarly has `listener()` and
`subscribe()` methods. A torrent listener receives only events associated with
that torrent and exposes its latest `TorrentView`.

The event channel retains 256 events per listener by default. Slow listeners
receive `EventStreamError::Lagged` instead of causing unbounded memory growth.
After lagging, redraw from `listener.view()` and continue calling `recv()`.
Sequence numbers remain engine-local and monotonic.

Use `subscribe()` when only discrete events are needed. Use `listener()` when
the frontend also needs a coherent current view for initial rendering or lag
recovery.

## Commands

`Engine::send(CoreCommand)` is the application-level message boundary. It can
add, start, pause, resume, configure, remove, and stop torrents, or shut down
the engine. Add commands return a `CoreCommandResult::TorrentAdded` handle.

`Torrent::send(TorrentCommand)` provides the same typed pattern when a
frontend already owns a torrent handle. Both handles retain their existing
explicit convenience methods.

## Views and persistence snapshots

`EngineView`, `TorrentView`, and `CoreEvent` are live presentation contracts.
They are updated by listeners and are suitable for rendering.

`Engine::snapshot()` and `Torrent::snapshot()` are not the live frontend path.
Snapshots are the persistence boundary for serializing resumable engine and
torrent state in an application-selected Serde format. Frontends should never
poll snapshots to refresh the UI.

For example, an application can serialize `engine.snapshot().await?` with
`serde_json`, `postcard`, `rmp-serde`, or another format, then deserialize it
and call `engine.restore(snapshot).await?` in a later process. Use
`engine.restore_torrent(snapshot).await?` for one torrent. Snapshot schemas are
versioned so incompatible data returns a typed error.

Snapshots retain metadata, lifecycle intent, storage paths and strategy,
verified pieces, and partial blocks. Downloaded bytes remain in the referenced
storage paths; the snapshot does not duplicate payload data into frontend
state.

## Runtime and shutdown

The library is Tokio-based. Keep the engine, torrent handles, command tasks,
and listener tasks on the application runtime. Terminal or UI operations that
block should run separately from those async tasks.

Send `CoreCommand::Shutdown` or call `Engine::shutdown()` and keep the engine
listener alive until it receives `CoreEventKind::Shutdown`. This ensures the
frontend observes the terminal state after managed torrents stop.

See [`live_frontend.rs`](../crates/libtortillas/examples/live_frontend.rs) for a
compiling command/listener loop.
