# S1 native deployment

Version 1.0.2 is deployed. The steps below describe the release procedure; final quality gates, deployment receipts and limitations are recorded in [1.0.2 validation](validation-1.0.2.md). Use the matching source, configuration and release-specific receipts before promotion.

This release uses Debian 12, systemd and Nginx. It does not require Docker, Redis or a Python application environment. The production registry is a single process; multiple independent replicas would split discovery state.

## Build an immutable release

Use the toolchain in `rust-toolchain.toml` (1.99.1), the committed `Cargo.lock`, and one build job on S1. Stop capacity tests while compiling. From a clean checkout:

```sh
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
cargo fmt --all -- --check
cargo test --workspace --locked -j 1
cargo clippy --workspace --all-targets --locked -j 1 -- -D warnings
cargo build --workspace --release --locked -j 1
```

Install `nginx`, `certbot`, `build-essential`, `pkg-config`, `libssl-dev`, `ca-certificates` and `curl` using the OS package manager. Create a system user `tracker` with no login shell. Keep binaries root-owned under `/opt/tracker-v1/releases/<release-id>/`, and link `/opt/tracker-v1/current` to that immutable directory. The four executables are `tracker-server`, `observatory`, `tracker-web` and `tracker-loadgen`. Record the source commit and SHA-256 of every executable with each release.

Copy `config/config.example.toml` and `config/sources.toml` to `/etc/tracker-v1/`, owned by root:tracker with mode 0640. Create `/var/lib/tracker-v1` owned by tracker:tracker, mode 0750. Copy the tracker-server, observatory and tracker-web systemd units to `/etc/systemd/system/`, run `systemctl daemon-reload`, and enable/start the services. Only the observatory has a writable data directory.

The baseline caps are 1536 MiB for the registry, 192 MiB for observations and 128 MiB for the website. Nginx is separately capped at 128 MiB. These are process control limits, not evidence that all limits can be consumed together. Keep OS/network headroom and measure actual RSS. The registry's 1024 MiB logical admission budget is a conservative state estimate, distinct from RSS and the systemd cap; check the final configuration before release.

## TLS and ingress

Use `deploy/nginx.conf`, `deploy/ingress.conf`, `deploy/tracker-browser.conf` and `deploy/tracker-locations.inc`. Put the ingress server block and browser-navigation maps under `/etc/nginx/conf.d/` and the location include at `/etc/nginx/tracker-locations.inc`. A parameter-free browser GET requesting HTML opens the client guide; real announce queries and BT protocol replies remain unchanged. The final ingress expects a valid certificate at `/etc/letsencrypt/live/tracker-v1/`. Bootstrap certificate issuance through the HTTP ACME location, then switch to final TLS ingress; run `nginx -t` before every reload. Keep `certbot.timer` enabled with a deploy hook that tests and reloads Nginx.

`tracker.augrains.com` retains proxied CDN ingress. `announce.augrains.com` and `udp.augrains.com` point directly to S1. Publish AAAA only after external IPv6 ingress succeeds. Website announce/scrape and the UDP alias share the same registry as the canonical announce domain.

Install the trusted Cloudflare source ranges and verify client-address recovery as described in [Cloudflare ingress](cloudflare-ingress.md). Nginx overwrites `X-Real-IP`; never expose the loopback Rust HTTP port or trust an arbitrary caller's forwarded headers. Announce, scrape, health, readiness and metrics must bypass CDN caching. Remove only this project's old Worker routes.

Nginx access logging is disabled and its runtime error sink is `/dev/null`: even a critical request error may otherwise include the complete announce query and source address. Use configuration-test output, systemd exit/OOM counters, readiness and aggregate request metrics for operations; do not enable raw request logging to investigate load.

The production firewall template allows SSH, HTTP/HTTPS, UDP 6969, ICMP and required DHCP. Adapt SSH access to the actual host before applying it. Stage a timed rollback and verify a second independent SSH connection before persisting a firewall replacement. Temporary benchmark ports require separate owner-only rules and must be removed after testing.

The `/releases/` location serves only the separately curated, versioned public release directory `/opt/tracker-v1/public/releases/`. Create it root-owned and publish reviewed source/runtime archives, aggregate evidence and checksums there after validation. Never place application state, private configuration, raw logs or keys under that directory. Directory browsing is disabled; only GET/HEAD are permitted. Confirm checksums from the public URL before calling an archive published. The presence of this route does not mean a release has already been published.

## Data and restart behavior

`private.sqlite3` contains sample identities and private measurement evidence; do not publish it or expose its directory. The website opens `public.sqlite3` read-only. No old database is imported.

Registry state is volatile. Restarting it clears peer registrations and resident completion counters; normal announces rebuild state. This is expected behavior, not a performance improvement. Observatory and website restarts do not require a registry restart.

The native tracker returns only peers genuinely registered in its local registry; scrape counts are local. Verify actual configuration, pressure limits, runtime state, local-only announce and scrape behavior, and clean restart handling with the released build. The 1.0.2 migration removes legacy public sample and run tables; preserve a protected rollback backup before upgrading the database.

Check native `/healthz`, `/readyz` and `/stats.json` through the announce host, and website `/readyz` through the website host. Check systemd restart/OOM counters, snapshot age, probe progress and real cross-transport discovery. A fresh public snapshot may be ready while strict lists are still warming.

## Promotion and rollback

Preserve the current and preceding qualified release packages plus their config/checksum manifests. Promote by atomically replacing `current`, then restart only changed services and verify readiness and real protocol behavior. A code rollback uses the preceding immutable directory and its matching config. Native state cannot be rolled back because it is never persisted.

Back up necessary SSH/network/firewall/TLS configuration off-host in a private location before host cleanup. Do not include private keys, API credentials, peer evidence or historical application data in public release archives. Cleanup is restricted to S1's authorized applications; it must not touch S2 or other domains' services.
