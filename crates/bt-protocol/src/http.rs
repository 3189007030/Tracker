use crate::{AnnounceEvent, AnnounceRequest, AnnounceResponse, InfoHash, ScrapeEntry};
use anyhow::{Result, bail, ensure};
use std::net::{IpAddr, SocketAddr};

const MAX_QUERY_BYTES: usize = 8192;
const MAX_PARAMS: usize = 128;
const MAX_NUMWANT: usize = 100;
const MAX_SCRAPE_HASHES: usize = 64;

fn unhex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn decode_part(raw: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            b'%' => {
                ensure!(i + 2 < raw.len(), "truncated percent encoding");
                let a =
                    unhex(raw[i + 1]).ok_or_else(|| anyhow::anyhow!("invalid percent encoding"))?;
                let b =
                    unhex(raw[i + 2]).ok_or_else(|| anyhow::anyhow!("invalid percent encoding"))?;
                out.push((a << 4) | b);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out)
}

fn query(raw: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    ensure!(raw.len() <= MAX_QUERY_BYTES, "query too large");
    let mut params = Vec::new();
    for pair in raw.split(|b| *b == b'&') {
        if pair.is_empty() {
            continue;
        }
        ensure!(params.len() < MAX_PARAMS, "too many query parameters");
        let split = pair.iter().position(|b| *b == b'=').unwrap_or(pair.len());
        let value = if split < pair.len() {
            &pair[split + 1..]
        } else {
            &[]
        };
        params.push((decode_part(&pair[..split])?, decode_part(value)?));
    }
    Ok(params)
}

fn one<'a>(params: &'a [(Vec<u8>, Vec<u8>)], key: &[u8]) -> Result<Option<&'a [u8]>> {
    let mut matches = params
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.as_slice());
    let first = matches.next();
    ensure!(matches.next().is_none(), "duplicate parameter");
    Ok(first)
}

fn required<'a>(p: &'a [(Vec<u8>, Vec<u8>)], key: &[u8]) -> Result<&'a [u8]> {
    one(p, key)?.ok_or_else(|| anyhow::anyhow!("missing required parameter"))
}

fn number(raw: &[u8]) -> Result<u64> {
    ensure!(
        !raw.is_empty() && raw.iter().all(u8::is_ascii_digit),
        "invalid integer"
    );
    Ok(std::str::from_utf8(raw)?.parse()?)
}

fn hash(raw: &[u8]) -> Result<InfoHash> {
    raw.try_into()
        .map_err(|_| anyhow::anyhow!("info_hash must be exactly 20 bytes"))
}

pub fn parse_http_announce(raw: &[u8], ip: IpAddr) -> Result<AnnounceRequest> {
    let p = query(raw)?;
    let info_hash = hash(required(&p, b"info_hash")?)?;
    let peer_id = required(&p, b"peer_id")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("peer_id must be exactly 20 bytes"))?;
    let port = u16::try_from(number(required(&p, b"port")?)?)?;
    ensure!(port != 0, "port must be nonzero");
    let uploaded = number(required(&p, b"uploaded")?)?;
    let downloaded = number(required(&p, b"downloaded")?)?;
    let left = number(required(&p, b"left")?)?;
    let event = match one(&p, b"event")? {
        None | Some(b"") => AnnounceEvent::None,
        Some(b"started") => AnnounceEvent::Started,
        Some(b"completed") => AnnounceEvent::Completed,
        Some(b"stopped") => AnnounceEvent::Stopped,
        _ => bail!("invalid announce event"),
    };
    let numwant = match one(&p, b"numwant")? {
        None | Some(b"-1") => 50,
        Some(v) => number(v)?.min(MAX_NUMWANT as u64) as usize,
    };
    Ok(AnnounceRequest {
        info_hash,
        peer_id,
        ip,
        port,
        uploaded,
        downloaded,
        left,
        event,
        numwant,
    })
}

pub fn parse_http_scrape(raw: &[u8]) -> Result<Vec<InfoHash>> {
    let p = query(raw)?;
    let mut hashes = Vec::new();
    for (_, value) in p.iter().filter(|(k, _)| k == b"info_hash") {
        ensure!(hashes.len() < MAX_SCRAPE_HASHES, "too many scrape hashes");
        hashes.push(hash(value)?);
    }
    ensure!(!hashes.is_empty(), "scrape requires info_hash");
    Ok(hashes)
}

/// Append the decimal form of `value`. Equivalent to `value.to_string()` but
/// without the intermediate allocation on the reply path.
fn write_decimal(out: &mut Vec<u8>, mut value: u64) {
    let mut digits = [0u8; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[start..]);
}

