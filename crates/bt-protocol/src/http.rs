use crate::bencode::{self, Value};
use crate::{AnnounceEvent, AnnounceRequest, AnnounceResponse, InfoHash, ScrapeEntry};
use anyhow::{Result, bail, ensure};
use std::collections::BTreeMap;
use std::net::IpAddr;

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

fn bytes(value: &[u8]) -> Value {
    Value::Bytes(value.to_vec())
}
fn integer(value: u32) -> Value {
    Value::Int(value as i64)
}

pub fn encode_http_announce(r: &AnnounceResponse) -> Vec<u8> {
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    for peer in r.peers.iter().take(MAX_NUMWANT) {
        match peer {
            std::net::SocketAddr::V4(a) => {
                v4.extend_from_slice(&a.ip().octets());
                v4.extend_from_slice(&a.port().to_be_bytes());
            }
            std::net::SocketAddr::V6(a) => {
                v6.extend_from_slice(&a.ip().octets());
                v6.extend_from_slice(&a.port().to_be_bytes());
            }
        }
    }
    let mut map = BTreeMap::new();
    map.insert(b"complete".to_vec(), integer(r.complete));
    map.insert(b"incomplete".to_vec(), integer(r.incomplete));
    map.insert(b"interval".to_vec(), integer(r.interval));
    map.insert(b"min interval".to_vec(), integer(r.min_interval));
    map.insert(b"peers".to_vec(), Value::Bytes(v4));
    if !v6.is_empty() {
        map.insert(b"peers6".to_vec(), Value::Bytes(v6));
    }
    bencode::encode(&Value::Dict(map))
}

pub fn encode_http_failure(reason: &str) -> Vec<u8> {
    let mut map = BTreeMap::new();
    map.insert(
        b"failure reason".to_vec(),
        bytes(&reason.as_bytes()[..reason.len().min(1024)]),
    );
    bencode::encode(&Value::Dict(map))
}

pub fn encode_http_scrape(entries: &[ScrapeEntry]) -> Vec<u8> {
    let mut files = BTreeMap::new();
    for e in entries.iter().take(MAX_SCRAPE_HASHES) {
        let mut counts = BTreeMap::new();
        counts.insert(b"complete".to_vec(), integer(e.complete));
        counts.insert(b"downloaded".to_vec(), integer(e.downloaded));
        counts.insert(b"incomplete".to_vec(), integer(e.incomplete));
        files.insert(e.info_hash.to_vec(), Value::Dict(counts));
    }
    let mut root = BTreeMap::new();
    root.insert(b"files".to_vec(), Value::Dict(files));
    bencode::encode(&Value::Dict(root))
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
