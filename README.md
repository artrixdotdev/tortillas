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

## Library API

`libtortillas` exposes high-level `Engine` and `Torrent` handles for frontend
applications. These handles keep the actor runtime behind the public API, so a
TUI, GUI, or service can use normal async methods and render from snapshots
without depending on actor internals.

```rust,no_run
use libtortillas::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
   let engine = Engine::builder()
      .output_path("downloads")
      .autostart(false)
      .build();

   let torrent = engine.add_torrent("magnet:?xt=urn:btih:...").await?;
   torrent.poll_ready().await?;
   torrent.start().await?;

   let snapshot = torrent.snapshot().await?;
   println!("{}: {:.1}%", snapshot.info_hash, snapshot.progress * 100.0);

   Ok(())
}
```

For UI rendering, prefer `Engine::snapshot` and `Torrent::snapshot` over direct
actor state. The snapshots are serializable, compact, and designed to remain a
stable frontend contract as the internal actor model evolves.

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
## 🤝 Contributing

We welcome contributions! If you'd like to help improve `tortillas`, please check out our [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines and tips.

## 📊 Analytics

![Repobeats Analytics](https://repobeats.axiom.co/api/embed/2937f666319e74a9467ef1d5442edf89beabd516.svg "Repobeats analytics image")
