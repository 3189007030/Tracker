# Cloudflare ingress and client addresses

The native HTTP tracker listens only on loopback. Nginx must establish the actual client address before it overwrites `X-Real-IP` for the tracker. Only Cloudflare's current published networks may supply a trusted `CF-Connecting-IP` value; a direct client must not be able to choose its own source address through that header.

The deployment utility `deploy/refresh-cloudflare-realip.py` fetches the [official IP API](https://api.cloudflare.com/client/v4/ips), parses `result.ipv4_cidrs` and `result.ipv6_cidrs` as IP networks and generates `/etc/nginx/conf.d/cloudflare-realip.conf`. The generated file has one `set_real_ip_from` line per network followed by:

```nginx
real_ip_header CF-Connecting-IP;
real_ip_recursive off;
```

The main Nginx configuration must include that file, and the tracker proxy must overwrite the downstream client-address header with `$remote_addr`. Inspect `nginx -T` to verify the effective configuration; do not rely on a stale static copy of the CIDRs.

For a refresh, fetch the official API over verified HTTPS and require a successful response with nonempty `result.ipv4_cidrs` and `result.ipv6_cidrs`. Parse every item with an IP-network parser and reject incorrect families, malformed entries, empty lists, or implausible counts. Generate the complete candidate using the same directives above. Under an exclusive operational lock, preserve the current file, install the candidate atomically on the same filesystem, run `nginx -t`, and reload only after that succeeds. On installation, test, or reload failure, restore the saved file, retest, and keep the last known good configuration; alert an operator. Do not replace the configuration with an empty or partially downloaded list.

Install the utility at `/opt/tracker-v1/deploy/refresh-cloudflare-realip.py` and the corresponding service/timer units from `deploy/`. Enable `tracker-cloudflare-realip.timer` for a daily refresh, and inspect its last result through systemd/journald. The utility uses an exclusive lock, rejects invalid/empty responses, preserves the previous file on test/reload failure, and avoids reloading unchanged ranges. Bootstrap alone is not evidence that this timer is installed; the validation ledger records the actual host state.

After changes, check the effective configuration and use controlled real-client canaries to verify both the expected full source endpoint and rejection of forged address headers. The public canaries exercised so far used the Meta/S2 exit; they do not establish behavior for every network path.
