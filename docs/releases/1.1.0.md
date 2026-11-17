# 1.1.0 validation

Status: offline correctness gates and a post-deployment verification round passed. One 1.1.0 release was promoted in three cutovers on S1 — 2026-11-12T21:51:08Z (`1.1.0-0aaf8c9e`, commit `0aaf8c9`), 22:33:29Z (`1.1.0-c5fc4cab`, commit `c5fc4ca`) and 22:49:32Z (`1.1.0-d0c873df`, commit `d0c873d`) — the latter two repairing a web-layout defect and its static-asset cache busting. No rollback was required. This ledger records one promotion. It is not an endurance result, a capacity measurement or evidence of admission to any third-party list.

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

This is one 1.1.0 release promoted in three cutovers. The second and third cutovers repair a web-layout defect and its static-asset cache busting; they are not separate releases.

- First cutover, 2026-11-12T21:51:08Z: `/opt/tracker-v1/current` → `/opt/tracker-v1/releases/1.1.0-0aaf8c9e` (commit `0aaf8c9`), replacing `1.0.4-a63434cf`.
- Layout defect found after the first cutover: the new home-page KPI block introduced eleven class names — `kpi`, `kpi-section`, `kpi-strip`, `kpi-label`, `kpi-note`, `kpi-detail`, `kpi-coverage`, `kpi-total`, `trends-unit`, `chart-empty`, `feed-empty` — for which `assets/style.css` contained no rules at all. Labels, values and footnotes collapsed onto one line with large gaps between rows. In addition, the `.copy-row label` selector no longer matched the new `<span class="copy-label">` markup, so address labels lost their styling.
- Fix: the missing rules were added — the KPI block became a four-column grid, two columns at ≤860px and one column at ≤460px; the three explanatory notes span `grid-column: 1 / -1` so each occupies its own row; `.copy-label` was styled. After writing the file, the class-coverage audit dropped from 11 missing classes to 0 (all 86 classes now have rules).
- Second cutover, 2026-11-12T22:33:29Z: release `1.1.0-c5fc4cab` (commit `c5fc4ca`, including the CSS fix). The page still rendered the old layout: the static-asset version string had not changed (still `?v=1.1.0`), and Cloudflare serves `/assets/*` with `max-age=14400`, so the request was served from cache (`cf-cache-status: HIT`, 17,553 bytes) while the origin already carried 18,998 bytes.
- Third cutover, 2026-11-12T22:49:32Z: release `1.1.0-d0c873df` (commit `d0c873d`), bumping the `style.css` and `charts.js` version string to `?v=1.1.1`. Verified afterwards: `/assets/style.css?v=1.1.1` returns 200, 18,998 bytes, contains `.kpi-section`, and reports `cf-cache-status: MISS` (served from origin); the home-page HTML references only `1.1.1`.
- Offline rollback backups, one per cutover: `/root/tracker-110-backup-0aaf8c9e/`, `/root/tracker-110-backup-c5fc4cab/` and `/root/tracker-110-backup-d0c873df/`. Each holds two SQLite `backup()` copies with `integrity_check=ok`, the Nginx site configuration, `config.toml`, `sources.toml` and `rollback.tar.gz` (directory 0700, archive 0600). `current` now points at `releases/1.1.0-d0c873df`.
- `config.toml` was patched line by line rather than overwritten: only `own_hosts`, `service_aliases` and `announce_base_url` changed. The deployment-specific keys `native_stats_url`, `public_base_url` and `concurrency` were preserved; overwriting the file with the example configuration would have dropped listener addresses and similar settings.
- Nginx: `server_name` collapsed to `tracker.augrains.com` and `udp.augrains.com`, and a 443 `default_server` returning `444` was added. Unmatched hostnames are closed instead of being served by the tracker site.
- Native registry state is volatile and was reset by the restart. The process-lifetime announce counter therefore restarted from zero; it is not comparable to the previous 15-hour registry. The two later cutovers changed only web assets and their references.

## Post-deployment verification

Local, on S1 (re-checked after the third cutover):

- `8081` and `8080` both answer `/readyz` with HTTP 200 and `version=1.1.0`; `/healthz` returns 200.
- `/stats.json` exposes the complete `diagnostics` set (five keys).
- `own_hosts` and the Nginx `server_name` contain only `tracker.augrains.com` and `udp.augrains.com`.
- All four units (three application services plus Nginx) are active.
- Journal error count over the preceding hour: 0.

Public, through Cloudflare:

- `/healthz` 200 `1.1.0`; `/api/v1/overview` 200 `1.1.0` with endpoints 312, services 290, all 87, standard 52, best 0.
- `/trackers.txt` returns 52 lines.
- The home page returns 200 and no longer contains `announce.augrains.com` (0 occurrences).

Host routing:

- `tracker` 200, `udp` 200.
- `announce` completes the TLS handshake and the connection is then closed — the 444 catch-all in effect, not a protocol reply.

Rendered output, reviewed in a browser on the live site:

- The home-page KPI block is four aligned cards: label, 26px value, and small footnote on separate lines. The three explanatory notes each occupy their own row.
- On the `/trackers` list page, flags, addresses, IPv4/IPv6 badges, success rate and latency columns line up, and each row's copy control is grouped with its text.

Home-page 24-hour row as rendered:

- Accepted announces over 24 hours 0.2 亿; 24-hour mean 256.8/s; registered peers 6.2 万; swarms 5.1 万.
- The page states its own coverage: 23.9 hours of samples, not a complete 24 hours.
- Composition: HTTP 0.1 亿, UDP 0.1 亿, IPv6 312.9 万.
- The since-start figure (10.8 万) is a footnote and is explicitly not mixed with the 24-hour window.

## Static-asset cache busting

When an asset's content changes but the URL version string does not, an edge cache keeps distributing the old file. The second cutover is the concrete case: the origin already served 18,998 bytes while Cloudflare returned a cached 17,553 bytes under the unchanged `?v=1.1.0`. Any change under `assets/` must bump the `?v=` string at every reference site in the same change; a deploy alone does not invalidate the edge copy.

## Explicit limits

- One verification round only. No one-hour or 24-hour endurance run, and no public-path capacity test, was performed for this release. The three cutovers are one promotion plus its defect repair, not three releases.
- A billion daily requests remains a preparation target, not a measured capability. A single promotion round says nothing about sustained load.
- The 24-hour window is derived from recorded minute samples: adjacent samples are differenced only while the instance identifier matches and the gap stays under 300 seconds, so restarts and collection outages are excluded rather than counted as zero. The displayed coverage (23.9 hours) is the actual span, not an assumption of a full day.
- Historical aggregate failure counters cannot be reconstructed. The fixed diagnostic counters start at process launch; nothing here backfills the previous aggregates.
- GitHub publication status is unverified. Local tags and archives must not be described as pushed until confirmed on the remote.
- No global latency improvement, maximum throughput increase, or third-party list admission follows from this deployment.
