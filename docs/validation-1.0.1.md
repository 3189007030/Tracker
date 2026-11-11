# Tracker 1.0.1 validation

This ledger describes 1.0.1 only. The original [validation ledger](validation.md) and [capacity report](capacity-report.md) remain historical 1.0.0 evidence, not a new capacity measurement.

## Implemented behavior

- Cross-Tracker mediation and the old experiment service, configuration, APIs and UI have been removed. Native HTTP/HTTPS/UDP announces return genuine same-resource registrations from the shared local engine. The observatory's scheduled public-sample probes remain separate from client announces.
- Three Rust services run under systemd: native tracker, observatory and website. The fourth executable is the owner-only load generator. Nginx supplies TLS and ingress. No new database service or frontend framework was added.
- Four bilingual pages provide local icons/flags, small copy actions, client-rendered Chart.js graphs, bounded aggregate history, and selected-sample/anonymous-node provenance. Ordinary client hashes and peer addresses are not added to the public index.
- Each daily sample pool has a target and cap of 100. Deduplication selects official resources first, forum resources second, then TPB supplements the public pool. Available pools have equal selection probability; unavailable pools do not create fake tests. Host/resource due times and backoffs remain binding.

## Offline and independent gates

Formatting and Clippy with warnings denied passed. All **116 workspace tests** passed with Cargo offline in a fresh network namespace containing only loopback at 2026-11-11 06:04 UTC. The cases cover protocol boundaries, true HTTP/UDP discovery, bounded engine state, source parsing, sample linkage/privacy, pool freshness and address-family scheduling. See `benchmarks/v1.0.1-offline-gate.json`.

The separate standard-library wire checker passed IPv4 and IPv6 against isolated loopback instances, including malformed bytes, UDP cookies, same-hash discovery, stopped cleanup, non-mutating scrape and unrelated-resource isolation. Each family used about 21 KiB and under a second of protocol activity; its synthetic state never entered production. See `benchmarks/v1.0.1-wirecheck.json`.

Linux release and Windows load-generator builds use the pinned Rust 1.99.1 toolchain and committed dependency lock. The release-specific deployment, external protocol and browser receipts are maintained under `benchmarks/v1.0.1-*.json`.

## Deployment and short external checks

The final code build `09e8dd10556b294573a42122b09a7b88b94b8892` was deployed at **2026-11-11 06:06:33 UTC**. Both readiness routes reported 1.0.1; all three services were active and the experiment unit was absent. A consistent database/configuration backup was checksum-verified off-host before cutover. The native executable and running process were preserved during this final observatory/website update; its registry was not reset.

Windows Rust checks passed HTTP and HTTPS for both address families, plus IPv6 UDP, including zero-want registration, exact two-way peer discovery, stopped removal, scrape 2→1→0 and unrelated-resource isolation. **Windows IPv4 UDP repeatedly timed out and is not marked passed.** A 14-second identifier-free server trace observed the owned requests and immediate replies, including a compact peer response before the client timeout. A separate standard-library client from another owned external IPv4 network passed the complete UDP discovery/scrape/cleanup sequence in 0.011 seconds, using 1,424 application bytes. This establishes functionality on that path; it does not explain or cure the Windows-path failure.

All temporary route checks confirmed cleanup and independent SSH access afterward. One very short retry collected zero periodic monitor samples, so its continuous PID/OOM flags are unknown; the receipt preserves that monitoring limitation. No server configuration or service was changed on the second test host, and its temporary peer registrations were removed.

Browser checks covered all four pages in Chinese/English, the desktop and 390px mobile layout, representative icon-only copy actions and actual clipboard values, local flags, minimal sample magnets, anonymous node linkage and 24h/7d/30d history selectors. The corrected calendar axis was visibly checked. External asset bytes/MIME matched the SVG/PNG/ICO release files; their icon URLs are versioned. Both removed experiment API routes returned 404. See `benchmarks/v1.0.1-web-check.json`.

## Source availability

The refreshed public pool reached **100/100** after official sources and both TPB endpoints succeeded. The forum pool remained **0/100**. The legacy forum JSON adapter has been reimplemented without importing historical samples or restoring browser automation. Its last direct request returned HTTP 502; the next collection respected the existing source backoff and reported it deferred. Other old forum portals require script verification or prohibit crawling and are not bypassed. No claim is made that the forum target has been reached.

The public pool prioritizes official software/OS torrents. TPB items retain their real publisher classification and are not described as official software. Magnet-only entries can support announces and same-hash handshakes; missing torrent geometry leaves bitmap coverage unknown.

## Scope and limits

Only short correctness and page checks are authorized for this release. No one-hour or 24-hour endurance run was performed, and no new maximum or sustained request capacity is claimed. Earlier capacity evidence is historical and cannot establish the overhead of the new observation workload.

The original 20 GiB public-test accounting baseline is preserved; the active stop remains 18 GiB, including concurrent natural and control traffic. Temporary verification routes/listeners must be removed after the short checks.

NZ maintainer communication remains paused and unsent. This release does not claim third-party acceptance or stable-list placement.
