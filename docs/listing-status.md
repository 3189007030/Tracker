# External tracker listings

External services operate independently. Submitted, detected, live and stable-list membership are different states. No source release, local test, successful first check or hostname change guarantees admission.

## NZ investigation — 2026-11-11

The [website record](https://newtrackon.co.nz/api/tracker/tracker.augrains.com) was stored as `http://tracker.augrains.com:443/announce`. Its latest HTTP check at 09:42:37 UTC reported HTTP 400. The correct website URL is `https://tracker.augrains.com:443/announce`; sending plaintext HTTP to TLS port 443 reproduces the mismatch.

The [legacy UDP record](https://newtrackon.co.nz/api/tracker/udp.augrains.com) was separate: its check at 12:34:49 UTC reported `up_unreliable`, score 75.2, peer checks 6/6, latest fake-peer count 0, and `stale=false`. This does not establish stable-list membership, and the historical score must not be equated with a current transport failure.

Public TXT declarations before the requested withdrawal:

```text
tracker.augrains.com   BITTORRENT TCP:443 TCP:80
udp.augrains.com       BITTORRENT UDP:6969
announce.augrains.com  BITTORRENT UDP:6969 TCP:443 TCP:80
```

[BEP 34](https://www.bittorrent.org/beps/bep_0034.html) declares transport and port, not the HTTP/TLS scheme. The NZ [monitor](https://github.com/KolanTheKoala/newtrackon-nz/blob/main/newtrackon/tracker.py) preserves an existing HTTP/HTTPS scheme when it changes TCP ports. A declaration with no supported protocol requests removal and also tells other conforming monitors and clients that the hostname no longer provides a tracker.

At the operator's explicit request, only the website hostname's TXT was changed at 2026-11-11 13:05:05 UTC to `BITTORRENT DENY ALL`, TTL 300. Its A/AAAA records and the canonical announce and legacy UDP hostnames were unchanged. Cloudflare and Google public DNS both returned the withdrawal at 13:09 UTC. NZ still showed its prior HTTP failure at that readback; actual removal awaits its next check. Restore the original website TXT and allow caches to refresh before requesting a new listing. A DNS update alone is not evidence of monitor removal.

NZ [ingestion](https://github.com/KolanTheKoala/newtrackon-nz/blob/main/newtrackon/ingest.py) applies its own UDP > HTTP > HTTPS preference and checks current/recent IP overlap. Resubmitting HTTPS does not reliably correct an existing HTTP record. A new canonical hostname may conflict with the already listed UDP service. Removal and resubmission do not guarantee history restoration or admission.

The [maintenance request](newtrackon-nz-request.md) remains an unsent alternative. The selected path is TXT withdrawal followed by a later reapplication, with the history/duplicate limitations above. Record actual monitor removal and any later resubmission separately.

## Other services

[newTrackon.com](https://newtrackon.com/), NZ, [ngosang](https://github.com/ngosang/trackerslist) and [XIU2](https://github.com/XIU2/TrackersListCollection) have separate policies, update cycles and histories. Previous 1.0.0–1.0.2 reports describe their own snapshots; this open-source review did not revalidate all those memberships. Check each service's current public list before stating that an endpoint is included.
