#!/usr/bin/env python3
"""Deployment utility: validate public Cloudflare ranges before atomic reload."""
import ipaddress
import fcntl
import json
import os
from pathlib import Path
import subprocess
import tempfile
import urllib.request


def validated_config(payload):
    if payload.get("success") is not True:
        raise ValueError("range API did not succeed")
    result = payload["result"]
    networks = []
    for key, version in (("ipv4_cidrs", 4), ("ipv6_cidrs", 6)):
        values = result[key]
        if not isinstance(values, list) or not 4 <= len(values) <= 128:
            raise ValueError("unexpected range count")
        for value in values:
            network = ipaddress.ip_network(value, strict=True)
            if network.version != version or not network.is_global or network.prefixlen == 0:
                raise ValueError("invalid trusted network")
            networks.append(network)
    if len(set(networks)) != len(networks):
        raise ValueError("duplicate trusted network")
    lines = ["# Generated from Cloudflare's public IP API; do not trust arbitrary proxies."]
    lines.extend("set_real_ip_from " + str(network) + ";" for network in networks)
    lines.extend(("real_ip_header CF-Connecting-IP;", "real_ip_recursive off;", ""))
    return "\n".join(lines).encode()


def replace(path, content):
    fd, temporary = tempfile.mkstemp(prefix=".cloudflare-realip-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
            os.fchmod(stream.fileno(), 0o644)
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def refresh():
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open("https://api.cloudflare.com/client/v4/ips", timeout=20) as response:
        data = response.read(65537)
    if len(data) > 65536:
        raise ValueError("range API response too large")
    content = validated_config(json.loads(data))
    path = Path("/etc/nginx/conf.d/cloudflare-realip.conf")
    previous = path.read_bytes() if path.exists() else None
    if previous == content:
        print("Trusted Cloudflare ranges unchanged")
        return
    replace(path, content)
    try:
        subprocess.run(["/usr/sbin/nginx", "-t"], check=True, capture_output=True, timeout=15)
        subprocess.run(["systemctl", "reload", "nginx"], check=True, capture_output=True, timeout=15)
    except Exception:
        if previous is None:
            path.unlink(missing_ok=True)
        else:
            replace(path, previous)
        raise
    print("Trusted Cloudflare ranges updated and Nginx reloaded")


def main():
    with open("/run/tracker-cloudflare-realip.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        refresh()


if __name__ == "__main__":
    main()
