# Documentation

Reference documentation for the Tracker Observatory workspace. Each entry states the question the file answers; all links are relative to this file.

## How the system works

- [architecture.md](architecture.md) — how a request reaches the native tracker, the observatory and the website, and which state is kept private.
- [api.md](api.md) — which public routes and fields exist, and how unknown, empty and failed data are represented.

## How to deploy and operate it

- [deployment.md](deployment.md) — which steps build, install, promote and roll back an immutable release on the target host.
- [cloudflare-ingress.md](cloudflare-ingress.md) — how Nginx learns the real client address behind Cloudflare, and which forwarded values must not be trusted.

## How a release is made

- [release-process.md](release-process.md) — which gates and steps turn reviewed source into a promoted release.
- [release-packaging.md](release-packaging.md) — how to create, verify and restore the curated source and runtime archives offline.
- [releases/README.md](releases/README.md) — the recorded lifecycle of each version, and where the release record is incomplete.
- `releases/<X.Y.Z>.md` — the validation ledger for one release: what was actually checked, on which commit and environment, and what remains unverified.

## Capacity and measurement

- [capacity/benchmark-method.md](capacity/benchmark-method.md) — how capacity and protocol tests are run, and why they are restricted to environments the operator controls.
- [capacity/capacity-report.md](capacity/capacity-report.md) — which capacity and endurance measurements were actually completed on the deployment host, and which were not.
- [capacity/billion-request-envelope.md](capacity/billion-request-envelope.md) — the state and bandwidth arithmetic behind a billion requests per day, and what testing it would require.
- [capacity/memory-profile-analysis.md](capacity/memory-profile-analysis.md) — where tracker process memory goes under state churn, measured before and after the retention change.
- [capacity/optimization-1.0.4.md](capacity/optimization-1.0.4.md) — which reference implementations and hot-path arguments motivated the native tracker optimization work.

## Third-party listings

- [listings/listing-status.md](listings/listing-status.md) — which external tracker lists the service appears in, and why submitted, detected, live and stable states differ.
- [listings/ngosang-submission.md](listings/ngosang-submission.md) — the prepared, unsent submission to the ngosang/trackerslist repository.
- [listings/newtrackon-nz-request.md](listings/newtrackon-nz-request.md) — the prepared, unsent maintenance request for the New Zealand newTrackon record.

## Review and licensing

- [open-source-review.md](open-source-review.md) — what the review of the public checkout found, which findings remain open, and the limits of that assessment.
- [third-party-notices.txt](third-party-notices.txt) — the license texts and attribution for the vendored third-party components.
