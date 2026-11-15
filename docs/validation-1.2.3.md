# 1.2.3 validation

Status: candidate. The source and UI changes are recorded in this ledger; no production promotion is claimed until the locked workspace gates and post-cutover checks below are completed. This document must be updated with the actual source commit, binary hashes, deployment path and verification results before describing 1.2.3 as live.

## Scope

This patch release combines the public observatory interface refactor with reliability fixes that do not change the public JSON schema:

- responsive, bilingual navigation and an editorial card/table system for home, list and detail pages;
- progressive list filtering and pagination that preserve `q`, `protocol`, `page`, `limit`, language and browser history;
- cancellation of stale list requests and consistent sanitised pagination totals/pages;
- chart and timeline fallback states with localized user-facing error labels, including the neutral `peers_unusable` category;
- cache-busting of all changed web assets to `?v=1.2.3`.

The public API routes and the no-JavaScript GET forms remain available. No database migration or configuration change is required by these changes.

## Checks completed before promotion

- `node --check crates/tracker-web/assets/list.js`: passed.
- `node --check crates/tracker-web/assets/charts.js`: passed.
- `node --check crates/tracker-web/assets/tools.js`: passed.
- `git diff --check`: passed.

The local Windows checkout did not have `cargo`, `rustc` or `rustup` on `PATH`, so the Rust gates must run on the pinned Linux builder before promotion. No Rust test, Clippy result or release binary is claimed by the checks above.

## Required release gates

Run from a clean source snapshot with Rust 1.99.1 and the committed lockfile:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked -j 1
cargo clippy --workspace --all-targets --locked -j 1 -- -D warnings
cargo build --workspace --release --locked -j 1
```

Record the exact source commit, toolchain output, test count and SHA-256 of all four release binaries here. A successful build alone does not establish production readiness or capacity.

## Deployment and post-cutover checks

Use the immutable release procedure in [deployment](deployment.md). Record the release directory, `/opt/tracker-v1/current` target, changed systemd units, rollback backup and the results of `/healthz`, `/readyz`, `/api/v1/overview`, the tracker list, feeds and a real announce smoke check. Verify that changed asset URLs return the new `?v=1.2.3` resources and that no stale asset is served by the edge cache.

## Explicit limits

This release does not claim a capacity increase, endurance result, global reachability, third-party list admission or a multi-browser rendering matrix. BrowserSkill setup is available, but a connected browser session is required for interactive verification; absence of that session must be recorded rather than inferred as a pass.