/// Append `<length>:<key>` for a bencode dictionary key.
fn write_key(out: &mut Vec<u8>, key: &[u8]) {
    write_decimal(out, key.len() as u64);
    out.push(b':');
    out.extend_from_slice(key);
}

/// Append a non-negative bencode integer.
fn write_int(out: &mut Vec<u8>, value: u32) {
    out.push(b'i');
    write_decimal(out, u64::from(value));
    out.push(b'e');
}

/// Replace `out` with the announce reply, retaining its allocation for reuse.
///
/// Keys are written in the ascending byte order bencode requires, so no
/// intermediate dictionary is built. `peers` is always present; `peers6` only
/// when at least one IPv6 candidate survives the 100-peer ceiling.
pub fn encode_http_announce_into(out: &mut Vec<u8>, r: &AnnounceResponse) {
    out.clear();
    let peers = &r.peers[..r.peers.len().min(MAX_NUMWANT)];
    let v4_count = peers.iter().filter(|peer| peer.is_ipv4()).count();
    let v6_count = peers.len() - v4_count;
    out.reserve(64 + v4_count * 6 + v6_count * 18);
    out.push(b'd');
    write_key(out, b"complete");
    write_int(out, r.complete);
    write_key(out, b"incomplete");
    write_int(out, r.incomplete);
    write_key(out, b"interval");
    write_int(out, r.interval);
    write_key(out, b"min interval");
    write_int(out, r.min_interval);
    write_key(out, b"peers");
    write_decimal(out, (v4_count * 6) as u64);
    out.push(b':');
    for peer in peers {
        if let SocketAddr::V4(a) = peer {
            out.extend_from_slice(&a.ip().octets());
            out.extend_from_slice(&a.port().to_be_bytes());
        }
    }
    if v6_count > 0 {
        write_key(out, b"peers6");
        write_decimal(out, (v6_count * 18) as u64);
        out.push(b':');
        for peer in peers {
            if let SocketAddr::V6(a) = peer {
                out.extend_from_slice(&a.ip().octets());
                out.extend_from_slice(&a.port().to_be_bytes());
            }
        }
    }
    out.push(b'e');
}

pub fn encode_http_announce(r: &AnnounceResponse) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    encode_http_announce_into(&mut out, r);
    out
}

/// Replace `out` with a bencode failure reply, retaining its allocation.
pub fn encode_http_failure_into(out: &mut Vec<u8>, reason: &str) {
    out.clear();
    let reason = &reason.as_bytes()[..reason.len().min(1024)];
    out.reserve(reason.len() + 24);
    out.push(b'd');
    write_key(out, b"failure reason");
    write_decimal(out, reason.len() as u64);
    out.push(b':');
    out.extend_from_slice(reason);
    out.push(b'e');
}

pub fn encode_http_failure(reason: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(40 + reason.len().min(1024));
    encode_http_failure_into(&mut out, reason);
    out
}

/// Replace `out` with the scrape reply, retaining its allocation for reuse.
///
/// `files` is keyed by info hash, so entries are index-sorted on the stack
/// (at most 64 keys) instead of being copied into a `BTreeMap`. Repeated
/// hashes keep the last supplied entry, matching map insertion semantics.
pub fn encode_http_scrape_into(out: &mut Vec<u8>, entries: &[ScrapeEntry]) {
    out.clear();
    let len = entries.len().min(MAX_SCRAPE_HASHES);
    let mut order = [0usize; MAX_SCRAPE_HASHES];
    for (index, slot) in order[..len].iter_mut().enumerate() {
        *slot = index;
    }
    order[..len].sort_unstable_by_key(|index| (entries[*index].info_hash, *index));
    out.reserve(16 + len * 96);
    out.push(b'd');
    write_key(out, b"files");
    out.push(b'd');
    for (offset, index) in order[..len].iter().enumerate() {
        // Equal hashes are ordered by original position, so skipping a run's
        // non-final entries keeps the last occurrence only.
        if offset + 1 < len && entries[*index].info_hash == entries[order[offset + 1]].info_hash {
            continue;
        }
        let entry = &entries[*index];
        write_key(out, &entry.info_hash);
        out.push(b'd');
        write_key(out, b"complete");
        write_int(out, entry.complete);
        write_key(out, b"downloaded");
        write_int(out, entry.downloaded);
        write_key(out, b"incomplete");
        write_int(out, entry.incomplete);
        out.push(b'e');
    }
    out.push(b'e');
    out.push(b'e');
}

pub fn encode_http_scrape(entries: &[ScrapeEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + entries.len().min(MAX_SCRAPE_HASHES) * 96);
    encode_http_scrape_into(&mut out, entries);
    out
}

fn escaped(raw: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(raw.len() * 3);
    for b in raw {
        out.push('%');
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 15) as usize] as char);
    }
    out
}

