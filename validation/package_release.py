#!/usr/bin/env python3
"""Create deterministic, curated source and Linux release archives."""

import argparse
import gzip
import hashlib
import io
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys
import tarfile
import tempfile
import tomllib

VERSION = "1.0.3"
BINARIES = ("tracker-server", "observatory", "tracker-web", "tracker-loadgen")
MAX_SOURCE_BYTES = 64 * 1024 * 1024
MAX_BINARY_BYTES = 100 * 1024 * 1024
ROOT_FILES = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "README.md", "README.zh-CN.md", "LICENSE", "CHANGELOG.md", "SECURITY.md", "CONTRIBUTING.md", ".gitignore", ".gitattributes"}
DOCS = {"architecture.md", "api.md", "validation.md", "validation-1.0.1.md", "validation-1.0.2.md", "validation-1.0.3.md", "open-source-review.md", "benchmark-method.md", "capacity-report.md", "memory-profile-analysis.md", "listing-status.md", "newtrackon-nz-request.md", "ngosang-submission.md", "deployment.md", "cloudflare-ingress.md", "release-packaging.md", "third-party-notices.txt"}
DEPLOY = {"tracker-browser.conf", "ingress.conf", "ingress-bootstrap.conf", "nftables.conf", "nginx.conf", "tracker-locations.inc", "tracker-web.service", "tracker-v1.service", "tracker-observatory.service", "tracker-cloudflare-realip.service", "tracker-cloudflare-realip.timer", "refresh-cloudflare-realip.py"}
VALIDATION = {"README.md", "wirecheck.rs", "capacity_guard.rs", "capacity-guard.md", "http_connection_probe.rs", "http-connection-probe.md", "package_release.py"}


