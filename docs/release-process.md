# Release process

Use this process for the next Tracker Observatory promotion. It separates source changes from evidence of a build and from evidence that S1 runs it. Release history includes same-version rebuilds; those are no longer allowed because `/healthz` reports the workspace version, not a build identifier, so same-version builds cannot be distinguished there.

## Records

| Record | Question answered | Update |
|---|---|---|
| [`CHANGELOG.md`](../CHANGELOG.md) | What source behavior changed for users? | Prepare one entry before the release commit. Never imply deployment. Correct published claims by retaining the entry and appending a dated correction that points to the correction record. |
| [`docs/releases/<version>.md`](releases/TEMPLATE.md) | What was built, tested, promoted and verified? | Create from the template as `candidate`; record only completed checks. After promotion, append dated corrections instead of rewriting the record. |
| [`docs/releases/README.md`](releases/README.md) | What is the recorded lifecycle of each version? | Update when release evidence is completed or corrected. Keep one row per version and list every known cutover. |
| Annotated Git tag `v<version>` | Which commit was built and promoted? | Create after successful cutover, pointing at the exact build commit. Do not tag a later ledger-only commit. |
| Operator host record (outside this repository) | What is currently installed on a particular host? | Maintain separately. To identify S1's running version, trust its `/healthz`, not a local checkout, tag, or this table. |

A ledger status is one of: `candidate` (prepared, not promoted), `promoted` (live, with UTC time and release directory), `superseded` (was live, replaced by a later version), `rolled back` (promoted then reverted), `not promoted` (never live), or `incomplete record` (known to have been live, but historical evidence is missing). Use `incomplete record` only for legacy history, never a new release.

## When to version

