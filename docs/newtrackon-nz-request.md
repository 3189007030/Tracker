# NZ maintenance request — prepared, not sent

Destination: [KolanTheKoala/newtrackon-nz issues](https://github.com/KolanTheKoala/newtrackon-nz/issues).

Title: Correct the existing tracker.augrains.com HTTPS record while retaining history

## Request body

I operate the augrains.com tracker. The existing NZ website entry is stored as:

```text
http://tracker.augrains.com:443/announce
```

This sends plaintext to a TLS-only port. The NZ API's last HTTP check at 2026-11-11 09:42:37 UTC reported HTTP 400. Please correct the existing URL to:

```text
https://tracker.augrains.com:443/announce
```

Please retain the existing record and its monitoring history if a maintenance correction is possible. The hostname previously advertised `BITTORRENT TCP:443 TCP:80`. BEP 34 does not identify HTTP versus HTTPS; the monitor's existing TCP record retains its scheme, so the transport/port declaration did not fix this mismatch. At the operator's later request, only this legacy website hostname was withdrawn with `BITTORRENT DENY ALL` at 2026-11-11 13:05:05 UTC. The prior TXT must be restored before a new application; actual monitor removal and any resulting history loss remain external state.

The legacy UDP endpoint remains `udp://udp.augrains.com:6969/announce`. At 2026-11-11 12:34:49 UTC, its NZ record showed `up_unreliable`, score 75.2, recent peer checks 6/6, latest fake-peer count 0, and `stale=false`. These are the monitor's observations at that time, not a claim of stable-list admission.

Our preferred UDP alias is `udp://announce.augrains.com:6969/announce`; it uses the same server and registry. Please advise how to canonicalize the existing UDP record without duplicating the service or discarding history. We understand that the monitor's current and historical IP duplicate rules apply.

Website: [Tracker Observatory](https://tracker.augrains.com/). Public evidence and previous release materials are linked there. We request the normal monitoring behavior, with no special response or score treatment.

## Tracking

This file is an unsent alternative request. The selected action was TXT withdrawal and a later reapplication. A sent issue URL, record correction, fresh protocol check and published-list membership must be recorded separately; none follows merely from this draft.
