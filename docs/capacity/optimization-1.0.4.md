# 1.0.4 tracker optimization research

Research draft; implementation, deployment and measurement results must be added only after they are independently verified. One billion requests per day is a preparation target: evenly divided over 86,400 seconds it is about **11,574 requests/s**. This is an average, not a peak, a measured capacity, or a guarantee. Connect, announce and scrape are distinct UDP operations; the workload mix, peer response size, address family, retry behavior and network overhead must be stated.

## Reference implementations

These projects are used as behavioral references, not as sources for copied code. Review the applicable licenses before reusing any code or assets.

| Project and inspected revision | License and observations |
| --- | --- |
| [aquatic](https://github.com/greatest-ape/aquatic/tree/a2ddc4b323c5aaf844ce32b655b0ffc8c4836cde) | Apache-2.0. Its [UDP tests](https://github.com/greatest-ape/aquatic/blob/a2ddc4b323c5aaf844ce32b655b0ffc8c4836cde/crates/udp/tests/invalid_connection_id.rs) require silence for invalid connection IDs. Its configuration includes independent socket workers, receive buffer sizing and an optional Linux io_uring backend. Its default maximum connection age is 120 seconds. |
| [opentracker](https://erdgeist.org/gitweb/opentracker/commit/?id=530d3ee20a06f2a128b1eddaf2398011d3f1d119) | Beerware, as described on its [official site](https://erdgeist.org/arts/software/opentracker/). The local reference checkout is pinned to this commit. Its [UDP handler](https://erdgeist.org/gitweb/opentracker/tree/src/ot_udp.c?id=530d3ee20a06f2a128b1eddaf2398011d3f1d119) compares current and previous IP-bound IDs and sends an error if both mismatch. That response is sent before a full announce-length check; it does not establish that an invalid ID belonged to the source. Do not copy this behavior without an independent anti-reflection design. |
| [Torrust Tracker](https://github.com/torrust/torrust-tracker/tree/4f0af379a32df0401f6d9ef65b3c852185d1e9b9) | AGPL-3.0-only with its stated legacy exception. Its UDP server separates receive, processing and error accounting, and has configurable invalid-ID counting/ban policies. Different license and deployment goals require care: no code is imported here. |

## Protocol and security boundaries

[BEP 15](https://www.bittorrent.org/beps/bep_0015.html) says a tracker ignores a mismatching connection ID. Clients may reuse a received ID for one minute, servers should accept it for two minutes, and clients must obtain a new one after expiry. A timeout can also result from packet loss, overload or a changed source address; an invalid-ID count alone does not identify the cause.

The current native server silently drops invalid UDP cookies. Preserve that behavior for unauthenticated or unclassifiable packets. A malformed request with a valid cookie is a separate case; any error reply still needs explicit rate and amplification limits. An expired-cookie classification may be considered only where a bounded prior slot and the original source can be authenticated, with wraparound and restart behavior tested. Never accept a cookie indefinitely, weaken its MAC, expose source addresses or reply based only on an untrusted timestamp hint. A server restart changes the cookie secret and clients must reconnect.

In one 15-second IPv4 observation, 5,646 UDP packets included 3,404 structurally valid announces and one structurally valid scrape. Across those two request types, 3,372 slot hints were within the accepted window and 33 were outside it; a separate server counter reported 1,433 invalid packets. These are aggregate observations, not per-packet authenticated correlation. They do not establish that most invalid packets were expired, forged or sent by a particular client. A separate attempted correlation did not capture the outgoing replies and is not valid evidence for identifying fake IDs.

## Candidate work

1. **Measure first.** Separate UDP connect, valid announce, scrape, malformed packet and invalid-cookie counters. Compare client sends/completions, timeouts and valid replies with server receive/send/error counters and operating-system UDP drops. Keep peer addresses, resource identifiers and cookie values out of logs.
2. **Reduce cookie overhead without changing validation.** Reuse keyed HMAC state and use an authenticated full slot after a bounded public slot hint selects the candidate. Verify old/current/future slots, 16-bit hint wrap, wrong source and restart with independent vectors. Treat code as a candidate until tests and a controlled before/after measurement pass.
3. **Profile the serial UDP receive/parse/engine/send path.** The current server uses one loop per address family and bounded shared state. Compare event-loop cost, socket drops, lock contention, CPU and RSS before adding workers or Linux batch syscalls. A portable fallback, bounded buffers, fair scheduling and correct error handling are required; on a two-core machine more workers can harm the website or observatory.
4. **Keep HTTP comparisons equivalent.** Test existing Nginx-to-native keepalive and client keepalive separately from new connections, with the same request mix and source path. Preserve no-cache announce/scrape semantics and real source addresses. Do not infer a gain from changing TLS security or treating a monitor specially.
5. **Protect live state.** Use owned, isolated instances and budgets; include cold and hot swarms, capacity refusal and real two-client discovery. Preserve same-resource isolation, `numwant=0`, stopped removal, scrape counts and dual-stack behavior. There is no cross-tracker peer aggregation.

## Short test matrix

- Offline protocol vectors and parser tests: IPv4/IPv6, bad/expired/wrong-source IDs, restart, slot wrap, bounded datagrams and amplification.
- Isolated same-host tests, then a genuinely independent client: UDP connect, announce and scrape separately and in a documented mix; compare current and candidate with matched workload distribution, state geometry, client concurrency, duration and byte cap. Record the random seed when the generator supports one.
- HTTP keepalive and new-connection tests separately, then HTTPS through the actual configured ingress; validate bencoded outcomes, not only HTTP 200.
- Test several short stepped rates around the historical operating range and the 11,574/s target, only while rate, packet-loss and resource guardrails remain healthy. High RTT requires enough client in-flight requests; report offered and actually started rates separately.
- Record duration, p50/p95/p99 and timeouts, successful transaction rate, bytes including host interface overhead, CPU, RSS, UDP kernel drops, process restart/OOM and contention with ordinary traffic. Check real announce/discovery and scrape before and after every run.

Historical tests are described in [the capacity report](capacity-report.md). Isolated loopback, synthetic state, a short external run and a long-duration service claim are different kinds of evidence. Short tests cannot prove daily or 24-hour endurance, and measurements from one version or path cannot be attributed to another.
