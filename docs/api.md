# Public API

This page describes the public routes, fields and data boundaries in the current source. See the [changelog](<../CHANGELOG.md>) for source changes and [release records](<releases/README.md>) for version-specific validation and deployment evidence.

The public API does not retain the Python JSON schema or composite score. Unknown is `null`, and a real, published empty dataset is distinct from a missing snapshot.

| Route | Behavior |
| --- | --- |
| `GET /api/v1/overview` | Published version, generation time, endpoint/service/list counts and warming state. Missing snapshot generation time and counts are null. |
| `GET /api/v1/trackers` | `q`, `protocol=udp|http|https`, 1-based `page` and `limit=1..100` (default 25); aliases are excluded from items and totals; missing-store total is null. |
| `GET /api/v1/trackers/{id}` | Public endpoint, independent family status, evidence and optional network registration metadata. An alias id still resolves; the detail page redirects to the canonical endpoint. |
| `GET /api/v1/trackers/{id}/observations` | Actual observations; optional `family=ipv4|ipv6`, `stage=announce|sample_announce|handshake|bitmap`, `window=24h`, `limit=1..500`. Returns `{window,items}`; items may include optional `target_ip`. |
| `GET /api/v1/stats/history` | Published aggregate history; `window=24h|7d|30d`. |
| `GET /trackers*.txt` | All, standard, best and protocol feeds. Corresponding `.json` and `.rss` representations use the same entries. Aliased endpoints are excluded. |
| `GET /trackers/partial` | Server-rendered HTML results fragment for the list page's progressive enhancement; same `q`, `protocol`, `page` and `limit` parameters. Internal, not a JSON API; the no-JavaScript path is the `/trackers` GET form. |
| `GET /stats.json` | Aggregate native process/state counters, including separate HTTP announces, UDP connects and UDP announces. |
| `GET /metrics` | Aggregate native Prometheus metrics without per-resource labels. |

`protocol_reply` and `result` are independent: a UDP connect reply can be valid even if the subsequent announce times out. `reply_checks_24h` is the denominator for protocol replies; accepted/failed health-check counts are a different measure. HTTP status is never fabricated for UDP.

The public individual-sample and run APIs from 1.0.1 are removed. Observation and aggregate history remain available; private sampling and evidence continue without public sample magnets. Null or missing observations remain unknown.

`/api/v1/trackers` returns one page at a time: `page` is 1-based and `limit` accepts 1 through 100 (default 25), combined with `q` and `protocol`. The HTML list uses the same values and keeps `q`, `protocol`, `page` and `limit` in the URL; it paginates with numbered links and ellipses, previous/next, a jump-to-page field and a 25/50/100 page-size selector. When JavaScript is available, typing in the search field is debounced, the results region is fetched from the same-origin `/trackers/partial` fragment, and only that region is replaced through the History API, so filtering and paging do not reload the page. `/trackers/partial` is an internal HTML fragment for this progressive enhancement, not a JSON API; the plain `/trackers` GET form remains the no-JavaScript path.

Public endpoint `first_seen` and `baseline` describe its stable service identity: the earliest discovery and initial-import flag across explicitly grouped endpoints. Adding a protocol, port or configured host alias does not restart the service NEW window. Private endpoint discovery times remain separate; shared IP addresses never group services.

An endpoint that duplicates another endpoint of the same origin — a hostname URL and its literal-IP URL on the same scheme, port and path — is not published twice. One canonical endpoint is announced to and listed; the duplicate becomes an alias with optional `alias_of` set to the canonical endpoint id. Aliases are excluded from `/api/v1/trackers` items and totals and from the `.txt`, `.json` and `.rss` feeds, and they are not probed again, so the tracker origin does not receive a duplicated announce. An aliased id is still resolvable rather than a 404: the HTML detail redirects to the canonical endpoint. `alias_of` is omitted for canonical and non-aliased endpoints.

`announce` events determine health, family status and subscription eligibility. `sample_announce` records a selected public-sample request and distinguishes a valid empty response from an error. `handshake` records an aggregate attempt outcome without peer addresses. These diagnostic stages do not increase health-success denominators or qualify a tracker by themselves.

