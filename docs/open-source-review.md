# Open-source review

This is a source review of the 1.0.2 baseline and work underway on the 1.0.3 branch. It is a list of issues and limits, not a statement that all fixes, production deployment or release gates have been completed. No secrets, private source addresses, peer endpoints or resource identifiers belong in this document.

## Findings

| Priority | Finding | Evidence and status |
| --- | --- | --- |
| Addressed and offline-validated | Forum source addresses must not be included in the public checkout if the operator intends to keep them private. | Built-in navigation origins were removed from `crates/observatory/src/pool.rs`. Optional roots now come from private `sources.toml`, with no requests when unconfigured and the existing network restrictions retained. The new GitHub repository is prepared from a current-source snapshot rather than legacy Git history. See the release validation for executed checks and publication state. |
| Medium | Website links require adaptation for a different deployment. | Parts of the website, feeds and Nginx template use instance-specific URLs. Unused website URL/stats keys have been removed from the configuration example to avoid promising settings that the binary ignores. Runtime URL configuration remains a portability task. |
| Medium | Observatory concurrency is fixed in the scheduler. | The ignored example `concurrency` key has been removed. `crates/observatory/src/network.rs` still uses bounded fixed widths; runtime tuning is not implemented. |
| Medium | A mixed-family swarm can yield fewer same-family UDP candidates than available. | `crates/swarm-engine/src/lib.rs` combines and caps IPv4/IPv6 candidates before `crates/bt-protocol/src/udp.rs` filters for the UDP request family. For example, two selected candidates may include one from each family, leaving one usable candidate despite more being available in the request family. `numwant` is an upper bound, so this is an efficiency issue, not proof of a protocol violation. **Confirmed by source control flow; a focused regression test and fix are pending.** |
| Medium | A source-discovery timeout can postpone remaining sources. | `crates/observatory/src/network.rs` stores the source-check timestamp before all work finishes; its 60-second outer timeout resets it on pressure but not on every timeout. A slow cycle could thus defer unfinished sources until the next six-hour window. **Static control-flow finding; a slow-source reproduction and fix are pending.** |
| Partially addressed | Third-party notices retain two platform-package text gaps. | The final 1.0.3 Cargo.lock checksum and all 253 registry name/version mappings have been reconciled. Exact-version archives supplied 39 previously missing package license texts. The upstream `r-efi` 5.3.0 and 6.0.0 archives still lack attached license text and remain marked `NEEDS REVIEW`; compiler distribution and runtime/source distribution are documented separately. No legal completeness claim is made. |

## Publication and testing

The top-level project license and vendored browser-asset license files are present. Their presence alone does not settle every dependency obligation. Review the final source tree, history, configuration examples, assets and release archives for private material and licensing before publishing. A Git source archive and a full Git history have different disclosure scopes.

CI is intended to check formatting, the locked workspace tests, Clippy and a release build without contacting live tracker services. It cannot establish production throughput, long-term stability, global reachability, or acceptance by an external list. Historical results on the original service describe only the particular versions and test conditions recorded there.
