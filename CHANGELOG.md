# Changelog

## 1.2.9 — homepage layout, language and local time

- Give the homepage address and subscription panels equal width on desktop. Show compact subscription rows with direct filename links and copy buttons.
- Use the browser's preferred language for page requests without an explicit language. The language control preserves the current filters and fragment, and the browser-only announce redirect uses the same language selection as the homepage.
- Show snapshot, observation and source times in the browser's time zone, including chart labels and tooltips. Render labelled UTC times when client-side formatting is unavailable.

See the [1.2.9 validation ledger](docs/releases/1.2.9.md).

## 1.2.8 — native Tracker efficiency

- Borrow unescaped HTTP query parameters and keep the existing validation and percent-decoding rules.
- Return the other live endpoint directly for a registered requester in a two-peer swarm. Deduplicate large-swarm samples by peer slot while preserving the reply budget and selection policy.
- Look up endpoints directly in small swarms. Build the hash index at five peers and release it at three. Capacity limits and expiry rules retain their existing values.

See the [1.2.8 validation ledger](docs/releases/1.2.8.md) and the [isolated performance comparisons](docs/capacity/tracker-hotpath-2026-11-18.md).

## 1.2.7 — Tracker Observatory interface

- Refresh the homepage, tracker list, details, and source pages with coordinated light and dark palettes and a clearer layout for live metrics and historical evidence.
- Improve narrow-screen navigation, give the homepage metrics a two-column layout on phones, and keep wide tables scrollable inside their sections.
- Show the public snapshot timestamp and endpoint and service totals in the homepage summary. Update the stylesheet and menu script cache keys to `?v=1.2.7`.

See the [1.2.7 validation ledger](docs/releases/1.2.7.md) for build and deployment records.

## 1.2.6 — observatory status and retired endpoints

- Show the ghost pool's actual target of 16 samples and explain what its private diagnostics measure. The public resource pool still targets 100 samples.
- Remove the website's fixed online labels. Report partial 24-hour coverage without rounding it up to a full day, and count completed failures in the IPv4 and IPv6 detail totals.
- Treat the three retired `announce.augrains.com` URLs as aliases of their corresponding current entry points when those are configured and present. Keep their historical detail URLs resolvable and stop duplicating their probes and feed entries.

See the [1.2.6 validation ledger](docs/releases/1.2.6.md) for build and deployment records.

## 1.2.5 — number formatting and the ghost-sample record

- Fix English large-number formatting: the `k` tier divided by ten thousand, so 566,542 registered peers rendered as `56.4k`; it now renders `566.5k`. Chinese 万 / 亿 formatting was already correct.
- Make the charts follow the theme. Legend, grid, tick, tooltip and series colours come from the active palette and are repainted when the theme changes; they were fixed dark-theme values and nearly unreadable on the light palette.
- Restore text contrast on accent-coloured controls (primary buttons, the skip link, the current page marker) and on small secondary text in the light palette, and keep the sticky navigation bar from covering in-page anchor targets. List paging no longer smooth-scrolls when the visitor prefers reduced motion.
- Report `handshake_failed` in public observation `error_category`. The observatory produces it, but the website's allowlist dropped it, so it was published as `null`. No field is added or removed.
- Correct the record on ghost-sample poison detection. The 1.2.2 entry said a tracker that returns peers for a ghost info-hash is removed from the published feeds. That never happened: the verdict is written on the `sample_announce` request, publication looked for it among health `announce` events, and only the hard-coded domain rejection applied. The exclusion is deliberately **not** connected in this release, because the public snapshot shows the verdict firing on about a fifth of all endpoints, long-established trackers included; see [1.2.5 validation](docs/releases/1.2.5.md). The verdict remains a private diagnostic, the code now says so, and a test pins that it does not change feed membership or health figures.
- Changed web assets are served as `?v=1.2.5`.
- Read the release version from the archived commit in `validation/package_release.py` instead of a hard-coded `1.0.4`, with an optional `--expect-version` guard; the archived commit must contain its own `docs/releases/<version>.md`.

