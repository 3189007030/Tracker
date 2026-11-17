# Tracker Observatory

Tracker Observatory is a Rust workspace with three independent services: `tracker-server`, a native BitTorrent tracker whose HTTP and UDP announces share a sharded in-memory swarm; `observatory`, which probes public trackers and publishes read-only data; and `tracker-web`, the bilingual website and public API. `tracker-loadgen` is an operator load generator; the workspace also has shared protocol, state and model libraries. Live service: [Tracker Observatory](<https://tracker.augrains.com/>).

[简体中文](<README.zh-CN.md>)

## What it does

- **Native tracker:** HTTP/UDP announces and local scrape counts.
- **Observatory:** public-tracker probes and private measurement evidence.
- **Website and subscriptions:** Home, List, Sources and Statement pages; native statistics come from published minute aggregates, not a poll per visitor. Six feeds — `trackers.txt`, `trackers_best.txt`, `trackers_all.txt`, `trackers_udp.txt`, `trackers_http.txt`, `trackers_https.txt` — have txt/JSON/RSS representations. Feeds select by endpoint evidence, one entry per service; insufficient evidence can leave them empty.
- **Public API:** endpoint status, observations and aggregate history; see the [API contract](<docs/api.md>).

## What it does not do / boundaries

- Announces return only peers genuinely registered here for the same resource, without querying other trackers. Registrations and completion counters are volatile and reset with the relevant swarm or process.
- Unknown stays unknown; empty lists are not padded. Two bounded sample pools reflect source availability, and outages are not filled with invented public samples. Forum-source settings stay private. Ghost-sample verdicts are private diagnostics, not feed gates.
- Peer sampling requests no file pieces. A successful handshake or claimed bitmap does not prove downloadability.
- Peer IPs and ports go to other clients announcing the same resource. No anonymity or legal immunity is promised. Individual samples, resource magnets, raw peer addresses and ordinary-client resource hashes are not public.
- A build, short test or listing check proves neither sustained capacity nor third-party acceptance. Historical release evidence applies only to its recorded deployment and test conditions, not the current checkout's status or performance.
- Do not load-test third-party services or run two observatory writers against the same data directory.

## Public data conventions

- IPv4 and IPv6 are judged independently. HTTP status, tracker protocol result and local failure are separate facts.
- Family status describes the latest observation's freshness; 24h success rate is historical and counts completed observations only, excluding untested, locally failed and no-address observations. A stale or untested family can therefore show 100%.
- A hostname URL and its literal-IP URL on the same scheme, port and path merge into one canonical endpoint. Only it appears in lists and feeds and is probed; aliases keep their detail-page redirect and optional `alias_of`.
- Optional observation `target_ip` is the contacted public tracker-server address, not a peer address. It helps distinguish stale DNS or an address change from a timeout.

## Build and verify

Install Rust via rustup; [rust-toolchain.toml](<rust-toolchain.toml>) pins the toolchain and components. Bundled SQLite also requires a C compiler and standard build tools. From the repository root:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --release --locked
```

Tests use fixtures and loopback mocks, not live trackers or the production host. Dependency installation requires access to the configured Rust registry.

## Configuration and deployment

Copy and adapt the [configuration examples](<config/>), checking bindings and source settings. Some example keys and site URLs are instance-specific; read [deployment](<docs/deployment.md>), [API](<docs/api.md>), [release packaging](<docs/release-packaging.md>) and the [open-source review](<docs/open-source-review.md>) before using another domain.

## Documentation

Start with the [documentation index](<docs/README.md>).

- [CHANGELOG.md](<CHANGELOG.md>) — source changes.
- [Release records](<docs/releases/README.md>) — each version's validation and deployment evidence.
- [Release process](<docs/release-process.md>) — how versions are recorded and published.
- [Capacity studies](<docs/capacity/>) — methods, measured limits and request/state envelopes.

A deployment's actual running version is the one its `/healthz` reports.

## Contributing, security and license

See [contributing](<CONTRIBUTING.md>), [security](<SECURITY.md>) and [third-party notices](<docs/third-party-notices.txt>). Project code is licensed under [MIT](<LICENSE>); vendored components have their own notices.
