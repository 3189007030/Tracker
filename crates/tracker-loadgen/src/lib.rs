//! Independent client-side validation and bounded accounting for owned-target tests.
use bt_protocol::bencode::{Value, decode};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};

pub const UDP_MAGIC: u64 = 0x0417_2710_1980;
pub const MAX_HTTP_BODY: u64 = 16 * 1024;
pub const MAX_UDP_PACKET: u64 = 1200;
pub const MAX_TARGET_BYTES: usize = 2048;
pub const HARD_MAX_BYTES: u64 = 18 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyError {
    Malformed,
    TransactionMismatch,
    TrackerFailure,
    NonPublicPeer,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrackerReply {
    pub interval: u32,
    pub complete: u32,
    pub incomplete: u32,
    pub peers: Vec<SocketAddr>,
}

fn integer(value: Option<&Value>) -> Option<u32> {
    match value {
        Some(Value::Int(v)) if *v >= 0 && *v <= u32::MAX as i64 => Some(*v as u32),
        _ => None,
    }
}

fn compact(data: &[u8], v6: bool) -> Result<Vec<SocketAddr>, ReplyError> {
    let size = if v6 { 18 } else { 6 };
    if !data.len().is_multiple_of(size) {
        return Err(ReplyError::Malformed);
    }
    let mut peers = Vec::with_capacity(data.len() / size);
    for part in data.chunks_exact(size) {
        let ip = if v6 {
            let octets: [u8; 16] = part[..16].try_into().map_err(|_| ReplyError::Malformed)?;
            IpAddr::V6(Ipv6Addr::from(octets))
        } else {
            IpAddr::V4(Ipv4Addr::new(part[0], part[1], part[2], part[3]))
        };
        let port = u16::from_be_bytes([part[size - 2], part[size - 1]]);
        if port == 0 {
            return Err(ReplyError::Malformed);
        }
        peers.push(SocketAddr::new(ip, port));
    }
    Ok(peers)
}

fn permitted_peer(ip: IpAddr, allow_private: bool) -> bool {
    match ip {
        IpAddr::V4(v) => {
            if v.is_unspecified() || v.is_multicast() || v.is_broadcast() {
                return false;
            }
            if allow_private {
                return true;
            }
            let [a, b, c, _] = v.octets();
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_documentation()
                || a == 0
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19)))
        }
        IpAddr::V6(v) => {
            if v.is_unspecified() || v.is_multicast() {
                return false;
            }
            if allow_private {
                return true;
            }
            let s = v.segments();
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] <= 0x01ff || s[1] == 0x0db8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] <= 0x0fff)
        }
    }
}

fn validate_peer_count(
    peers: &[SocketAddr],
    requested: usize,
    allow_private: bool,
) -> Result<(), ReplyError> {
    if peers.len() > requested {
        return Err(ReplyError::Malformed);
    }
    let mut seen = HashSet::with_capacity(peers.len());
    if !peers.iter().all(|peer| seen.insert(*peer)) {
        return Err(ReplyError::Malformed);
    }
    if !peers
        .iter()
        .all(|peer| permitted_peer(peer.ip(), allow_private))
    {
        return Err(ReplyError::NonPublicPeer);
    }
    Ok(())
}

pub fn parse_http_announce(
    body: &[u8],
    requested: usize,
    allow_private: bool,
) -> Result<TrackerReply, ReplyError> {
    if body.len() as u64 > MAX_HTTP_BODY {
        return Err(ReplyError::Malformed);
    }
    let Value::Dict(dict) = decode(body).map_err(|_| ReplyError::Malformed)? else {
        return Err(ReplyError::Malformed);
    };
    if dict.contains_key(b"failure reason".as_slice()) {
        return Err(ReplyError::TrackerFailure);
    }
    let interval = integer(dict.get(b"interval".as_slice())).ok_or(ReplyError::Malformed)?;
    let mut reply = TrackerReply {
        interval,
        complete: integer(dict.get(b"complete".as_slice())).unwrap_or(0),
        incomplete: integer(dict.get(b"incomplete".as_slice())).unwrap_or(0),
        peers: Vec::new(),
    };
    let mut has_peers = false;
    if let Some(value) = dict.get(b"peers".as_slice()) {
        has_peers = true;
        match value {
            Value::Bytes(data) => reply.peers.extend(compact(data, false)?),
            Value::List(entries) => {
                for entry in entries {
                    let Value::Dict(fields) = entry else {
                        return Err(ReplyError::Malformed);
                    };
                    let ip = match fields.get(b"ip".as_slice()) {
                        Some(Value::Bytes(value)) => std::str::from_utf8(value)
                            .ok()
                            .and_then(|v| v.parse::<IpAddr>().ok())
                            .ok_or(ReplyError::Malformed)?,
                        _ => return Err(ReplyError::Malformed),
                    };
                    let port = integer(fields.get(b"port".as_slice()))
                        .filter(|p| *p > 0 && *p <= u16::MAX as u32)
                        .ok_or(ReplyError::Malformed)? as u16;
                    reply.peers.push(SocketAddr::new(ip, port));
                }
            }
            _ => return Err(ReplyError::Malformed),
        }
    }
    if let Some(value) = dict.get(b"peers6".as_slice()) {
        has_peers = true;
        let Value::Bytes(data) = value else {
            return Err(ReplyError::Malformed);
        };
        reply.peers.extend(compact(data, true)?);
    }
    if !has_peers && requested != 0 {
        return Err(ReplyError::Malformed);
    }
    validate_peer_count(&reply.peers, requested, allow_private)?;
    Ok(reply)
}

