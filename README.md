# Tortillas
A terminal-based TUI (Text User Interface) torrent client built in Rust 🫓.

## 🚧 Project Status

**Note:** This project is currently under active development and is not yet feature-complete. It is by no means ready for use.

## 🛠️ Roadmap
See our roadmap [here](https://github.com/users/artrixdotdev/projects/6).

### ✅ Currently Supported
- Parsing and handling Magnet URIs
- Parsing and handling `.torrent` files
- uTP and TCP peer connections
- Full implementation of the BitTorrent protocol
- Mainline DHT peer discovery for public and trackerless torrents

### 🔄 In Development
- Completion of [libtortillas](crates/libtortillas) library


### 📈 Future Plans
- Frontend TUI (Text User Interface)

### ❌ Not Planned
- WebTorrent connections: Due to the lack of clear documentation and complex, undocumented protocols WebTorrent support is not currently planned.

## 🧪 Testing

We use [Nextest](https://nexte.st/) for running tests. To run tests locally, you may need to install Nextest:

```bash
# Install Nextest (example using pre-built binaries)
# See: https://nexte.st/docs/installation/pre-built-binaries/
```

The default suite should be deterministic and must not depend on public trackers, public peers, or mutable internet state:

```bash
cargo nextest run --workspace
```

Tests that intentionally exercise public BitTorrent infrastructure are marked `#[ignore]` and should only be run explicitly:

```bash
cargo nextest run --workspace --run-ignored ignored-only
```

The Arch Linux fixture exercises a trackerless torrent through the public DHT
and passes only after downloading real ISO data:

```bash
cargo nextest run -p libtortillas --test dht_network --run-ignored ignored-only
```

Test names use this convention:

```text
subject_when_condition_then_expected_result
```

Examples:

```text
magnet_uri_when_query_is_missing_then_returns_error
peer_stream_when_handshake_is_valid_then_returns_peer_id
```

Keep tests focused on one behavior, prefer deterministic fixtures with `include_str!` or `include_bytes!`, and avoid public network access unless the test is explicitly ignored.

## 📦 Installation
### Tortillas
Tortillas is the frontend TUI (Text User Interface) application (what most people want)

There are plans to publish tortillas to registries such as [crates.io](https://crates.io) and [the AUR](https://aur.archlinux.org). However, for now, you can install it from source using cargo:
```bash
cargo install --git https://github.com/artrixdotdev/tortillas
```

This will install `tortillas` to your local Rust toolchain.


### Libtortillas
Libtortillas is the library that powers the frontend TUI application. It is a library that can be used to build your own frontend application or integrate with existing frontend applications.

```bash
cargo add --git https://github.com/artrixdotdev/tortillas libtortillas
```

#### Runtime Contract

`libtortillas` is a Tokio-first library. Applications that use it must run
engine and torrent operations inside a Tokio runtime.

For the Tortillas TUI, the binary should own a single application runtime,
typically through `#[tokio::main]`, and create `libtortillas::engine::Engine`
inside that runtime. UI rendering or terminal input that blocks should run on a
dedicated thread or, for bounded work, through Tokio blocking tasks, then send
commands into async engine tasks. Long-lived input loops should use a dedicated
thread because `spawn_blocking` tasks cannot be aborted once they start. The
library does not currently support swapping in a different async runtime, HTTP
client, clock, listener, or storage executor.

## 🤝 Contributing

We welcome contributions! If you'd like to help improve `tortillas`, please check out our [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines and tips.

## 📊 Analytics

![Repobeats Analytics](https://repobeats.axiom.co/api/embed/2937f666319e74a9467ef1d5442edf89beabd516.svg "Repobeats analytics image")
