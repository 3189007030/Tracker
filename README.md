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
- The List page filters as you type: after a short pause it fetches a server-rendered results fragment from the same origin and replaces only the results region. Without JavaScript the same GET form submits and renders a full page. Pagination offers numbered page links with ellipses, previous/next, a jump-to-page field and a 25/50/100 page-size selector; `q`, `protocol`, `page` and `limit` stay in the URL, and a filter or page change updates through fetch and the History API rather than a full reload.
- Duplicate endpoints for the same origin — a hostname URL and its literal-IP URL on the same scheme, port and path — collapse into one canonical endpoint. Only the canonical endpoint is published and probed; aliases are hidden from the list and feeds, keep their detail redirect, and are not probed, so the tracker origin is not announced to twice. Public endpoint JSON carries optional `alias_of`.
- Detail observations state failure reasons in localized terms with the status where one applies (`HTTP 429 · rate limited`, timed out, `DNS lookup failed`, `HTTP 502 · bad gateway`) instead of raw internal categories, and show the resolved public address the probe actually contacted. The address is the tracker's server address, not a peer address, and observation JSON carries it as optional `target_ip`; it distinguishes stale DNS or a changed address from a timeout. An observation whose result is no address for that family renders an em dash instead of unknown in the reply and reason columns.
- Endpoint family status (success, stale, unknown/untested, no address) describes the latest observation's freshness. 24h success rate is historical and counts only completed observations, with untested, locally failed and no-address observations outside the denominator; the pages say so and show the successful/completed count (for example 12/12), so a stale or untested family can display 100% without contradiction.

The live service is [Tracker Observatory](https://tracker.augrains.com/). Historical release material describes the deployment and test conditions of its own version, not this checkout's present status or performance. The current 1.2.3 release combines the observatory UI refresh, list reliability fixes and the existing 1.2.x public API; the current validation ledger is [1.2.3 validation](docs/validation-1.2.3.md), with the [changelog](CHANGELOG.md), [1.2.1 validation](docs/validation-1.2.1.md) and [1.2.0 validation](docs/validation-1.2.0.md) retained as release history. A build, a short test or a listing check does not establish sustained capacity or third-party acceptance.

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

Native hot-path changes and measured limits: [1.0.4 validation](docs/validation-1.0.4.md), [reference research](docs/optimization-1.0.4.md), and [daily request/state envelope](docs/billion-request-envelope.md).