pub fn parse_http_scrape(body: &[u8], hash: &[u8; 20]) -> Result<(u32, u32), ReplyError> {
    if body.len() as u64 > MAX_HTTP_BODY {
        return Err(ReplyError::Malformed);
    }
    let Value::Dict(dict) = decode(body).map_err(|_| ReplyError::Malformed)? else {
        return Err(ReplyError::Malformed);
    };
    if dict.contains_key(b"failure reason".as_slice()) {
        return Err(ReplyError::TrackerFailure);
    }
    let Some(Value::Dict(files)) = dict.get(b"files".as_slice()) else {
        return Err(ReplyError::Malformed);
    };
    let Some(Value::Dict(entry)) = files.get(hash.as_slice()) else {
        return Err(ReplyError::Malformed);
    };
    Ok((
        integer(entry.get(b"complete".as_slice())).ok_or(ReplyError::Malformed)?,
        integer(entry.get(b"incomplete".as_slice())).ok_or(ReplyError::Malformed)?,
    ))
}

pub fn udp_connect_request(tx: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(16);
    p.extend_from_slice(&UDP_MAGIC.to_be_bytes());
    p.extend_from_slice(&0_u32.to_be_bytes());
    p.extend_from_slice(&tx.to_be_bytes());
    p
}

#[derive(Debug, Clone, Copy)]
pub struct Announce<'a> {
    pub hash: &'a [u8; 20],
    pub peer_id: &'a [u8; 20],
    pub port: u16,
    pub event: u32,
    pub numwant: i32,
    pub key: u32,
}

pub fn udp_announce_request(conn: u64, tx: u32, req: &Announce<'_>) -> Vec<u8> {
    let mut p = Vec::with_capacity(98);
    p.extend_from_slice(&conn.to_be_bytes());
    p.extend_from_slice(&1_u32.to_be_bytes());
    p.extend_from_slice(&tx.to_be_bytes());
    p.extend_from_slice(req.hash);
    p.extend_from_slice(req.peer_id);
    p.extend_from_slice(&0_u64.to_be_bytes()); // downloaded
    p.extend_from_slice(&1_u64.to_be_bytes()); // incomplete; do not claim a seed
    p.extend_from_slice(&0_u64.to_be_bytes()); // uploaded
    p.extend_from_slice(&req.event.to_be_bytes());
    p.extend_from_slice(&0_u32.to_be_bytes()); // origin IP, never spoofed
    p.extend_from_slice(&req.key.to_be_bytes());
    p.extend_from_slice(&req.numwant.to_be_bytes());
    p.extend_from_slice(&req.port.to_be_bytes());
    p
}

pub fn udp_scrape_request(conn: u64, tx: u32, hash: &[u8; 20]) -> Vec<u8> {
    let mut p = Vec::with_capacity(36);
    p.extend_from_slice(&conn.to_be_bytes());
    p.extend_from_slice(&2_u32.to_be_bytes());
    p.extend_from_slice(&tx.to_be_bytes());
    p.extend_from_slice(hash);
    p
}

fn udp_header(data: &[u8], tx: u32, action: u32, min_len: usize) -> Result<(), ReplyError> {
    if data.len() < 8 || data.len() as u64 > MAX_UDP_PACKET {
        return Err(ReplyError::Malformed);
    }
    let got_action = u32::from_be_bytes(data[0..4].try_into().unwrap());
    let got_tx = u32::from_be_bytes(data[4..8].try_into().unwrap());
    if got_tx != tx {
        return Err(ReplyError::TransactionMismatch);
    }
    if got_action == 3 {
        return Err(ReplyError::TrackerFailure);
    }
    if got_action != action || data.len() < min_len {
        return Err(ReplyError::Malformed);
    }
    Ok(())
}

