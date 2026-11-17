# Open-source review

This review covers known findings and publication limits in the current public source. See the [changelog](<../CHANGELOG.md>) for source changes and [release records](<releases/README.md>) for version-specific validation and deployment evidence.

It is not a production-readiness, capacity or third-party-list acceptance claim. Secrets, private source addresses, peer endpoints and resource identifiers must never enter this document or a release archive.

## Findings

| Priority | Finding | Evidence and status |
| --- | --- | --- |
| Addressed | Private forum/source addresses must not be embedded in the public checkout. | Built-in navigation origins are removed from [pool.rs](<../crates/observatory/src/pool.rs>). Optional roots come from private `sources.toml` configuration (see the [example](<../config/sources.toml>)); unconfigured sources do not make network requests. The deterministic ghost-hash pool is intended for poison detection, not as a claim that real forum content was sampled. |
| Known limitation | The ghost-sample check is not a reliable poison verdict. | [network.rs](<../crates/observatory/src/network.rs>) flags any peer, or a non-zero UDP seeder count, returned for a ghost hash. Aggregate counts from the production host show it firing on about a fifth of listed endpoints, including long-established trackers; the cause is not established. [Observatory store.rs](<../crates/observatory/src/store.rs>) does not use the verdict for feed eligibility, family status or success rates. It remains a private diagnostic, excluded from public `error_category` by the [website allowlist](<../crates/tracker-web/src/store.rs>); see the [release evidence](<releases/1.2.5.md>). |
| Medium | Website links and ingress templates are instance-specific. | The public pages, feeds and Nginx templates intentionally target the canonical Tracker deployment. A different operator must adapt URLs, certificates, hostnames and private source settings before deployment. |
| Medium | Observatory concurrency is bounded in code. | The scheduler uses fixed bounded widths; the removed example `concurrency` key was never a runtime control. Changing those limits requires a separate behavior change and capacity validation. |
| Known limitation | Mixed-family candidate selection can return fewer same-family UDP candidates than are available. | [swarm-engine lib.rs](<../crates/swarm-engine/src/lib.rs>) caps the combined candidate set before the UDP adapter filters by request family. This is an efficiency limitation because `numwant` is an upper bound, not evidence of a protocol violation. This limitation remains. |
| Known limitation | Source discovery records the six-hour cycle before all source fetches finish. | [network.rs](<../crates/observatory/src/network.rs>) records `sources_checked_at` before iterating configured lists. Pressure or failed collection resets the marker, but an isolated slow source can still postpone the next cycle. This remains a bounded operational limitation, not a release claim. |
| Partially addressed | Third-party notices include two platform-package text gaps. | The committed lockfile and registry mappings are reconciled, but the `r-efi` 5.3.0 and 6.0.0 archives still lack attached license text. They remain marked `NEEDS REVIEW`; no legal completeness claim is made. |

## Publication and testing

The top-level [project license](<../LICENSE>) and [third-party notices](<third-party-notices.txt>) for vendored browser assets are present. Review the final source tree, configuration examples, assets and release archive for private material and licensing before publication. A Git source archive and a full Git history have different disclosure scopes.

CI and the release gate should run formatting, locked workspace tests, Clippy and a release build without contacting live tracker services. These checks cannot establish production throughput, long-term stability, global reachability or acceptance by an external list. Each promoted version must publish a separate validation ledger containing only the checks actually completed for that source commit and deployment.