def git(*args, cwd):
    return subprocess.run(["git", *args], cwd=cwd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def allowed(name):
    p = PurePosixPath(name)
    if p.is_absolute() or any(part in ("", ".", "..") for part in p.parts) or not re.fullmatch(r"[A-Za-z0-9_./-]+", name):
        return False
    if name in ROOT_FILES or name == ".github/workflows/ci.yml":
        return True
    if any(part.startswith(".") or any(word in part.lower() for word in ("credential", "password", "secret", "private")) for part in p.parts):
        return False
    if name in {"config/config.example.toml", "config/sources.toml"}:
        return True
    if len(p.parts) == 2 and ((p.parts[0] == "docs" and p.name in DOCS) or (p.parts[0] == "deploy" and p.name in DEPLOY) or (p.parts[0] == "validation" and p.name in VALIDATION)):
        return True
    return (len(p.parts) >= 3 and p.parts[0] == "crates" and
            ((len(p.parts) == 3 and p.name == "Cargo.toml") or
             (p.parts[2] in {"src", "tests", "examples", "assets"} and
              (p.suffix in {".rs", ".css", ".js", ".svg", ".png", ".ico", ".txt", ".json"}
               or p.name == "LICENSE"))))


def source_snapshot(repo, commit):
    # Only committed files from an explicit tree; never archive the working directory.
    records = git("ls-tree", "-rz", "--full-tree", commit, cwd=repo).split(b"\0")
    paths = []
    for record in records:
        if not record:
            continue
        header, raw_path = record.split(b"\t", 1)
        name = raw_path.decode("utf-8")
        if not allowed(name):
            continue
        mode, kind, _ = header.split(b" ")
        if kind != b"blob" or mode not in (b"100644", b"100755"):
            raise ValueError(f"non-regular source entry: {name}")
        paths.append(name)
    required = ROOT_FILES | {"config/config.example.toml", "config/sources.toml"} | {f"docs/{name}" for name in DOCS} | {f"validation/{name}" for name in VALIDATION}
    if not required.issubset(paths) or not 1 <= len(paths) <= 500:
        raise ValueError("commit is missing required release files or has too many source files")
    with tempfile.TemporaryFile() as archive:
        subprocess.run(["git", "archive", "--format=tar", commit, "--", *paths], cwd=repo, stdout=archive, stderr=subprocess.PIPE, check=True)
        if archive.tell() > MAX_SOURCE_BYTES:
            raise ValueError("source archive exceeds size limit")
        archive.seek(0)
        files = {}
        with tarfile.open(fileobj=archive, mode="r:") as tar:
            for member in tar:
                if member.isdir():
                    continue
                if member.name not in paths or not member.isfile() or member.size > MAX_SOURCE_BYTES:
                    raise ValueError(f"unexpected archive member: {member.name}")
                if member.name in files:
                    raise ValueError(f"duplicate source member: {member.name}")
                files[member.name] = (tar.extractfile(member).read(), 0o755 if member.mode & 0o111 else 0o644)
        if set(files) != set(paths):
            raise ValueError("git archive did not contain all selected files")
    cargo = tomllib.loads(files["Cargo.toml"][0].decode("utf-8"))
    if cargo["workspace"]["package"]["version"] != VERSION:
        raise ValueError("workspace version does not match release version")
    return files


def reject_link_path(path):
    path = Path(os.path.abspath(path))
    for part in (path, *path.parents):
        if part.is_symlink() or (hasattr(part, "is_junction") and part.is_junction()):
            raise ValueError(f"symbolic link or junction in input path: {part}")
    return path


def linux_binaries(directory):
    directory = reject_link_path(directory)
    if not directory.is_dir():
        raise ValueError("Linux release input is not a directory")
    result = {}
    for name in BINARIES:
        path = directory / name
        st = path.lstat()
        if not stat.S_ISREG(st.st_mode) or st.st_size < 64 or st.st_size > MAX_BINARY_BYTES:
            raise ValueError(f"missing, linked, non-regular or oversized binary: {name}")
        data = path.read_bytes()
        if len(data) != st.st_size or data[:6] != b"\x7fELF\x02\x01" or data[18:20] != b"\x3e\x00":
            raise ValueError(f"binary is not a complete little-endian x86_64 ELF: {name}")
        result[f"bin/{name}"] = (data, 0o755)
    return result


def package(path, contents):
    hashes = "".join(f"{hashlib.sha256(data).hexdigest()}  {name}\n" for name, (data, _) in sorted(contents.items()))
    contents = {**contents, "SHA256SUMS": (hashes.encode(), 0o644)}
    with path.open("xb") as output:
        with gzip.GzipFile(filename="", mode="wb", fileobj=output, mtime=0, compresslevel=9) as gz:
            with tarfile.open(fileobj=gz, mode="w", format=tarfile.USTAR_FORMAT) as tar:
                for name, (data, mode) in sorted(contents.items()):
                    item = tarfile.TarInfo(name)
                    item.size, item.mode, item.mtime = len(data), mode, 0
                    item.uid = item.gid = 0
                    item.uname = item.gname = ""
                    tar.addfile(item, io.BytesIO(data))


def resolve_commit(repo, ref):
    commit = git("rev-parse", "--verify", "--end-of-options", ref + "^{commit}", cwd=repo).decode().strip()
    if not re.fullmatch(r"[0-9a-f]{40,64}", commit):
        raise ValueError("invalid resolved commit")
    return commit


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--commit", required=True, help="explicit commit SHA or Git ref to archive")
    parser.add_argument("--output-dir", required=True, type=Path, help="existing empty directory")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--source-only", action="store_true")
    mode.add_argument("--linux-release-dir", type=Path)
    parser.add_argument("--build-toolchain", help="declared exact rustc version for Linux binaries")
    parser.add_argument("--binary-build-commit", help="declared Git commit used to build the supplied Linux executables")
    args = parser.parse_args()
    repo = args.repo.resolve(strict=True)
    if git("status", "--porcelain=v1", "--untracked-files=no", cwd=repo):
        raise ValueError("tracked working tree or index is dirty; commit or stash changes first")
    commit = resolve_commit(repo, args.commit)
    if args.binary_build_commit and not args.linux_release_dir:
        raise ValueError("--binary-build-commit requires --linux-release-dir")
    build_commit = resolve_commit(repo, args.binary_build_commit) if args.binary_build_commit else None
    if build_commit:
        comparison = subprocess.run(["git", "diff", "--quiet", "--no-ext-diff", "--no-textconv", build_commit, commit, "--", "Cargo.lock", "Cargo.toml", "rust-toolchain.toml", "crates"], cwd=repo)
        if comparison.returncode != 0:
            raise ValueError("binary build commit and source commit differ in tracked build inputs, or comparison failed")
    output = reject_link_path(args.output_dir)
    if not output.is_dir() or any(output.iterdir()):
        raise ValueError("output directory must exist and be empty")
    if args.linux_release_dir and (not args.build_toolchain or not re.fullmatch(r"[A-Za-z0-9 ._+()/\-]{1,120}", args.build_toolchain)):
        raise ValueError("provide a concise --build-toolchain declaration for Linux binaries")
    source = source_snapshot(repo, commit)
    toolchain = tomllib.loads(source["rust-toolchain.toml"][0].decode())["toolchain"]["channel"]
    meta = (f"version: {VERSION}\nsource_commit: {commit}\nsource_toolchain: {toolchain}\n"
            f"linux_build_toolchain_declared: {args.build_toolchain or 'not supplied'}\n"
            f"binary_build_commit_declared: {build_commit or 'not supplied'}\n"
            f"build_input_trees_match: {'yes' if build_commit else 'not checked'}\n"
            "binary_provenance: supplied by operator; not independently verified\n").encode()
    binaries = linux_binaries(args.linux_release_dir) if args.linux_release_dir else None
    source = {**source, "RELEASE-METADATA.txt": (meta, 0o644)}
    archives = [output / f"tracker-v{VERSION}-source.tar.gz"]
    package(archives[0], source)
    if binaries is not None:
        runtime = {name: data for name, data in source.items() if name in ROOT_FILES or name.startswith("config/") or name.startswith("deploy/") or name in {"docs/third-party-notices.txt", "docs/deployment.md", "docs/cloudflare-ingress.md", "docs/release-packaging.md", "RELEASE-METADATA.txt"}}
        archives.append(output / f"tracker-v{VERSION}-linux-x86_64.tar.gz")
        package(archives[-1], {**runtime, **binaries})
    (output / "SHA256SUMS").write_text("".join(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n" for p in archives), encoding="ascii", newline="\n")
    for archive in archives:
        print(archive)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, UnicodeError, subprocess.CalledProcessError, tarfile.TarError) as exc:
        print(f"packaging failed: {exc}", file=sys.stderr)
        sys.exit(1)