Only a change to a release build input that is intended for production requires a new version and full build/promotion process. Build inputs are `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, and `crates/**`, including `crates/tracker-web/assets/**` embedded in `tracker-web`.

| Change | Version | Build / promotion | Tag |
|---|---|---|---|
| README, `docs/`, `CHANGELOG.md`, `validation/`, or CI-only changes | No change | No build or promotion required; commit and push normally. | No tag. |
| Display polish such as wording or CSS, with no intended separate release | Defer; bundle with the next release. | No separate build or cutover. | No tag. |
| Behavior fix or display change in a production build | Patch (`1.x.y`). | Full gates, build and cutover. | Tag the built/promoted commit. |
| Public API/JSON shape, database schema, subscription eligibility, configuration keys, or another structural/migration change | Minor (`1.x.0`). | Full gates, build and cutover; document migration and compatibility. | Tag the built/promoted commit. |

This replaces the old practice of rebuilding or cutting over several times under one version. A version now identifies one build-input commit. If any build input changes after gates pass, increment the patch version and rerun every gate on the new commit. Documentation-only updates to an already promoted ledger do not require a new build.

## Steps

1. **Prepare the source.** Confirm the change is a release build input. Set the workspace version in `Cargo.toml`, synchronize the workspace entries in `Cargo.lock`, and review `git diff -- Cargo.toml Cargo.lock`. Add a user-facing `CHANGELOG.md` entry that describes source behavior without saying it is live. Copy [`TEMPLATE.md`](releases/TEMPLATE.md) to `docs/releases/<version>.md` and set status to `candidate`. If an `assets/` file changes, set the corresponding `?v=<version>` in `html.rs` and update its test assertion in the same change.
2. **Commit the candidate.** Commit all intended build inputs and the candidate ledger. Record the resulting full commit hash. Do not include unrelated working-tree changes. The release version in `Cargo.toml` is the version `/healthz` will report.
3. **Run gates on that exact commit.** Use a clean Linux x86-64 build directory with the pinned `rust-toolchain.toml` toolchain and committed lockfile. Windows is not a substitute: it has no Rust toolchain. On S1 use the independent gate directory, `-j 1 --offline`, and `--locked`; never build in the production release directory or run capacity tests during compilation.

   ```sh
   cargo fmt --all -- --check
   cargo test --workspace --locked --offline -j 1
   cargo clippy --workspace --all-targets --locked --offline -j 1 -- -D warnings
   cargo build --workspace --release --locked --offline -j 1
   ```

   Record the full source commit, `rustc --version` and `cargo --version`, each exact command and exit code, test totals, and the full SHA-256 for `tracker-server`, `observatory`, `tracker-web`, and `tracker-loadgen`. Keep the gate logs and copy the commands, exit codes and test counts into the ledger; the 1.2.2 and 1.2.3 build directories had no such logs. A failed or skipped gate is not a pass. If a build input changes after the run, commit it and rerun all four gates on the new commit.
4. **Back up and promote.** Follow [`docs/deployment.md`](deployment.md) for the protected backup, immutable release directory and atomic `current` switch. Record the UTC cutover time, new and previous directories, backup location, and only the systemd units actually restarted. Never overwrite a release directory.
5. **Verify after cutover.** Complete the checklist below. If a required check fails, do not mark the release promoted; use the rollback procedure below and record the outcome.
6. **Tag the built commit.** After successful verification, create an annotated tag on the exact commit used for the binaries and cutover:

   ```sh
   git tag -a v<version> <built-commit> -m "Tracker Observatory <version>"
   ```

   A later commit that only completes the ledger or index is not the tag target.
7. **Complete the record and publish.** Change the ledger to `promoted`, add the cutover and checks, update the release index, then commit those documentation changes. Push `main` and the version tag. Verify remote refs before describing a tag as published. If the release is not cut over, retain `candidate` or mark it `not promoted`; do not create a release tag.
8. **Optional archives.** Use [`docs/release-packaging.md`](release-packaging.md) and `validation/package_release.py` only when useful. Packaging neither builds nor proves provenance. The selected commit must contain its own versioned ledger, and the recorded binary-build commit/toolchain must match the inputs actually used.

## Post-cutover checklist

Record each result and time in the ledger. Write `not run` for a check that was not performed; do not imply a pass.

- `/healthz` returns the expected version. This is the authoritative check for the version currently running.
- `/readyz` returns success on both native and web listeners (ports 8081 and 8080 on the current deployment).
- `/api/v1/overview` returns successfully and reports the expected version and plausible counts.
- The public tracker list renders and its rows agree with the current overview.
- Check all subscription outputs: record row/line counts and confirm they are plausible for the current snapshot.
- Perform a real announce smoke check and verify same-resource peer discovery, scrape, and stopped cleanup; do not leave test registrations behind.
- If assets changed, confirm page HTML references the release's new `?v=` strings. Fetch each changed asset through the public host, calculate its SHA-256, and compare it with that asset in the built commit. If hashes differ, purge that exact URL from the CDN and fetch it again; record a pass only when the bytes match.
- Record status for the three application units and Nginx, plus systemd restart and OOM counters. Note which units restarted and any registry reset; native registry state is volatile.

## Rollback and corrections

For rollback, point `/opt/tracker-v1/current` to the preceding immutable release directory and restart only the affected units, following [`docs/deployment.md`](deployment.md). Mark the rolled-back release `rolled back` and record the reason, UTC time, restored directory, restarted units and checks. Native tracker registrations are in memory and cannot be restored by switching directories. A fix after rollback is a new patch version.

After promotion, the ledger is append-only. Add a dated `Corrections` entry with the original statement, correction, evidence and author/operator context; do not silently replace history. Correct a published changelog by retaining its original version entry and appending a dated note pointing to the correction ledger.

## Common errors

- **Same-version rebuilds:** 1.1.0 and 1.2.1 had multiple cutovers, and 1.2.2 and 1.2.3 each had repeated cutovers under one version. `/healthz` could not distinguish builds; the current rule requires a new patch for every subsequent build-input change.
- **Stale immutable assets:** the 1.1.0 CSS repair was first deployed without changing `?v=`, so Cloudflare served the previous asset. Static assets use `Cache-Control: immutable, max-age=31536000`, so stale content may remain cached for a year. Update every matching cache-buster and test assertion whenever an embedded asset changes.
- **Requesting a new asset URL too early:** do not request URLs carrying the next release's `?v=` before cutover. The server ignores the query value and returns the same asset with a one-year immutable cache policy, so the CDN can cache old bytes at the new URL. On 2026-11-17, `style.css?v=1.2.5` and `charts.js?v=1.2.5` were requested before their matching assets were live; the edge cached old content. The inferred 1.2.4 trial may also have cached old bytes at `?v=1.2.4`. Before the 1.2.5 promotion, purge those exact URLs or use an unrequested cache-buster.
- **Tagging the ledger commit:** historical tags for 1.0.4, 1.2.0 and 1.2.1 point to later documentation commits, not the respective built commit. Always tag the built/promoted commit explicitly.
- **Leaving a live ledger as `candidate`:** 1.2.3 was cut over repeatedly under the same version and is currently observed at `1.2.3-743cacd0`, while its original ledger remained a candidate. The inventory found release artifacts but no `fmt`, `test` or `clippy` logs or test counts. Append verified facts without inventing missing results.
- **Changelog claims that code never enforced:** 1.2.2 said ghost-sample verdicts removed trackers from feeds, but that behavior was not connected. A later changelog entry corrected the claim. Describe implemented behavior only and verify eligibility paths before documenting it.