pub fn parse_udp_connect(data: &[u8], tx: u32) -> Result<u64, ReplyError> {
    udp_header(data, tx, 0, 16)?;
    if data.len() != 16 {
        return Err(ReplyError::Malformed);
    }
    Ok(u64::from_be_bytes(data[8..16].try_into().unwrap()))
}

pub fn parse_udp_announce(
    data: &[u8],
    tx: u32,
    v6: bool,
    requested: usize,
    allow_private: bool,
) -> Result<TrackerReply, ReplyError> {
    udp_header(data, tx, 1, 20)?;
    let peers = compact(&data[20..], v6)?;
    validate_peer_count(&peers, requested, allow_private)?;
    Ok(TrackerReply {
        interval: u32::from_be_bytes(data[8..12].try_into().unwrap()),
        incomplete: u32::from_be_bytes(data[12..16].try_into().unwrap()),
        complete: u32::from_be_bytes(data[16..20].try_into().unwrap()),
        peers,
    })
}

pub fn parse_udp_scrape(data: &[u8], tx: u32) -> Result<(u32, u32), ReplyError> {
    udp_header(data, tx, 2, 20)?;
    if data.len() != 20 {
        return Err(ReplyError::Malformed);
    }
    Ok((
        u32::from_be_bytes(data[8..12].try_into().unwrap()),
        u32::from_be_bytes(data[16..20].try_into().unwrap()),
    ))
}

/// Only classify explicit connection-cookie failures; ordinary overload or
/// tracker rejection must not trigger a fresh UDP handshake on every request.
pub fn udp_cookie_rejected(data: &[u8]) -> bool {
    if data.len() < 8 || data[..4] != 3_u32.to_be_bytes() {
        return false;
    }
    let message = String::from_utf8_lossy(&data[8..]).to_ascii_lowercase();
    message.contains("connection") || message.contains("cookie")
}

pub fn percent(data: &[u8]) -> String {
    let mut result = String::with_capacity(data.len() * 3);
    const HEX: &[u8] = b"0123456789ABCDEF";
    for &b in data {
        result.push('%');
        result.push(HEX[(b >> 4) as usize] as char);
        result.push(HEX[(b & 15) as usize] as char);
    }
    result
}

/// Reservation prevents parallel operations exceeding the configured application-byte budget.
pub struct ByteBudget {
    remaining: AtomicU64,
    tx: AtomicU64,
    rx: AtomicU64,
}

impl ByteBudget {
    pub fn new(max: u64) -> Self {
        Self {
            remaining: AtomicU64::new(max),
            tx: AtomicU64::new(0),
            rx: AtomicU64::new(0),
        }
    }
    pub fn reserve(&self, amount: u64) -> bool {
        self.remaining
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_sub(amount)
            })
            .is_ok()
    }
    pub fn settle(&self, reserved: u64, tx: u64, rx: u64) {
        assert!(tx.saturating_add(rx) <= reserved, "reservation exceeded");
        self.tx.fetch_add(tx, Ordering::Relaxed);
        self.rx.fetch_add(rx, Ordering::Relaxed);
        self.remaining
            .fetch_add(reserved - tx - rx, Ordering::Release);
    }
    pub fn totals(&self) -> (u64, u64, u64) {
        (
            self.tx.load(Ordering::Relaxed),
            self.rx.load(Ordering::Relaxed),
            self.remaining.load(Ordering::Relaxed),
        )
    }
}