Validation and deployment facts are recorded in [1.2.5 validation](docs/releases/1.2.5.md); this entry describes source changes and does not claim a completed deployment.

## 1.2.4 — theme support, staged and not kept in production

- Add a dark/light theme that follows the system preference, can be switched manually and remembers an explicit choice. Hide the page scrollbar and show scroll depth in the navigation bar.
- The workspace version and four asset query strings changed to `1.2.4`. These interface changes had already been served by builds labelled 1.2.3. The S1 inventory suggests a brief 1.2.4 trial from 2026-11-16T03:56:47Z until the return to 1.2.3 at 03:57:07Z; the interim switch is inferred from restart times. See [1.2.4 validation](docs/releases/1.2.4.md).

## 1.2.3 — observatory UI refresh and list reliability

- Rebuild the public web interface around a calmer observatory layout with responsive navigation, clearer evidence hierarchy, tracker connection workflows and mobile-safe tables/cards.
- Preserve language and filter state across navigation, improve live-list request cancellation, and clamp sanitized pagination consistently so displayed counts and rows agree.
- Add chart/timeline fallback states and localize user-facing observation error labels without exposing internal error categories.
- Validation and deployment facts are recorded in [1.2.3 validation](docs/releases/1.2.3.md); this changelog entry does not claim promotion by itself.

## 1.2.2 — ghost sample pool for poison detection and relaxed HTTP compliance

- Replace the unconfigured forum sample pool with a deterministic ghost sample pool (`tracker-observatory:ghost:v1:sample-{i}`). Ghost samples use unpublished 20-byte info-hashes that should have zero peers on any legitimate tracker. If a tracker returns peers or seeders for any ghost info-hash, it is flagged as a poisoned / fake-seeding node (`poisoned_peers`). The flag has been recorded from this release on, but the disqualification from the published feeds that this entry originally announced was never connected; see the 1.2.5 entry.
- Reject known poisoned tracker domains such as `durukanbal.com` directly during normalization and feed publication.
- Relax HTTP `numwant=0` health compliance: conform to BEP 3 by accepting responses that omit the `peers` dictionary key when `numwant=0` was requested, allowing legitimate implementations like opentracker (`tracker2.dler.org`) to be correctly recognized as accepted rather than marked `incomplete_protocol`.

## 1.2.1 — stable canonical endpoints

- Keep the canonical URL of a duplicate origin stable between observation cycles. The adapter previously ranked candidates by the single latest announce, so a dual-stack hostname whose newest check was the address family it cannot serve looked unhealthy and the canonical could alternate between the hostname and its literal IP; a hostname is now considered working when any announce in the 24-hour window was accepted, hostnames are preferred over literal addresses as the public URL, and the previous canonical is kept while it remains a healthy candidate. This changes only how aliases are chosen; the published endpoint and observation schemas are unchanged.
- Never alias the hosts this project operates. A live check after the first cutover showed `udp.augrains.com` being collapsed into the retired `announce.augrains.com` name because both resolve to the same address, which hid the configured public UDP entry point; `refresh_aliases` now receives the configured `own_hosts` and skips them. A regression test covers the retired-name case.
- No web asset changed, so static asset version strings stay at `?v=1.2.0`.
- Validation and deployment receipts for this cutover are tracked in [1.2.1 validation](docs/releases/1.2.1.md); the 1.2.0 ledger remains the record of the first promotion.

## 1.2.0 — interactive list, canonical endpoints and diagnostic observation detail