Observation query results may include optional `target_ip`: the resolved public address the probe actually contacted, the value the detail observation log shows in its 目标 IP column. It is the tracker's server address, not a peer address, and it is omitted when no address was selected or the address is not public. It separates stale DNS or a changed address from a timeout, which otherwise produce the same failed observation. The detail log localizes failure reasons and includes HTTP status when applicable rather than displaying raw internal categories. A no-address result shows a dash in the reply and reason columns; the target-address column also shows a dash when no address was contacted.

`families[].state` describes the latest observation's state and freshness (for example success, stale, unknown/untested or no address). `evidence.success_rate_24h` is historical and counts only completed observations: untested families, local failures and families with no address are not in that denominator. A stale or untested family can therefore show a 100% rate without contradiction; the website states this explicitly, including the successful/completed count behind the rate (for example 12/12).

When a future restriction is known, `families[].next_check_at` contains the earliest time, in Unix seconds, at which the observatory may probe that address family. It includes the family health schedule and any shared host backoff. The Web interface uses it to distinguish waiting for the next allowed check from a check that is due. The time is a lower bound on allowed attempts. The observation validity deadline and subscription evidence use their usual criteria.

HTTP success requires structurally valid tracker fields. Since 1.2.2 a reply to a `numwant=0` health request may omit the peer fields, and the observatory no longer writes `incomplete_protocol`; the label remains accepted for observations recorded earlier. `peers_unusable` means entries were returned but none can be used by the bounded peer verifier; it is distinct from an empty reply and does not imply an attempted or failed handshake.

Observation `error_category` uses an allowlist: a category the website does not recognise is published as null rather than passed through; `handshake_failed` is allowed. The observatory's ghost-sample verdict is not on this allowlist. It remains a private diagnostic, is not published as `error_category`, and does not affect family status, success rates or feed membership; the [release evidence](<releases/1.2.5.md>) explains why.

Native `/stats.json` exposes a coarse `pressure.active` flag and reason for bounded background cooperation. State admission at 90% of the logical budget, a recent global capacity refusal or near-exhausted HTTP handler slots raises the flag. This is a resource signal, not a quality score or a throughput guarantee.

Since 1.0.4, native `diagnostics` separates `http_invalid_queries`, `udp_malformed_packets`, `udp_cookie_window_rejections` and `udp_cookie_auth_rejections`. A window rejection means the public slot hint is outside the accepted window; it does not prove the ID was authentic or issued by this server. Authentication rejection means the hint selected a current candidate but its MAC/source binding did not validate. Neither count alone proves malicious traffic. Invalid IDs remain silent. Existing `requests.invalid_packets` remains the aggregate; individual atomics are read separately, so totals and categories may briefly differ in a busy snapshot.

`diagnostics.engine_errors` has fixed keys `invalid_address`, `invalid_port`, `peer_capacity`, `swarm_capacity`, `swarm_peer_capacity`, `memory_budget` and `busy`. Their aggregate remains `requests.business_failures`. The new counters start at process launch; old aggregate failures cannot be backfilled. Prometheus exposes the fixed diagnostic counters and `tracker_engine_failures_total{reason="..."}` without peer, resource, URL-query or cookie labels.

The website domain's `/healthz` tests its web process; `/readyz` tests a readable, fresh public snapshot and reports evidence warmup separately. These routes on the direct announce domains test the native process and its listeners/maintenance. Neither liveness nor HTTP 200 alone establishes successful tracker announces.

Subscription HEAD requests have no response body and do not initiate measurement. ETags apply to generated representations; large feed responses have a bounded single-stream slot and may return 503 with Retry-After when busy.

Public records exclude individual sample magnets, ordinary client resource hashes, raw peer addresses, private source text and credentials. Native announce exchanges peer endpoints as required by the protocol; it is not a public observation index.

The home page reads the latest published native minute record. If it is more than 180 seconds old or unavailable, the current values are unknown. A visitor request does not poll the native tracker process. Existing text, JSON and RSS subscription formats remain available, while the home page displays six text subscriptions.

The native announce path returns only same-resource peers genuinely registered in the local swarm. It does not query other trackers. Native swarm state, scrape counts and completion counts remain local.

Native scrape `complete` and `incomplete` report currently resident peers' self-reported roles. `downloaded` counts self-reported `completed` events during the current in-memory swarm lifetime; repeated events for the same active endpoint and peer ID count once. When the final peer is removed or expires, or the process restarts, that swarm's counter is discarded. A later registration may be counted again. This is not an independently verified download count.
