# Native deployment

This guide describes the current immutable-release procedure for the Tracker Observatory services. It is version-neutral: use the release version, source commit and validation ledger that are actually being promoted. A procedure is not evidence that a release was built, deployed or accepted by a third party; record those facts in `docs/validation-<version>.md`.

## Build an immutable release

Build on the target Debian 12 host or an equivalent Linux x86-64 builder with the toolchain pinned by `rust-toolchain.toml` and the committed `Cargo.lock`. Stop capacity tests while compiling. From a clean checkout:

```sh
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
cargo fmt --all -- --check
cargo test --workspace --locked -j 1
cargo clippy --workspace --all-targets --locked -j 1 -- -D warnings
cargo build --workspace --release --locked -j 1
```

Record the source commit, exact toolchain output and SHA-256 of `tracker-server`, `observatory`, `tracker-web` and `tracker-loadgen`. Keep binaries root-owned under `/opt/tracker-v1/releases/<version>-<commit>/`, then atomically point `/opt/tracker-v1/current` at that immutable directory. Preserve the preceding qualified release for rollback.

Install `nginx`, `certbot`, `build-essential`, `pkg-config`, `libssl-dev`, `ca-certificates` and `curl` using the OS package manager. Run the services as the dedicated `tracker` user with no login shell. Copy `config/config.example.toml` and the private `sources.toml` to `/etc/tracker-v1/`, owned by `root:tracker` with mode `0640`; create `/var/lib/tracker-v1` owned by `tracker:tracker` with mode `0750`. Only the observatory writes its data directory. The website opens `public.sqlite3` read-only.

The baseline process memory caps are deployment settings, not capacity evidence. Keep OS, Nginx and network headroom and verify actual RSS after each promotion. Do not run two observatory writers against the same data directory.

## Services and ingress

Install the tracker-server, observatory and tracker-web systemd units, run `systemctl daemon-reload`, and enable the units. Restart only services whose binaries changed. Verify `/healthz` and `/readyz` for the native and web listeners before exposing the release.

Use `deploy/nginx.conf`, `deploy/ingress.conf`, `deploy/tracker-browser.conf` and `deploy/tracker-locations.inc` as the reviewed Nginx templates. The canonical public hosts are `tracker.augrains.com` for HTTP/HTTPS announce, scrape and the website, and `udp.augrains.com` for direct UDP. Keep announce, scrape, health, readiness and metrics uncached. Never expose the loopback Rust listener or trust an arbitrary forwarded-address header.

Cloudflare-proxied ingress must trust `CF-Connecting-IP` only from the current Cloudflare IPv4 and IPv6 ranges, overwrite `X-Real-IP`, and retain the last known-good ranges if refresh or `nginx -t` fails. Access logging remains disabled because announce queries can contain sensitive addresses and hashes. Use readiness, aggregate metrics and systemd state for operations instead of raw request logs.

Before every firewall or Nginx change, keep a timed rollback and verify an independent SSH session. Do not apply a template without adapting SSH access, certificates, interfaces and private source settings to the actual host.

## Data and restart behavior

`private.sqlite3` contains measurement evidence and must never be published. The public website reads only the sanitized public projection. Native registry state is volatile: restarting the tracker-server clears peer registrations and counters, after which normal announces rebuild state. Observatory and website restarts do not require a registry restart.

Check the native `/healthz`, `/readyz` and `/stats.json`, and the website `/readyz`, through the intended ingress host. Confirm systemd restart/OOM counters, snapshot freshness, probe progress and real cross-transport discovery. A fresh public snapshot may still be warming strict lists.

## Promotion and rollback

Create a protected rollback copy of the current release and configuration before promotion. Switch `/opt/tracker-v1/current` atomically, restart only changed services, and verify readiness, the public API, feeds and real announce behavior. If a check fails, point `current` back to the preceding immutable directory and restart the affected units. Native peer state cannot be rolled back because it is not persisted.

Keep private SSH, firewall, TLS, configuration, database and evidence backups off-host. Never place secrets, raw logs, private databases or keys in a source/runtime archive or in the public `/releases/` directory. Publish a release archive only after its validation ledger and checksum manifest have been reviewed.