- Filter the tracker list while typing. A short debounce fetches a server-rendered results fragment from the same origin and replaces the results region. This is progressive enhancement: the GET form still submits and renders a full page without JavaScript, and the active filter stays in the URL.
- Rebuild pagination as numbered page links with ellipses, previous/next controls, a jump-to-page number input and a page-size selector (25/50/100). `page` and `limit` are preserved together with `q` and `protocol`, so changing the page or page size does not drop the active filter.
- Update a filter or page through fetch and the History API instead of a full page reload: only the results region is replaced, the rest of the document stays, and back/forward navigate the recorded states.
- Localize detail-page failure reasons and attach the status that caused them. An HTTP 429 reads `HTTP 429 · 频率限制` (rate limited) and an HTTP 502 reads `HTTP 502 · 网关错误` (bad gateway), alongside `超时` and `DNS 解析失败`, instead of a raw internal category string; HTTP status, protocol result and local failure remain separate facts.
- Render an em dash in the 有效 Tracker 回复 and 原因 columns when an observation's result is 未发现地址 (no address for that family), instead of 未知 or an internal code; the 目标 IP column shows the same dash when no address was contacted.
- Collapse duplicate endpoints of the same origin into one canonical endpoint. A hostname URL and its literal-IP URL on the same scheme, port and path announce to the same origin, so only the canonical endpoint is published and probed; aliases are hidden from the list and feeds, keep their detail redirect, and are not probed again, so the tracker does not receive a duplicated announce. Public endpoint JSON gains optional `alias_of`.
- Expose optional `target_ip` on observations: the resolved public address the probe actually contacted, shown in a 目标 IP column in the detail observation log. A stale DNS record or a changed address is now distinguishable from a timeout. It is the tracker's server address, not a peer address, and only public addresses are recorded.
- Separate freshness from historical success in what the pages say. Family status pills (成功/已过期/未知 / 未测/未发现地址) describe the latest observation's freshness, while 24h 成功率 is historical and counts only completed observations, with untested, locally failed and no-address observations outside the denominator. Help text, tooltips and the successful/completed count behind the rate (for example 12/12) state this explicitly, so a stale or untested family can show 100% without contradiction.
- Bump the version to 1.2.0 as a minor release: the endpoint and observation schemas gain additive optional fields, and list/detail behavior changes; existing names and meanings are unchanged.

Validation and deployment receipts are tracked separately in [1.2.0 validation](docs/releases/1.2.0.md); this entry describes the source change and does not claim a completed deployment.

## 1.1.0 — 24-hour runtime framing, single public hostname and hot-path work

- Publish 24-hour announce totals on the home page instead of the counter that grows with process uptime. `native_summary` now also returns `announces_24h`, `http_announces_24h`, `udp_announces_24h`, `accepted_ipv6_24h`, `covered_seconds_24h` and `rate_24h`, derived from recorded minute samples: adjacent samples are differenced only while the instance identifier matches and the gap stays under 300 seconds, so restarts and collection outages are excluded rather than counted as zero. The since-start figure is retained and demoted to a footnote.
- Retire `announce.augrains.com` as a public name. The site advertises `https://tracker.augrains.com/announce` (HTTPS), `http://tracker.augrains.com/announce` (HTTP) and `udp://udp.augrains.com:6969/announce`; owned-host and alias configuration collapses to the two published names. UDP carries no hostname, so UDP clients are unaffected. The TLS catch-all now closes unmatched hostnames instead of serving the tracker site on them.
- Reduce fixed per-packet and per-request cost: hot counters are grouped by writer to stop cache-line sharing between the UDP and HTTP paths, the service-time bucket lookup replaces its linear scan, business-failure replies use static text instead of formatting a `String`, and HTTP announce/scrape replies are written straight into a reused buffer with a byte-for-byte equivalence test against the previous encoder.
- Rebuild the public site's information hierarchy around the 24-hour window, unify large-number and rate formatting (万/亿 in Chinese, k/M/B in English), make wide tables scroll on narrow screens with sticky headers, and state explicitly when a subscription list is empty because evidence is insufficient.

