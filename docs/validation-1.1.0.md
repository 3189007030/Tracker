# 1.1.0 validation

Status: offline correctness gates and a single post-deployment verification round passed. Deployed to S1 at 2026-11-12 21:51:08 UTC as `/opt/tracker-v1/releases/1.1.0-0aaf8c9e` (local commit `0aaf8c9`), replacing `1.0.4-a63434cf`; no rollback was required. This ledger records one promotion. It is not an endurance result, a capacity measurement or evidence of admission to any third-party list.

## Scope

This release publishes 24-hour announce totals on the home page, retires `announce.augrains.com` as a public name, collapses the ingress to two published hostnames, and reduces fixed per-packet/per-request cost. Native announce and scrape wire behavior is unchanged from the reviewed 1.0.4 baseline; the announce and scrape responses are now written into a reused buffer, with a byte-for-byte equivalence test against the previous encoder.

## Correctness gates

The gates ran in `/root/tracker-110-gate` on S1, outside the production directories. They used the pinned toolchain and Cargo offline.

- `cargo fmt --all` was applied first: the contributor host has no Rust toolchain, so the committed sources had never been formatted. It rewrote three files — `crates/bt-protocol/src/http.rs`, `crates/tracker-server/src/main.rs` and `crates/tracker-web/src/store.rs` — after which `cargo fmt --all -- --check` passed. The three files were returned to the local checkout, and local sources match the build sources.
- `cargo test --workspace --offline` inside a fresh network namespace with only loopback (`unshare -n`, `ip link set lo up`): exit 0, 196.2 s, 0 failed.
- `cargo clippy --workspace --all-targets --offline -- -D warnings`: exit 0, 68.7 s.
- `cargo build --workspace --release --offline`: exit 0, 416.4 s.

Release binaries, SHA-256 prefix: `tracker-server` `8d112078…`, `observatory` `6a5d4f59…`, `tracker-web` `d8f709ec…`, `tracker-loadgen` `62b88abe…`.

These are source/runtime checks. They do not establish external capacity, endurance, third-party acceptance or monitor behavior.

## Deployment and rollback material

- Cutover at 2026-11-12T21:51:08Z; `/opt/tracker-v1/current` now points at `/opt/tracker-v1/releases/1.1.0-0aaf8c9e`. The preceding release `1.0.4-a63434cf` remains in place as the code rollback target.
- Offline rollback backup `/root/tracker-110-backup-0aaf8c9e/` (directory 0700, archive 0600): two SQLite `backup()` copies with `integrity_check=ok`, the Nginx site configuration, `config.toml`, `sources.toml` and `rollback.tar.gz`.
- `config.toml` was patched line by line rather than overwritten: only `own_hosts`, `service_aliases` and `announce_base_url` changed. The deployment-specific keys `native_stats_url`, `public_base_url` and `concurrency` were preserved; overwriting the file with the example configuration would have dropped listener addresses and similar settings.
- Nginx: `server_name` collapsed to `tracker.augrains.com` and `udp.augrains.com`, and a 443 `default_server` returning `444` was added. Unmatched hostnames are closed instead of being served by the tracker site.
- Native registry state is volatile and was reset by the restart. The process-lifetime announce counter therefore restarted from zero; it is not comparable to the previous 15-hour registry.

## Post-deployment verification

Local, on S1:

- `8081` and `8080` both answer `/readyz` with HTTP 200 and `version=1.1.0`; `/healthz` returns 200.
- `/stats.json` exposes the complete `diagnostics` set (five keys).
- All four units (three application services plus Nginx) are active.
- Journal error count over the preceding hour: 0.

Public, through Cloudflare:

- `/healthz` 200 `1.1.0`; `/api/v1/overview` 200 `1.1.0` with endpoints 312, services 290, all 87, standard 52, best 0.
- `/trackers.txt` returns 52 lines.
- The home page returns 200 and no longer contains `announce.augrains.com` (0 occurrences).

Host routing:

- `tracker` 200, `udp` 200.
- `announce` completes the TLS handshake and the connection is then closed — the 444 catch-all in effect, not a protocol reply.

Home-page 24-hour row as rendered:

- Accepted announces over 24 hours 0.2 亿; 24-hour mean 256.8/s; registered peers 6.2 万; swarms 5.1 万.
- The page states its own coverage: 23.9 hours of samples, not a complete 24 hours.
- Composition: HTTP 0.1 亿, UDP 0.1 亿, IPv6 312.9 万.
- The since-start figure (10.8 万) is a footnote and is explicitly not mixed with the 24-hour window.

## Explicit limits

- One verification round only. No one-hour or 24-hour endurance run, and no public-path capacity test, was performed for this release.
- A billion daily requests remains a preparation target, not a measured capability. A single promotion round says nothing about sustained load.
- The 24-hour window is derived from recorded minute samples: adjacent samples are differenced only while the instance identifier matches and the gap stays under 300 seconds, so restarts and collection outages are excluded rather than counted as zero. The displayed coverage (23.9 hours) is the actual span, not an assumption of a full day.
- Historical aggregate failure counters cannot be reconstructed. The fixed diagnostic counters start at process launch; nothing here backfills the previous aggregates.
- GitHub publication status is unverified. Local tags and archives must not be described as pushed until confirmed on the remote.
- No global latency improvement, maximum throughput increase, or third-party list admission follows from this deployment.
