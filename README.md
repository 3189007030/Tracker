# Tracker Observatory

A Rust BitTorrent tracker and observatory. The native tracker serves HTTP and UDP announces from a shared, sharded in-memory swarm. The separate observatory measures public trackers and publishes a read-only view for the bilingual website.

[简体中文](README.zh-CN.md)

The workspace contains `tracker-server`, `observatory`, `tracker-web`, and `tracker-loadgen`, plus shared protocol, state and model libraries. The load generator is an operator tool; the other three are independent services.

## Behavior and limits

- Announce replies contain only genuinely registered peers for the same resource in this tracker. The announce path does not query other trackers. Scrape counts are local; registrations and completion counters are volatile and reset with the relevant swarm or process.
- The observatory checks IPv4 and IPv6 independently. HTTP status, tracker protocol result and local failure are separate facts. Unknown observations remain unknown; empty lists are not padded.
- Peer handshakes and bitmaps are sampled without requesting file pieces. A successful handshake or claimed bitmap does not prove that content is downloadable.
- Peer IP addresses and ports are shared with other clients announcing the same resource. The native tracker retains registrations in memory; the observatory retains private measurement evidence. No anonymity or legal immunity is promised. The public website and API do not expose individual samples, resource magnets, raw peer addresses or ordinary client resource hashes.
- Samples are collected in two bounded pools; counts reflect source availability. A source outage is not filled with invented samples. Forum source settings are deployment-specific and must remain private.
- The website provides Home, List, Sources and Statement pages, observations and text/JSON/RSS feeds. The website reads published aggregate minute records; it does not query native statistics for every visitor.

The live service is [Tracker Observatory](https://tracker.augrains.com/). Historical [1.0.0](https://tracker.augrains.com/releases/v1.0.0/), [1.0.1](https://tracker.augrains.com/releases/v1.0.1/) and [1.0.2](https://tracker.augrains.com/releases/v1.0.2/) release material describes those particular deployments, not this checkout's present status or performance. A build, a short test or a listing check does not establish sustained capacity or third-party acceptance.

## Build and verify

Install Rust via rustup; `rust-toolchain.toml` pins the toolchain and components. The bundled SQLite build also requires a C compiler and standard build tools. From the repository root:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --release --locked
```

Tests use fixtures and loopback mocks and do not require a live tracker or the production host. Dependency installation requires access to the configured Rust registry. Do not load-test a third-party service.

Configuration examples are in `config/`. For a deployment, copy and adapt the examples, inspect network bindings and source settings, and follow [deployment](docs/deployment.md), [API](docs/api.md) and [release packaging](docs/release-packaging.md). Some example configuration keys and site URLs are currently instance-specific; see the [open-source review](docs/open-source-review.md) before using a different domain. Do not run two observatory writers against the same data directory.

See [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md), [the open-source review](docs/open-source-review.md) and [third-party notices](docs/third-party-notices.txt). Project code is licensed under [MIT](LICENSE); vendored components have their own notices.