Historical aggregate failures still cannot be backfilled, and a billion daily requests remains a workload-dependent preparation target rather than a measured capability. Validation and deployment receipts are recorded in [1.1.0 validation](docs/releases/1.1.0.md).

## 1.0.4 — bounded native hot paths and rejection diagnostics

- Reuse keyed HMAC state and validate one authenticated time slot per UDP connection ID, preserving source binding, wire format and the existing validity window.
- Reuse per-socket UDP response buffers; keep unauthenticated invalid IDs silent and response sizes bounded.
- Skip timing-index removal/reinsertion when a peer refresh leaves its deadline unchanged; retain role, expiry and peer-discovery behavior.
- Expose fixed, privacy-safe counters for HTTP parse failures, UDP malformed packets, cookie validation stages and individual engine refusal reasons. Historical aggregate failures cannot be backfilled.
- Increase bounded Nginx connection reuse while keeping announce/scrape uncached and origin TLS ticket policy unchanged.

Validation and short comparative measurements are recorded in [1.0.4 validation](docs/releases/1.0.4.md). A billion daily requests remains a workload-dependent preparation target.

## 1.0.3 — open-source preparation

- Remove built-in forum navigation locations; load optional exact HTTPS roots from private source configuration. Unconfigured sources perform no network requests, retain their stable stored identifiers, and report a configuration shortfall.
- Add a compact GitHub repository link next to the language selector, bilingual contribution/security guidance, and a locked Rust CI workflow.
- Remove unused configuration example keys and document remaining deployment portability, UDP candidate-selection and collection-timeout findings in the open-source review.
- Publish only a reviewed current-source snapshot with a new Git root; keep legacy development history and private operational configuration out of the new repository.

Validation, publication and deployment are tracked separately in [1.0.3 validation](docs/releases/1.0.3.md); preparation does not imply that these steps are complete.

## 1.0.2

- Replace the old home-page hero and summary cards with four history charts and minute-sampled native totals, followed by native addresses and six text subscriptions; arrange the address and subscription sections side by side on desktop and stacked on mobile.
- Use Home, List, Sources and Statement navigation, compact ranked tracker rows without a repeated address label, concise infrastructure details, and independent IPv4/IPv6 observation timelines. The former status route redirects to the home page.
- Remove public individual-sample and magnet APIs, pages and legacy public tables while retaining private observation pools, handshake/bitmap sampling and list eligibility rules.
- Read home-page native totals from the latest published minute, marking data older than 180 seconds unknown; visitor requests do not poll the native service.

Deployed with 117 offline tests and short isolated/browser checks; validation and deployment are tracked in [1.0.2 validation](docs/releases/1.0.2.md). No new throughput or endurance result is claimed.

## 1.0.1

Deployed with 116 offline tests and short functional validation; receipts and limitations are tracked in [1.0.1 validation](docs/releases/1.0.1.md).

- Return only genuinely registered same-resource local peers; native registrations and scrape counts remain local.
- Use daily, capped forum and public sample pools, equal pool selection and traceable observations, with public trackerless sample magnets and private ordinary-client hashes. Forum collection currently reports HTTP 502 and zero entries.
- Provide four bilingual pages, SVG icons, copy actions, country flags, local Chart.js 4.6.1 and detailed observation history.

No long-duration capacity, endurance result or third-party acceptance is implied by this release.

## 1.0.0 — Rust rebuild

- Rebuild the native tracker, observatory, public APIs and website from protocol and behavior contracts.
- Replace the prior RPC and storage layout; start all observation evidence from a clean dataset.
- Keep real peer exchange, dual-stack diagnostics, zero-payload verification and gated subscriptions.
- Replace the composite score with explicit evidence dimensions.
- Deploy S1 as a dedicated project host and publish measured capacity with explicit limits.

Released with short-test validation at the user's request. One-hour and 24-hour endurance remain unverified; see `docs/releases/1.0.0.md` and `docs/capacity/capacity-report.md`.
