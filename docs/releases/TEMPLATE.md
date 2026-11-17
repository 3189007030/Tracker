# Release ledger template

Copy this file to `docs/releases/<version>.md` before the release commit. Replace every placeholder with evidence from that release. Keep each check row; write `not run` when it was not done. Do not convert a planned check into a pass.

## Status

`<candidate | promoted | superseded | rolled back | not promoted | incomplete record>`

Use `incomplete record` only for a historical release known to have been live despite missing evidence. New releases must not use it. For a promoted release, include the UTC promotion time and immutable release directory below.

## Scope

- Version: `<X.Y.Z>`
- User-facing change: `<concise summary>`
- Build inputs changed: `<paths, or none>`
- Migration / compatibility notes: `<details, or none>`
- Changelog entry: [`<version>`](../../CHANGELOG.md)

## Source

- Built commit (full hash): `<commit>`
- Workspace version from `Cargo.toml`: `<version>`
- `rustc --version`: `<exact output>`
- `cargo --version`: `<exact output>`
- Build host / clean gate directory: `<Linux host and path>`
- Lockfile state: `<committed Cargo.lock hash or commit reference>`

## Release gates

Record the exact command, exit code and test count where applicable. If a command was changed from the standard release command, include the exact command that ran.

| Gate | Command | Exit code | Result / test count |
|---|---|---:|---|
| Format | `cargo fmt --all -- --check` | `<0 / code / not run>` | `<result>` |
| Workspace tests | `cargo test --workspace --locked --offline -j 1` | `<0 / code / not run>` | `<passed / failed / ignored counts>` |
| Clippy | `cargo clippy --workspace --all-targets --locked --offline -j 1 -- -D warnings` | `<0 / code / not run>` | `<result>` |
| Release build | `cargo build --workspace --release --locked --offline -j 1` | `<0 / code / not run>` | `<result>` |

## Binaries

Use the full SHA-256 digest of each executable produced by the recorded build. Do not use a prefix or a hash from a different build.

| Binary | SHA-256 |
|---|---|
| `tracker-server` | `<64 hex characters or not built>` |
| `observatory` | `<64 hex characters or not built>` |
| `tracker-web` | `<64 hex characters or not built>` |
| `tracker-loadgen` | `<64 hex characters or not built>` |

## Promotion

- Promotion status: `<not promoted / promoted / rolled back>`
- Cutover time (UTC): `<YYYY-MM-DDTHH:MM:SSZ or not run>`
- Release directory: ``/opt/tracker-v1/releases/<version>-<commit8>/`` or `<not promoted>`
- Previous release directory: `<path or not recorded>`
- Backup location and verification: `<protected path, checksums, or not run>`
- Restarted units: `<unit names, or none>`
- Rollback target, if used: `<path and reason, or not applicable>`

## Post-cutover checks

Record a timestamp/result for each item. For checks not performed, write `not run` and keep the row.

| Check | Result |
|---|---|
| `/healthz` version | `<result or not run>` |
| `/readyz` on native listener (8081) | `<result or not run>` |
| `/readyz` on web listener (8080) | `<result or not run>` |
| `/api/v1/overview` | `<result or not run>` |
| Public tracker list | `<result or not run>` |
| Subscription routes and row/line counts | `<result or not run>` |
| Real announce, peer discovery, scrape and stopped cleanup | `<result or not run>` |
| Changed asset URLs and new `?v=` through public edge | `<result or not run>` |
| systemd state, restart counters and OOM counters | `<result or not run>` |

## Tag

- Annotated tag: `<vX.Y.Z or not created>`
- Tag target (full commit): `<built commit or not created>`
- Tag target is the built/promoted commit: `<yes/no/not applicable>`
- Remote verification: `<remote and ref result, or not pushed>`

## Limits

- Capacity / endurance / browser / third-party claims: `<what was not measured or established>`
- Other known limitations: `<details, or none>`

## Corrections

Append dated entries after promotion. Do not replace earlier statements.

- `<YYYY-MM-DD>`: `<original record and correction, evidence, and reason>`
