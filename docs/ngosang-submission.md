# ngosang/trackerslist submission draft

**Status: prepared only; not sent.** Publication requires the user's explicit instruction and an available authenticated account; this draft does not assume an account is ready.

The repository [README](https://github.com/ngosang/trackerslist#contribute) directs reports of additional public trackers to [open a new GitHub issue](https://github.com/ngosang/trackerslist/issues/new). An [earlier tracker submission](https://github.com/ngosang/trackerslist/issues/682) shows that issues have been used for this purpose; it is an example, not a guarantee of acceptance. The [maintainer's description](https://github.com/ngosang/trackerslist/issues/463) says that rankings consider uptime, latency, returned peers, fake peers and duplicate trackers. The README does not state that prior inclusion in a different list is mandatory. Admission and duplicate handling remain the maintainer's decisions.

The Gitee repository is not currently anonymously accessible (API 404, web 403, Git authentication failure). The five candidate Linux executables passed local SHA-256 download verification. Source, runtime and redacted test evidence: [release downloads](https://tracker.augrains.com/releases/v1.0.0/). Check its SHA256SUMS manifest; do not rely on anonymous Gitee access.

## Proposed issue title

Please consider adding the augrains public tracker

## Proposed issue body

Hello, I operate a public BitTorrent tracker and would appreciate your consideration for the tracker list.

Preferred addresses, all served by the same tracker and shared swarm registry:

```text
udp://announce.augrains.com:6969/announce
https://announce.augrains.com:443/announce
http://announce.augrains.com:80/announce
```

The following older addresses remain compatible aliases for the same service; they are not separate trackers:

```text
udp://udp.augrains.com:6969/announce
https://tracker.augrains.com:443/announce
```

Please apply your normal availability, peer-quality and duplicate checks, and select the address or addresses appropriate for your list.

The tracker is a Rust implementation. The release includes a source archive on the project website.

On 2026-11-10, a direct physical-network verification passed exact-endpoint, two-way peer discovery, zero-numwant registration, stopped removal, scrape transitions and resource isolation, including IPv4/IPv6 HTTP cross-discovery. A separate independent wire checker passed IPv4 core and IPv6 Nginx/UDP protocol checks on loopback, including malformed-packet boundaries and HTTP/UDP peer discovery. These are specific test results, not a claim of long-term availability or capacity. Supporting records are included in the redacted evidence archive at the release download directory.

Thank you for maintaining the list.
