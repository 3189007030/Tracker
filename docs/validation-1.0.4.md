# 1.0.4 validation

Status: offline correctness and short isolated comparative checks passed. Production promotion is pending; the deployment receipt must be recorded separately.

## Correctness gates

Rust 1.99.1 formatting, Clippy with warnings denied, 124 workspace tests in a fresh loopback-only network namespace, and a locked offline Linux release build passed. Source/runtime checks do not constitute an external capacity or endurance qualification. See [gate evidence](../benchmarks/v1.0.4-offline-gate.json).

The independent standalone Rust wire validator passed IPv4 and IPv6 against both the original 1.0.3 release binary and the candidate. Its checks cover binary query encoding, same-hash HTTP/UDP discovery, unrelated resources, zero numwant, scrape without mutation, stopped cleanup, malformed packets and invalid-cookie silence. The first harness attempt used an IPv4-only HTTP listener while requesting an IPv6 check; it was stopped, corrected to a dual-stack isolated listener, and the complete comparison rerun. Production was not changed by that failed harness.

New unit regressions cover independent cookie vectors and full-slot wraparound, buffer reuse/length limits, same-deadline timer behavior, and actual UDP connect/announce/scrape/stopped exchanges plus silent invalid packets that do not mutate state. Diagnostics distinguish HTTP protocol rejection from engine business refusal.

## Short comparison on S1

All network benchmarks ran inside a namespace with only loopback. Load generator and test server shared S1's two CPUs with ordinary production; each case started fresh state, used a bounded 128 MiB logical test budget and at most 1,000 configured resources with 32 endpoints each. Actual final states are recorded. Synthetic registrations did not enter the public registry and active public pressure bytes were zero. The full matrix used twelve 10–15 second cases, 152 configured test seconds in total.

| Case | Baseline success/s | Candidate success/s | Baseline/candidate success p99 upper ms | Baseline/candidate server CPU cores |
|---|---:|---:|---:|---:|
| UDP 1,000/s, 12s each | 999.91 | 999.91 | 1 / 1 | 0.052 / 0.047 |
| UDP 6,000/s, 12s each | 5,998.81 | 5,998.96 | 1 / 1 | 0.147 / 0.152 |
| UDP 12,000/s, 15s each | 11,996.76 | 11,997.71 | 6 / 7 | 0.278 / 0.272 |
| HTTP keepalive 6,000/s, 12s each | 5,964.29 | 5,990.63 | 20 / 4 | 0.378 / 0.364 |
| HTTP new connections 1,000/s, 10s each | 999.95 | 999.80 | 2 / 2 | 0.218 / 0.220 |
| UDP 12,000/s repeat, 15s each | 11,993.41 | 11,994.24 | 2 / 2 | 0.258 / 0.229 |

Every case had 100% successful completed protocol operations, zero client timeouts/business/protocol errors, zero kernel UDP receive/send buffer errors, and no test-server exit before shutdown. Percentiles are 1 ms bucket upper bounds. This is an offered-rate comparison rather than a maximum-throughput search. Both versions met these offered UDP rates; do not claim an increase in maximum throughput. Candidate mean UDP handler time at 12,000/s fell from 8.54 to 7.54 microseconds and in the repeat from 8.59 to 6.54 microseconds; total CPU gains varied by case and some cases were slightly worse. The HTTP results measure native HTTP, not TLS/CDN.

An in-process fixed/advancing-time microbenchmark used identical source on each engine version, 3/64/512 mixed-family peers, numwant 0/50, 50,000 operations per case and three interleaved rounds. Most small/zero-output refresh cases improved, but the first dense 512-peer/50-result fixed-time median was 8.4% lower; a further three-round check varied to 3.0% lower, while its advancing-time result improved 12.8%. Report this variation instead of a universal optimization percentage. See the full [comparison and repeat evidence](../benchmarks/v1.0.4-short-comparison.json).

## Diagnostics and retained boundaries

Fifteen seconds of pre-change passive IPv4 packet-shape observation counted 3,404 structurally valid announces and one valid-shape scrape: 3,372 public cookie hints were within the acceptance window and 33 outside; the server separately counted 1,433 invalid packets. These are aggregate samples, not authenticated per-packet attribution. They do not establish that all or most historical failures were expiration or malicious traffic. A second correlation experiment did not observe outgoing replies and is not valid evidence about issued IDs. No raw packets, hashes, addresses, identities or cookie values were retained.

Historical aggregate failures cannot be reconstructed. New fixed counters distinguish HTTP parsing, UDP malformed packets, slot-window rejection, authentication rejection and each engine failure reason. Atomic snapshots are eventually consistent. Cookie source binding, validity window, full-slot authentication and invalid-ID silence remain intact. No peer aggregation or monitor-specific reply is introduced.

The Nginx policy increases bounded connection reuse (75-second client idle timeout, 128 idle native upstream connections per worker, 300-second upstream idle timeout, 10,000-request recycle caps) while keeping stateful responses uncached and origin TLS tickets disabled. No new Cloudflare setting or global latency gain is claimed.

[Research references](optimization-1.0.4.md) and [state/bandwidth arithmetic](billion-request-envelope.md) explain the next limits. No full-scale state, public-path 12,000/s, one-hour or 24-hour run was performed. One billion daily requests remains a workload-dependent goal; the normal 30-minute-refresh state model exceeds current S1 memory.