/// One-millisecond bounded histogram; overflow records >= 60 seconds.
pub struct Histogram {
    counts: Box<[AtomicU64]>,
    total: AtomicU64,
}
impl Default for Histogram {
    fn default() -> Self {
        Self {
            counts: (0..=60_000).map(|_| AtomicU64::new(0)).collect(),
            total: AtomicU64::new(0),
        }
    }
}
impl Histogram {
    pub fn record(&self, millis: u64) {
        self.counts[millis.min(60_000) as usize].fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn percentile_ms(&self, numerator: u64, denominator: u64) -> Option<u64> {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 || denominator == 0 {
            return None;
        }
        let target = total.saturating_mul(numerator).div_ceil(denominator).max(1);
        let mut sum = 0_u64;
        for (bucket, count) in self.counts.iter().enumerate() {
            sum += count.load(Ordering::Relaxed);
            if sum >= target {
                return Some(if bucket == 60_000 {
                    60_000
                } else {
                    bucket as u64 + 1
                });
            }
        }
        None
    }
}

#[derive(Debug, Default, Serialize)]
pub struct StatusCounts(pub BTreeMap<u16, u64>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_success_rejection_and_zero_want_are_distinct() {
        assert_eq!(
            parse_http_announce(b"d8:intervali1800e5:peers0:e", 50, false)
                .unwrap()
                .interval,
            1800
        );
        assert_eq!(
            parse_http_announce(b"d14:failure reason4:busye", 50, false),
            Err(ReplyError::TrackerFailure)
        );
        assert_eq!(
            parse_http_announce(b"d8:intervali1800e5:peers6:\x7f\0\0\x01\x1a\xe1e", 0, true),
            Err(ReplyError::Malformed)
        );
        assert_eq!(
            parse_http_announce(
                b"d8:intervali1800e5:peers6:\x7f\0\0\x01\x1a\xe1e",
                50,
                false
            ),
            Err(ReplyError::NonPublicPeer)
        );
        assert!(
            parse_http_announce(b"d8:intervali1800e5:peers6:\x7f\0\0\x01\x1a\xe1e", 50, true)
                .is_ok()
        );
        assert_eq!(
            parse_http_announce(
                b"d8:intervali1800e5:peers12:\x7f\0\0\x01\x1a\xe1\x7f\0\0\x01\x1a\xe1e",
                50,
                true
            ),
            Err(ReplyError::Malformed)
        );
    }

    #[test]
    fn udp_validates_transaction_type_family_and_cap() {
        let mut valid = Vec::new();
        valid.extend_from_slice(&1_u32.to_be_bytes());
        valid.extend_from_slice(&42_u32.to_be_bytes());
        valid.extend_from_slice(&1800_u32.to_be_bytes());
        valid.extend_from_slice(&1_u32.to_be_bytes());
        valid.extend_from_slice(&2_u32.to_be_bytes());
        valid.extend_from_slice(&[127, 0, 0, 1, 0x1a, 0xe1]);
        assert_eq!(
            parse_udp_announce(&valid, 42, false, 50, true)
                .unwrap()
                .peers
                .len(),
            1
        );
        assert_eq!(
            parse_udp_announce(&valid, 42, false, 50, false),
            Err(ReplyError::NonPublicPeer)
        );
        assert_eq!(
            parse_udp_announce(&valid, 43, false, 50, true),
            Err(ReplyError::TransactionMismatch)
        );
        assert_eq!(
            parse_udp_announce(&valid, 42, true, 50, true),
            Err(ReplyError::Malformed)
        );
        assert_eq!(
            parse_udp_announce(&valid, 42, false, 0, true),
            Err(ReplyError::Malformed)
        );
    }

    #[test]
    fn udp_cookie_refresh_does_not_confuse_overload_with_bad_cookie() {
        let mut packet = Vec::new();
        packet.extend_from_slice(&3_u32.to_be_bytes());
        packet.extend_from_slice(&42_u32.to_be_bytes());
        packet.extend_from_slice(b"Invalid connection ID");
        assert!(udp_cookie_rejected(&packet));
        packet.truncate(8);
        packet.extend_from_slice(b"peer capacity reached");
        assert!(!udp_cookie_rejected(&packet));
    }

    #[test]
    fn reservation_counts_concurrent_work_without_overspending() {
        let b = ByteBudget::new(100);
        assert!(b.reserve(70));
        assert!(!b.reserve(31));
        b.settle(70, 10, 20);
        assert!(b.reserve(60));
        b.settle(60, 25, 35);
        assert_eq!(b.totals(), (35, 55, 10));
    }

    #[test]
    fn concurrent_reservations_never_overspend() {
        let budget = std::sync::Arc::new(ByteBudget::new(100));
        let threads: Vec<_> = (0..100)
            .map(|_| {
                let budget = budget.clone();
                std::thread::spawn(move || {
                    if budget.reserve(10) {
                        budget.settle(10, 10, 0);
                        true
                    } else {
                        false
                    }
                })
            })
            .collect();
        let successes = threads
            .into_iter()
            .map(|t| usize::from(t.join().unwrap()))
            .sum::<usize>();
        assert_eq!(successes, 10);
        assert_eq!(budget.totals(), (100, 0, 0));
    }

    #[test]
    fn histogram_reports_bounded_upper_millisecond() {
        let h = Histogram::default();
        for v in [1, 2, 3, 4, 499] {
            h.record(v);
        }
        assert_eq!(h.percentile_ms(50, 100), Some(4));
        assert_eq!(h.percentile_ms(99, 100), Some(500));
    }
}
