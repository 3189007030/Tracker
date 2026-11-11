# Offline release packaging and recovery

The packaging tool creates a curated source archive from an explicitly selected Git commit and, optionally, a separate Linux x86-64 runtime archive from four explicitly supplied executables. It does not build, download, publish, modify a server, or prove that the executables were built from the declared commit. A recovery archive is not evidence of a historically tested previous release.

Use Python 3.11 or newer and Git. First review and commit the intended source, packaging script and this document; both the tracked working tree and index must be clean. Untracked files are never taken from the working directory. The tool resolves the requested Git ref to a commit, exports only the allowlisted source paths from that commit, and rejects selected symlinks and invalid archive members. The allowlist intentionally omits databases, data directories, raw logs, `.env` files, keys, old collections, build output and arbitrary untracked files. Review tracked source and example configurations for secrets before publication: a filename allowlist and binary-format check cannot detect all embedded credentials or private data.

Build and validate the four production binaries separately using the committed lockfile and the toolchain recorded in `rust-toolchain.toml`. Put only the resulting Linux x86-64 binaries in the chosen input directory. The packager checks their names, regular-file status, size and ELF architecture; this is not a substitute for build provenance or executable testing. Input-directory links and junctions are rejected. Record exact `rustc --version` output, the build environment, test evidence and binary hashes independently. Do not put production secrets into the templates or release output.

For a source-only archive, create a new empty output directory and run:

```sh
python3 validation/package_release.py --commit <reviewed-commit> --source-only --output-dir /path/to/empty-output
```

For source and Linux runtime archives:

```sh
python3 validation/package_release.py --commit <reviewed-commit> \
  --linux-release-dir /path/to/linux/release --build-toolchain 'rustc 1.99.1' \
  --binary-build-commit <actual-binary-build-commit> \
  --output-dir /path/to/empty-output
```

The input directory must contain `tracker-server`, `observatory`, `tracker-web` and `tracker-loadgen`. `--binary-build-commit` is optional, but should be supplied when the actual build commit is known. When supplied, the tool rejects differing `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` or `crates/` Git trees between the build and source commits; documentation and validation-tool changes may differ. The metadata identifies both commits and records whether that comparison ran. The comparison does not prove that the binaries were actually built from the declared commit or with the declared toolchain.

The source archive contains the allowlisted committed workspace, configuration examples, selected deployment files, reviewed public documentation and validation tools. It deliberately does not include the `benchmarks/` JSON files; references to those files in documentation require the separately reviewed, redacted evidence bundle. The runtime archive contains the four executables, configuration and deployment templates, README, license, third-party notices, deployment guidance and this recovery guide. Each includes `RELEASE-METADATA.txt` with version, source commit, optional declared binary build commit, source toolchain and operator-declared binary toolchain; its `SHA256SUMS` covers the other files in that archive. The output directory also gets `SHA256SUMS` for the archive files. Archives have sorted paths, fixed timestamps, ownership and modes. Reproducibility assumes identical inputs and compatible Python/zlib implementations.

The separate, operator-run asset preparation tool may create a reviewed evidence archive and a bilingual index after its committed-evidence gates pass; its final manifest covers all files next to the index. The supplied Nginx template maps `/releases/` to `/opt/tracker-v1/public/releases/`, so an eventual immutable `v1.0.3` directory would place the reviewed assets under `/opt/tracker-v1/public/releases/v1.0.3/`; relative links then resolve under `/releases/v1.0.3/`. Preparing assets does not install them at that path or establish public availability.

## Offline verification and restoration

Transfer the output directory through a trusted channel and compare its manifest against the independently recorded or authenticated archive hashes. A checksum shipped beside an archive detects accidental damage but does not authenticate its origin. Then, in an empty staging directory on a compatible Linux x86-64 host:

```sh
(cd /path/to/output && sha256sum -c SHA256SUMS)
tar -xzf /path/to/output/tracker-v1.0.3-linux-x86_64.tar.gz
sha256sum -c SHA256SUMS
```

Inspect `RELEASE-METADATA.txt`, `config/`, `deploy/` and `docs/deployment.md`. Confirm that the build provenance, software dependencies, network, firewall and certificates match the destination. Restore appropriate private configuration and TLS material from separately protected backups; examples are not production credentials. Create a new root-owned immutable release directory and copy the four verified executables from the archive's `bin/` directly into that release directory: the supplied units expect `/opt/tracker-v1/current/tracker-server` and the corresponding executable names at the release root. Configure the service units and private state directory as described in the deployment guide. Check Nginx configuration before reload, and verify service readiness and real announce behavior after a controlled switch. Registry peer state is in memory and will be rebuilt by normal announces; databases and past peer state are not restored by this package.

Do not overwrite a known-good installation as a test of the archive. Preserve independently qualified releases and their matching configuration for rollback. Successful packaging or checksum verification alone does not establish production readiness, performance, community listing or a published release.

## Validation scope for this release

For 1.0.3, report only the checks actually completed in [1.0.3 validation](validation-1.0.3.md). Historical 1.0.0 and 1.0.1 test results do not establish 1.0.3 throughput or endurance. The 1.0.3 source, runtime and service templates contain the native tracker, observatory and website services and the separate load generator.
