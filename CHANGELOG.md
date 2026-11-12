# Changelog

## 1.1.0 — 24-hour runtime framing, single public hostname and hot-path work

- Publish 24-hour announce totals on the home page instead of the counter that grows with process uptime. `native_summary` now also returns `announces_24h`, `http_announces_24h`, `udp_announces_24h`, `accepted_ipv6_24h`, `covered_seconds_24h` and `rate_24h`, derived from recorded minute samples: adjacent samples are differenced only while the instance identifier matches and the gap stays under 300 seconds, so restarts and collection outages are excluded rather than counted as zero. The since-start figure is retained and demoted to a footnote.
- Retire `announce.augrains.com` as a public name. The site advertises `https://tracker.augrains.com/announce` (HTTPS), `http://tracker.augrains.com/announce` (HTTP) and `udp://udp.augrains.com:6969/announce`; owned-host and alias configuration collapses to the two published names. UDP carries no hostname, so UDP clients are unaffected. The TLS catch-all now closes unmatched hostnames instead of serving the tracker site on them.
- Reduce fixed per-packet and per-request cost: hot counters are grouped by writer to stop cache-line sharing between the UDP and HTTP paths, the service-time bucket lookup replaces its linear scan, business-failure replies use static text instead of formatting a `String`, and HTTP announce/scrape replies are written straight into a reused buffer with a byte-for-byte equivalence test against the previous encoder.
- Rebuild the public site's information hierarchy around the 24-hour window, unify large-number and rate formatting (万/亿 in Chinese, k/M/B in English), make wide tables scroll on narrow screens with sticky headers, and state explicitly when a subscription list is empty because evidence is insufficient.

Historical aggregate failures still cannot be backfilled, and a billion daily requests remains a workload-dependent preparation target rather than a measured capability. Validation and deployment receipts are recorded in [1.1.0 validation](docs/validation-1.1.0.md).

## 1.0.4 — bounded native hot paths and rejection diagnostics

- Reuse keyed HMAC state and validate one authenticated time slot per UDP connection ID, preserving source binding, wire format and the existing validity window.
- Reuse per-socket UDP response buffers; keep unauthenticated invalid IDs silent and response sizes bounded.
- Skip timing-index removal/reinsertion when a peer refresh leaves its deadline unchanged; retain role, expiry and peer-discovery behavior.
- Expose fixed, privacy-safe counters for HTTP parse failures, UDP malformed packets, cookie validation stages and individual engine refusal reasons. Historical aggregate failures cannot be backfilled.
- Increase bounded Nginx connection reuse while keeping announce/scrape uncached and origin TLS ticket policy unchanged.

Validation and short comparative measurements are recorded in [1.0.4 validation](docs/validation-1.0.4.md). A billion daily requests remains a workload-dependent preparation target.

## 1.0.3 — open-source preparation

- Remove built-in forum navigation locations; load optional exact HTTPS roots from private source configuration. Unconfigured sources perform no network requests, retain their stable stored identifiers, and report a configuration shortfall.
- Add a compact GitHub repository link next to the language selector, bilingual contribution/security guidance, and a locked Rust CI workflow.
- Remove unused configuration example keys and document remaining deployment portability, UDP candidate-selection and collection-timeout findings in the open-source review.
- Publish only a reviewed current-source snapshot with a new Git root; keep legacy development history and private operational configuration out of the new repository.

Validation, publication and deployment are tracked separately in [1.0.3 validation](docs/validation-1.0.3.md); preparation does not imply that these steps are complete.

## 1.0.2

- Replace the old home-page hero and summary cards with four history charts and minute-sampled native totals, followed by native addresses and six text subscriptions; arrange the address and subscription sections side by side on desktop and stacked on mobile.
- Use Home, List, Sources and Statement navigation, compact ranked tracker rows without a repeated address label, concise infrastructure details, and independent IPv4/IPv6 observation timelines. The former status route redirects to the home page.
- Remove public individual-sample and magnet APIs, pages and legacy public tables while retaining private observation pools, handshake/bitmap sampling and list eligibility rules.
- Read home-page native totals from the latest published minute, marking data older than 180 seconds unknown; visitor requests do not poll the native service.

Deployed with 117 offline tests and short isolated/browser checks; validation and deployment are tracked in [1.0.2 validation](docs/validation-1.0.2.md). No new throughput or endurance result is claimed.

## 1.0.1

Deployed with 116 offline tests and short functional validation; receipts and limitations are tracked in [1.0.1 validation](docs/validation-1.0.1.md).

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

Released with short-test validation at the user's request. One-hour and 24-hour endurance remain unverified; see `docs/validation.md` and `docs/capacity-report.md`.
