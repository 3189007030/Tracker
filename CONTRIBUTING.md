# Contributing

Issues and pull requests are welcome for reproducible defects, documentation corrections and focused improvements. Describe the trigger, observed behavior and expected behavior; include a small test or fixture when it is useful. Please read [SECURITY.md](SECURITY.md) before reporting a vulnerability.

## Development

Use the pinned `rust-toolchain.toml`, a C compiler for bundled SQLite, and the committed `Cargo.lock`. From the repository root:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --release --locked
```

The tests should use fixtures or loopback mocks and must not depend on the production service or third-party trackers. To verify cached dependencies without registry access, run `cargo test --workspace --locked --offline` after fetching them. Do not add traffic tests against services you do not operate.

Keep protocol behavior and public data contracts explicit. Preserve the difference between local registrations, private measurement evidence and public aggregates. Do not include credentials, private source origins, raw peer addresses, real resource identifiers, production databases or unredacted request logs in fixtures, commits or issue reports.

Update the relevant documentation and third-party notices when behavior or dependencies change. The [open-source review](docs/open-source-review.md) lists known findings and their evidence status; it is not a substitute for testing a proposed fix.
