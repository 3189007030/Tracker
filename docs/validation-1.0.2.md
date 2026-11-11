# Tracker 1.0.2 validation

This release changes website layout and the public observation surface. Native announce/scrape rules, peer selection, sample-pool selection, upstream timing and list admission thresholds are unchanged.

## Behavior

- Home starts with minute-sampled registered peers, swarms and accepted announces since process start, followed by four browser-rendered charts. The former hero and snapshot banner are removed. Native endpoints and six TXT subscriptions sit side by side on desktop and stack on phones. Existing JSON/RSS subscription routes remain available.
- Navigation is Home, List, Sources and Statement. `/status` permanently redirects home, retaining language. The list uses compact ranked rows and copy controls; network metadata uses smaller text in its own layout.
- Public sample/magnet/per-node views and `/api/v1/samples/{id}` and `/api/v1/trackers/{id}/runs` are removed. The migration drops their old public tables and the obsolete private trace table. Private verification pools, handshake/bitmap aggregates, tracker observations and history remain. Matching pre-migration database/configuration backups are required for rollback; dropping tables is not a claim that historical backups were physically erased.
- The homepage reads one published minute bucket, with a 180-second freshness limit. Missing or old data displays unknown, including when no sample has yet followed a restart. It does not request live native statistics for each visitor.
- Two green timeline points are independent IPv4 and IPv6 observations. A pre-change read-only check found no exact duplicate events: the inspected endpoint had paired family checks about 30 minutes apart, with the two families typically 1–2 seconds apart. The new legend/tooltips identify family, status and UTC seconds; no records are merged or invented.
- Source links point to the upstream project repositories. The Statement describes actual tracker behavior, peer-address visibility, private observation metadata, zero-payload probes and application logging limits; it makes no promise of anonymity, universal reachability or legal immunity.

## Verification

Formatting, Clippy with warnings denied, all **117 offline workspace tests**, the Linux release build and Windows load-generator build passed. The workspace tests ran in a fresh loopback-only network namespace with Cargo offline. A separate wire checker passed HTTP/UDP discovery, scrape, stopped cleanup, malformed packets and resource isolation for both address families against isolated loopback instances: approximately 21 KiB and 0.95 seconds per family, without introducing synthetic state into production.

Build `e39cdf5916a232c83c731782c693718c0a202c4f` was deployed on S1 at **2026-11-11 09:48:09 UTC**, after a consistent pre-migration database/configuration backup was checksum-verified off-host. Both native and website readiness reported 1.0.2; independent SSH access passed. This version change restarted the in-memory registry; its subsequent lower counts are fresh registrations, not a capacity improvement. Collection schedules were not reset.

The public browser review confirmed the four top charts, desktop address/subscription columns, six always-visible TXT subscriptions, clipboard controls, 76px representative list rows without the redundant address label, a single desktop row of 14px network values, family-separated timeline wording, source repository links and Chinese/English Statement. The 390px home/list layouts had no horizontal overflow; address panels stacked on mobile.

Scripted route/data assertions ran through S1's loopback Nginx virtual host after the operator's Python HTTPS client encountered an SSL EOF; this is distinct from the successful public browser check. The retired sample APIs returned 404, the public sample/run tables and obsolete private trace table were absent, while 225 aggregate history buckets, 10,346 private verification rows and 7,173 upstream due rows remained at the check time. These are point-in-time counts. Receipts are `benchmarks/v1.0.2-offline-gate.json`, `v1.0.2-wirecheck.json`, `v1.0.2-deployment.json` and `v1.0.2-web-check.json`.

No pressure or endurance test was performed for this website release. Prior [1.0.1 network limitations](validation-1.0.1.md) are historical observations, not retested results or a new capacity claim. The original public-test budget baseline is preserved; a pre-cutover NIC snapshot charged 10,888,883,935 bytes, including concurrent natural/control traffic, below the 18 GiB active-test stop. NZ maintainer communication remains paused.