pub fn encode_http_announce_query(r: &AnnounceRequest) -> String {
    let event = match r.event {
        AnnounceEvent::None => "",
        AnnounceEvent::Started => "&event=started",
        AnnounceEvent::Completed => "&event=completed",
        AnnounceEvent::Stopped => "&event=stopped",
    };
    format!(
        "info_hash={}&peer_id={}&port={}&uploaded={}&downloaded={}&left={}&numwant={}&compact=1{}",
        escaped(&r.info_hash),
        escaped(&r.peer_id),
        r.port,
        r.uploaded,
        r.downloaded,
        r.left,
        r.numwant,
        event
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bencode::{self, Value};
    use std::collections::BTreeMap;

    fn legacy_int(value: u32) -> Value {
        Value::Int(value as i64)
    }

    // Frozen copy of the pre-optimization map-based encoder. It is kept only
    // as the byte-equality oracle for the direct writers above; the production
    // path no longer builds `Value` trees or copies dictionary keys.
    fn legacy_announce(r: &AnnounceResponse) -> Vec<u8> {
        let mut v4 = Vec::new();
        let mut v6 = Vec::new();
        for peer in r.peers.iter().take(MAX_NUMWANT) {
            match peer {
                SocketAddr::V4(a) => {
                    v4.extend_from_slice(&a.ip().octets());
                    v4.extend_from_slice(&a.port().to_be_bytes());
                }
                SocketAddr::V6(a) => {
                    v6.extend_from_slice(&a.ip().octets());
                    v6.extend_from_slice(&a.port().to_be_bytes());
                }
            }
        }
        let mut map = BTreeMap::new();
        map.insert(b"complete".to_vec(), legacy_int(r.complete));
        map.insert(b"incomplete".to_vec(), legacy_int(r.incomplete));
        map.insert(b"interval".to_vec(), legacy_int(r.interval));
        map.insert(b"min interval".to_vec(), legacy_int(r.min_interval));
        map.insert(b"peers".to_vec(), Value::Bytes(v4));
        if !v6.is_empty() {
            map.insert(b"peers6".to_vec(), Value::Bytes(v6));
        }
        bencode::encode(&Value::Dict(map))
    }

    fn legacy_failure(reason: &str) -> Vec<u8> {
        let mut map = BTreeMap::new();
        map.insert(
            b"failure reason".to_vec(),
            Value::Bytes(reason.as_bytes()[..reason.len().min(1024)].to_vec()),
        );
        bencode::encode(&Value::Dict(map))
    }

    fn legacy_scrape(entries: &[ScrapeEntry]) -> Vec<u8> {
        let mut files = BTreeMap::new();
        for e in entries.iter().take(MAX_SCRAPE_HASHES) {
            let mut counts = BTreeMap::new();
            counts.insert(b"complete".to_vec(), legacy_int(e.complete));
            counts.insert(b"downloaded".to_vec(), legacy_int(e.downloaded));
            counts.insert(b"incomplete".to_vec(), legacy_int(e.incomplete));
            files.insert(e.info_hash.to_vec(), Value::Dict(counts));
        }
        let mut root = BTreeMap::new();
        root.insert(b"files".to_vec(), Value::Dict(files));
        bencode::encode(&Value::Dict(root))
    }

    fn v4_peer(a: u8, b: u8, port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4([8, a, b, 1].into()), port)
    }

    fn v6_peer(suffix: u16, port: u16) -> SocketAddr {
        SocketAddr::new(
            IpAddr::V6(std::net::Ipv6Addr::new(
                0x2001, 0x4860, 0x4860, 0, 0, 0, 0, suffix,
            )),
            port,
        )
    }

    fn response(peers: Vec<SocketAddr>) -> AnnounceResponse {
        AnnounceResponse {
            interval: 1800,
            min_interval: 300,
            complete: 1,
            incomplete: 2,
            peers,
        }
    }

    #[test]
    fn announce_into_matches_the_map_oracle_for_every_peer_shape() {
        let cases = [
            ("empty", response(Vec::new())),
            (
                "v4 only",
                response(vec![v4_peer(8, 8, 6881), v4_peer(1, 1, 1)]),
            ),
            (
                "v4 and v6",
                response(vec![
                    v4_peer(8, 8, 6881),
                    v6_peer(0x8888, 6882),
                    v4_peer(9, 9, 0),
                    v6_peer(1, u16::MAX),
                ]),
            ),
            ("v6 only", response(vec![v6_peer(2, 6969), v6_peer(3, 1)])),
            ("defaults", AnnounceResponse::default()),
        ];
        for (name, r) in cases {
            let expected = legacy_announce(&r);
            assert_eq!(encode_http_announce(&r), expected, "case {name}");
            let mut out = vec![0xaa; 32];
            encode_http_announce_into(&mut out, &r);
            assert_eq!(out, expected, "case {name} into reused buffer");
        }
    }

    #[test]
    fn announce_into_truncates_at_one_hundred_total_candidates() {
        let mut peers = vec![v4_peer(1, 1, 6881); 60];
        peers.extend(vec![v6_peer(9, 6881); 60]);
        let r = response(peers);
        assert_eq!(encode_http_announce(&r), legacy_announce(&r));
        let bencode::Value::Dict(map) = bencode::decode(&encode_http_announce(&r)).unwrap() else {
            panic!()
        };
        let bencode::Value::Bytes(p4) = &map[b"peers".as_slice()] else {
            panic!()
        };
        let bencode::Value::Bytes(p6) = &map[b"peers6".as_slice()] else {
            panic!()
        };
        assert_eq!(p4.len() / 6 + p6.len() / 18, 100);
        // Beyond the ceiling the extra v6 candidates must be dropped entirely.
        let overflow = response(vec![v6_peer(7, 6881); 150]);
        let encoded = encode_http_announce(&overflow);
        assert_eq!(encoded, legacy_announce(&overflow));
        let bencode::Value::Dict(map) = bencode::decode(&encoded).unwrap() else {
            panic!()
        };
        let bencode::Value::Bytes(p6) = &map[b"peers6".as_slice()] else {
            panic!()
        };
        assert_eq!(p6.len(), 100 * 18);
    }

    #[test]
    fn announce_bytes_are_the_documented_wire_sequence() {
        let mut expected = Vec::new();
        expected.extend_from_slice(b"d8:completei1e10:incompletei2e8:intervali1800e");
        expected.extend_from_slice(b"12:min intervali300e5:peers6:");
        expected.extend_from_slice(&[8, 8, 8, 1, 0x1a, 0xe1]);
        expected.push(b'e');
        assert_eq!(
            encode_http_announce(&response(vec![v4_peer(8, 8, 6881)])),
            expected
        );
    }

    #[test]
    fn failure_into_matches_the_map_oracle_including_truncation() {
        let long = "x".repeat(5000);
        for reason in [
            "",
            "full",
            "invalid or non-public address",
            "expiry backlog; please retry",
            "内存不足",
            long.as_str(),
        ] {
            let expected = legacy_failure(reason);
            assert_eq!(encode_http_failure(reason), expected, "reason {reason:?}");
            let mut out = b"stale".to_vec();
            encode_http_failure_into(&mut out, reason);
            assert_eq!(out, expected, "reason {reason:?} into reused buffer");
        }
        let truncated = encode_http_failure(&"y".repeat(2000));
        assert_eq!(truncated.len(), legacy_failure(&"y".repeat(1024)).len());
        let bencode::Value::Dict(map) = bencode::decode(&truncated).unwrap() else {
            panic!()
        };
        let bencode::Value::Bytes(reason) = &map[b"failure reason".as_slice()] else {
            panic!()
        };
        assert_eq!(reason.len(), 1024);
    }

    fn entry(hash: u8, complete: u32, downloaded: u32, incomplete: u32) -> ScrapeEntry {
        ScrapeEntry {
            info_hash: [hash; 20],
            complete,
            downloaded,
            incomplete,
        }
    }

    #[test]
    fn scrape_into_matches_the_map_oracle_for_order_duplicates_and_limits() {
        let cases: [(&str, Vec<ScrapeEntry>); 5] = [
            ("empty", Vec::new()),
            ("single", vec![entry(1, 3, 4, 5)]),
            (
                "unsorted",
                vec![entry(9, 1, 0, 0), entry(2, 0, 0, 1), entry(200, 7, 8, 9)],
            ),
            (
                "duplicate hashes keep the last entry",
                vec![entry(1, 1, 1, 1), entry(2, 2, 2, 2), entry(1, 9, 9, 9)],
            ),
            (
                "beyond the 64-hash ceiling",
                (0..100u8)
                    .map(|hash| entry(hash, u32::from(hash), 0, 1))
                    .collect(),
            ),
        ];
        for (name, entries) in cases {
            let expected = legacy_scrape(&entries);
            assert_eq!(encode_http_scrape(&entries), expected, "case {name}");
            let mut out = vec![0x5a; 7];
            encode_http_scrape_into(&mut out, &entries);
            assert_eq!(out, expected, "case {name} into reused buffer");
        }
        let many: Vec<ScrapeEntry> = (0..64u8)
            .map(|hash| entry(hash, u32::from(hash), 1, 2))
            .collect();
        assert_eq!(encode_http_scrape(&many), legacy_scrape(&many));
    }
}
