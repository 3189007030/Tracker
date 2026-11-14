# 1.2.1 validation

Status: offline correctness gates and a post-deployment verification round passed. One 1.2.1 release was promoted on S1 in two cutovers on 2026-11-14 — `20:35:10Z` to `/opt/tracker-v1/releases/1.2.1-3cf8ea82` (commit `3cf8ea8`) and `20:46:54Z` to `/opt/tracker-v1/releases/1.2.1-4ea93a47` (commit `4ea93a4`), the second fixing an own-service aliasing defect found by the first — replacing `1.2.0-b3acc310`. No rollback was required. This ledger records one promotion; it is not an endurance result, a capacity measurement or evidence of admission to any third-party list. The [1.2.0 ledger](validation-1.2.0.md) remains the record of the community-report release.

## Scope

This release changes only how the observatory chooses a canonical endpoint among duplicates. The public endpoint and observation schemas, the API shape, the web assets and the asset version strings (`?v=1.2.0`) are unchanged.

- Health for canonical selection is judged per address family: an endpoint is working when any family has a fresh accepted announce, while a family whose latest check failed still counts against it. Judging by the single newest announce made a dual-stack hostname unhealthy whenever that check hit the family it cannot serve.
- A hostname is preferred over a literal address as the public canonical URL, and the previous canonical is kept while it is still a healthy candidate, so evidence changes do not make the public URL flip between observation cycles.
- Hosts configured in `own_hosts` are excluded from duplicate detection entirely. The first cutover showed why: `udp://udp.augrains.com:6969/announce` was collapsed into the retired `udp://announce.augrains.com:6969/announce` because both resolve to the same address, which hid the configured public UDP entry point.

The workspace version was bumped to 1.2.1 as a patch release; no schema change accompanies it.

## Correctness gates

The gates ran in `/root/tracker-120-gate/src` on S1, a copy separate from the production directories, with the pinned toolchain (rustc 1.99.1, cargo 1.99.1) and Cargo offline against the committed lockfile.

- `cargo fmt --all -- --check`: exit 0.
- `cargo test --workspace --locked --offline`: exit 0; 148 tests passed, 0 failed.
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`: exit 0.
- `cargo build --workspace --release --locked --offline`: exit 0.

Release binaries, SHA-256 prefix: `tracker-server` `3a6f665f…`, `observatory` `9aff34e7…`, `tracker-web` `e44b8167…`, `tracker-loadgen` `ad3708ae…`.

New observatory tests cover: a dual-stack hostname whose latest check is the missing family still keeping the hostname canonical; the canonical choice staying stable when later evidence would rank another hostname first; and configured own-service hosts never being aliased even when a retired name resolves to the same address. The three pre-existing alias tests (hostname/literal merge, failing hostname not absorbing a working literal IP, UDP duplicate merge) still pass.

## Deployment

One 1.2.1 release, two cutovers. The second is a defect fix, not a second release.

- 2026-11-14T20:35:10Z: `/opt/tracker-v1/current` → `releases/1.2.1-3cf8ea82` (commit `3cf8ea8`), replacing `1.2.0-b3acc310`. Readiness and version reported 1.2.1 on both ports.
- Live check after the first cutover: the alias table contained `udp.augrains.com → announce.augrains.com`, the public API search for `augrains` returned five rows without `udp.augrains.com`, and the overview reported 311 endpoints. The configured public entry point had been hidden behind a retired name.
- Fix: `refresh_aliases` receives the configured `own_hosts` and skips those hosts; a regression test covers the retired-name case.
- 2026-11-14T20:46:54Z: `/opt/tracker-v1/current` → `releases/1.2.1-4ea93a47` (commit `4ea93a4`). Readiness and version reported 1.2.1 on both ports.
- No configuration change was needed: `config.toml`, `sources.toml`, the Nginx site configuration and the systemd units are unchanged. The cutover replaced binaries, switched the symlink and restarted `tracker-v1.service`, `tracker-web.service` and `tracker-observatory.service`.
- Offline rollback backups: `/root/tracker-120-backup-3cf8ea82/` and `/root/tracker-120-backup-4ea93a47/` (directories 0700, `rollback.tar.gz` 0600), each holding two SQLite `backup()` copies with `integrity_check=ok`, the Nginx site configuration, `config.toml` and `sources.toml`. Both the previous 1.2.1 build and `1.2.0-b3acc310` remain available for a code-level rollback; 1.2.1 adds no schema, and the additive 1.2.0 table and optional JSON fields are ignored by 1.1.0, so older binaries remain compatible with the existing data.

## Post-deployment verification

Server-side, on S1, after the second cutover:

- Both `/readyz` endpoints returned HTTP 200 with `version=1.2.1`; `/healthz` and `/api/v1/overview` also report 1.2.1; all three application units and nginx were active.
- The private alias table holds five external duplicate pairs and no `augrains.com` host. `/api/v1/trackers?q=augrains` returns six rows including `udp://udp.augrains.com:6969/announce`, where the first cutover returned five without it.
- `/api/v1/overview` reports 310 endpoints / 287 services / 81 all / 47 standard / 20 best; `trackers.txt` has 47 lines. The endpoint count is 315 minus the five hidden aliases. The earlier 311 was 315 minus four aliases, one of which was the wrongly merged own host.

Canonical direction and stability:

- Five external pairs, all with a hostname as the canonical URL: `retracker.hotplug.ru` ← IPv6 literal `[2a04:ac00:1:3dd8::1:2710]`; `bt1.archive.org` ← `207.241.226.111`; `bt2.archive.org` ← `207.241.231.226`; `tracker.torrent.eu.org` ← IPv6 literal `[2a03:7220:8083:cd00::1]`; `opentracker2.lain.moscow` ← `opentracker.lain.moscow`. Each alias detail returns `302` to the canonical page with `lang` preserved, and API list results exclude the alias.
- The alias mapping written at the 20:47:30Z refresh (`updated_at` 20:47:30Z) still had the same five alias → canonical pairs when the table was read again at 20:53:06Z and when the next refresh advanced `alias_checked_at` past 20:57:35Z. A later cycle with unchanged evidence therefore did not flip any canonical, which is the defect this patch addresses: under 1.2.0 the hotplug pair had reversed direction between two consecutive refreshes.
- Alias detection still depends on new probes carrying `target_ip`; endpoints not re-probed since 1.2.0 and pairs without a healthy canonical are not merged. This round verified the mechanism and the stability rule, not a complete duplicate census.

## Explicit limits

- One verification round only. No endurance run, no public-path capacity test, and no multi-browser rendering matrix was performed. The 1.2.0 browser verification of the interactive list remains the reference for those page behaviors, and 1.2.1 changes no web asset.
- Alias detection needs a post-upgrade observation with `target_ip`; an endpoint that has not been re-probed can still appear duplicated until its next 30-minute cycle, and the mapping is recomputed at most every ten minutes.
- The canonical-stability rule keeps the previous canonical only while it is still healthy and only among healthy hostname candidates; a genuinely failing canonical is replaced.
- The own-host exclusion uses the configured `own_hosts` list; a retired name that is no longer listed there is not treated as an own service by this rule.
- GitHub publication status is unverified. The local tag and release directory must not be described as pushed until confirmed on the remote.
- No global latency improvement, maximum throughput increase, or third-party list admission follows from this deployment.
