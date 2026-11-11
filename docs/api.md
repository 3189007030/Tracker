# Public API — 1.0.2

The routes describe the 1.0.2 implementation. Final validation and deployment status are recorded in [1.0.2 validation](validation-1.0.2.md).

The new API does not retain the Python JSON schema or composite score. Unknown is `null`, and a real, published empty dataset is distinct from a missing snapshot.

| Route | Behavior |
| --- | --- |
| `GET /api/v1/overview` | Published version, generation time, endpoint/service/list counts and warming state. Missing snapshot generation time and counts are null. |
| `GET /api/v1/trackers` | `q`, `protocol=udp|http|https`, `page>=1`, `limit=1..100`; missing-store total is null. |
| `GET /api/v1/trackers/{id}` | Public endpoint, independent family status, evidence and optional network registration metadata. |
| `GET /api/v1/trackers/{id}/observations` | Actual observations; optional `family=ipv4|ipv6`, `stage=announce|sample_announce|handshake|bitmap`, `window=24h`, `limit=1..500`. Returns `{window,items}`. |
| `GET /api/v1/stats/history` | Published aggregate history; `window=24h|7d|30d`. |
| `GET /trackers*.txt` | All, standard, best and protocol feeds. Corresponding `.json` and `.rss` representations use the same entries. |
| `GET /stats.json` | Aggregate native process/state counters, including separate HTTP announces, UDP connects and UDP announces. |
| `GET /metrics` | Aggregate native Prometheus metrics without per-resource labels. |

`protocol_reply` and `result` are independent: a UDP connect reply can be valid even if the subsequent announce times out. `reply_checks_24h` is the denominator for protocol replies; accepted/failed health-check counts are a different measure. HTTP status is never fabricated for UDP.

The public individual-sample and run APIs from 1.0.1 are removed. Observation and aggregate history remain available; private sampling and evidence continue without public sample magnets. Null or missing observations remain unknown.

Public endpoint `first_seen` and `baseline` describe its stable service identity: the earliest discovery and initial-import flag across explicitly grouped endpoints. Adding a protocol, port or configured host alias does not restart the service NEW window. Private endpoint discovery times remain separate; shared IP addresses never group services.

`announce` events determine health, family status and subscription eligibility. `sample_announce` records a selected public-sample request and distinguishes a valid empty response from an error. `handshake` records an aggregate attempt outcome without peer addresses. These diagnostic stages do not increase health-success denominators or qualify a tracker by themselves.

HTTP success requires structurally valid tracker fields. `incomplete_protocol` is an unverified reply missing required peer fields, even when HTTP is 200. `peers_unusable` means entries were returned but none can be used by the bounded peer verifier; it is distinct from an empty reply and does not imply an attempted or failed handshake.

Native `/stats.json` exposes a coarse `pressure.active` flag and reason for bounded background cooperation. State admission at 90% of the logical budget, a recent global capacity refusal or near-exhausted HTTP handler slots raises the flag. This is a resource signal, not a quality score or a throughput guarantee.

The website domain's `/healthz` tests its web process; `/readyz` tests a readable, fresh public snapshot and reports evidence warmup separately. These routes on the direct announce domains test the native process and its listeners/maintenance. Neither liveness nor HTTP 200 alone establishes successful tracker announces.

Subscription HEAD requests have no response body and do not initiate measurement. ETags apply to generated representations; large feed responses have a bounded single-stream slot and may return 503 with Retry-After when busy.

Public records exclude individual sample magnets, ordinary client resource hashes, raw peer addresses, private source text and credentials. Native announce exchanges peer endpoints as required by the protocol; it is not a public observation index.

The home page reads the latest published native minute record. If it is more than 180 seconds old or unavailable, the current values are unknown. A visitor request does not poll the native tracker process. Existing text, JSON and RSS subscription formats remain available, while the home page displays six text subscriptions.

The native announce path returns only same-resource peers genuinely registered in the local swarm. It does not query other trackers. Native swarm state, scrape counts and completion counts remain local.

Native scrape `complete` and `incomplete` report currently resident peers' self-reported roles. `downloaded` counts self-reported `completed` events during the current in-memory swarm lifetime; repeated events for the same active endpoint and peer ID count once. When the final peer is removed or expires, or the process restarts, that swarm's counter is discarded. A later registration may be counted again. This is not an independently verified download count.
