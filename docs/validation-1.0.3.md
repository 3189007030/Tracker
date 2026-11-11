# 1.0.3 validation and publication

Status: source correctness checks completed; GitHub import and production deployment are separate, later actions. This document does not claim either has completed.

Changes are private configuration for previously built-in forum navigation roots, a repository navigation link, and open-source documentation/packaging. Native announce and scrape implementations are unchanged from the reviewed 1.0.2 baseline. Ordinary client hashes and peer endpoints remain runtime/private evidence, not publication inputs.

The publication plan is a clean Git root containing reviewed current Rust source, example configuration, license notices, deployment templates and synthetic verification tools. It excludes old Git objects, real databases, private source settings, credentials, request logs and local build output. The original development repository and rollback materials remain private and intact.

The pinned Rust 1.99.1 workspace passed formatting, Clippy with warnings denied, 119 tests in a loopback-only network namespace, and a locked offline Linux release build. The independent wire client passed isolated IPv4 and IPv6 HTTP/UDP exchange, scrape, stopped, malformed-cookie/packet and unrelated-resource checks in approximately one second per family. Synthetic registrations did not enter production. See [offline gates](../benchmarks/v1.0.3-offline-gate.json) and [wire checks](../benchmarks/v1.0.3-wirecheck.json).

Source scanning, packaging, GitHub upload and deployment are separate checks. No Windows 1.0.3 binary or new throughput/endurance result is claimed; no long-duration public load test is part of this release.

The intended repository is `https://github.com/3189007030/Tracker`; this is not evidence that the repository exists or that a commit has been uploaded.

## External NZ record

On 2026-11-11, NZ still stored the historical website endpoint as `http://tracker.augrains.com:443/announce`, reporting HTTP 400. The correct website endpoint uses HTTPS. The legacy UDP endpoint had a separate successful recent peer check and historical score; it is not the same failure.

BEP 34 TXT declares transport and port, not HTTP versus HTTPS. The monitor preserves an existing TCP endpoint's scheme when changing ports. At the operator's explicit request, the legacy website hostname alone was withdrawn with `BITTORRENT DENY ALL` at 2026-11-11 13:05:05 UTC; the canonical announce and legacy UDP hostnames were unchanged. Public DNS readback passed. NZ's actual removal remains pending at the 13:09 UTC check. Restore the previous `BITTORRENT TCP:443 TCP:80` before later reapplication; no monitor-specific protocol response is used.
