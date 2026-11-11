# Architecture

This page describes the implemented 1.0.2 architecture. Final validation and deployment status are recorded in [1.0.2 validation](validation-1.0.2.md). Architecture alone is not proof of capacity or third-party acceptance.

## Request path

Nginx terminates HTTP/HTTPS and overwrites the trusted client-address header. The Rust HTTP listener is loopback-only. The UDP listener independently validates an unpredictable source-bound connection cookie. Both adapters use the same in-process sharded registry.

The registry stores only live resource registrations and bounded indexes. Each registration has an expiry timer; maintenance runs incrementally. Capacity refusal is explicit. Neither a successful HTTP status nor a liveness response establishes accepted announces under pressure.

The scrape completion counter is in-memory and lasts only while the swarm remains resident. It counts client-reported completed events, at most once for the same active endpoint and peer ID, not independently verified downloads. Removing or expiring the final peer discards the swarm counter; restart also resets it, and a later registration may count again.

The native request path returns only same-resource peers genuinely registered in local state and does not query other trackers. Native swarm registrations and scrape counts remain local. State is not synchronized across multiple tracker-server processes: run exactly one production registry on S1. Actual performance and monitor compatibility require validation.

## Independent evidence path

Public tracker lists and selected public torrents feed the observatory. Daily collection maintains forum and public sample pools, each targeting and capped at 100 entries. Cross-pool deduplication gives official open-source samples first claim, forum samples second, and The Pirate Bay daily ranking fills remaining public-pool capacity. When both pools have eligible samples, selection chooses the pool with 50/50 probability, then randomly chooses an eligible sample within that pool while retaining due-work fairness. The forum JSON adapter is present, but its upstream returned HTTP 502 and the observed forum pool had zero entries; a source failure or unfilled pool is visible rather than fabricated. The observatory resolves and validates addresses, checks IPv4 and IPv6 separately, and records transport, protocol and local outcomes. A bounded pool verifies returned peer handshakes and bitmaps without requesting payload.

Private sample state is separated from the public SQLite contract. The website and public APIs no longer expose individual sample records or magnets; ordinary client hashes, peer endpoints, credentials and private forum source details remain private. The migration drops the old public sample and run tables; private sampling, handshake/bitmap evidence and subscription thresholds remain in use. Public endpoints and observations use typed allowlisted models from `tracker-model`. The website opens public data read-only. Web requests never initiate a probe or open the private sample store.

The home page reads the latest published minute record for native peers, swarms and accepted announces; records older than 180 seconds are unknown. This read-only publication path avoids an HTTP request to the native service per visitor. Four browser charts use published data; detail timelines show actual IPv4 and IPv6 observations separately with family-specific labels and tooltips.

Health checks, peer verification and metadata collection share the observatory's bounded network budget. Request intervals and Retry-After remain upstream constraints. A locally deferred operation does not become a remote failure.

The native process publishes a coarse resource-pressure flag through its existing loopback statistics endpoint. Background observation uses short, size-bounded local requests with proxies and redirects disabled. The observatory pauses new noncritical sampling and metadata work when pressured or unable to read that signal; health checks continue under their normal limits. Already-running work has its existing deadline; this is asynchronous cooperation rather than an instantaneous preemption guarantee. It uses no host-CPU percentage controller. External measurement slots are separate from the small loopback control request.

## Publication

Detailed observations retain seven days; daily summaries retain ninety. Feeds use endpoint-specific evidence and all explicitly advertised address families, with one eligible endpoint per configured service. Shared IP addresses do not prove shared tracker identity.

Unknown values are null. The system has no composite score. Definitions and policy defaults are versioned with the source; warmup evidence is never fabricated.

## Operations

Native tracker, observatory and website run as independent systemd services with bounded memory and different CPU priorities. The native service may use both CPUs. Nginx and operating-system memory are included in the host budget.

Cloudflare-proxied ingress must trust `CF-Connecting-IP` only from Cloudflare's current IPv4 and IPv6 networks, then pass the resolved address as `X-Real-IP $remote_addr` to the loopback listener. The bootstrap helper obtains the ranges from the [official IP API](https://api.cloudflare.com/client/v4/ips); it does not by itself establish ongoing refresh. Production operations must periodically refresh both families, validate and stage the new configuration, test Nginx before reload, retain the last known good configuration on failure, and alert on refresh failures. Inspect `nginx -T` and verify real client addresses and forged headers through the actual ingress paths. Do not rely on a permanently frozen CIDR list.

Benchmark instances use separate ports and state and are discarded before public promotion. Application byte counts exclude TLS, transport overhead and retransmission; host interface counters bound the total test budget. S1 internal measurements and remote client results are reported separately.

## Discovery and peer evidence boundaries

The public NEW window belongs to the stable service identity, with the earliest discovery and baseline flag shared across its explicitly grouped endpoints. Private endpoint discovery times remain available internally. A completed, matching BitTorrent handshake remains evidence even if the subsequent bitmap wait times out; the bitmap stays unknown and both stages share the original total network deadline. No file-piece request is sent.
