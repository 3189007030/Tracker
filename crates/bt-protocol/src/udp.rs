use crate::{AnnounceEvent, AnnounceRequest, AnnounceResponse, ScrapeEntry, UdpRequest};
use anyhow::{Result, bail, ensure};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

const MAGIC: u64 = 0x0417_2710_1980;
const MAX_HASHES: usize = 64;
const MAX_PEERS: usize = 50;
const MAX_DATAGRAM: usize = 1200;

fn u32_at(data: &[u8], start: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        data.get(start..start + 4)
            .ok_or_else(|| anyhow::anyhow!("truncated packet"))?
            .try_into()?,
    ))
}
fn u64_at(data: &[u8], start: usize) -> Result<u64> {
    Ok(u64::from_be_bytes(
        data.get(start..start + 8)
            .ok_or_else(|| anyhow::anyhow!("truncated packet"))?
            .try_into()?,
    ))
}

pub fn parse_udp(raw: &[u8], source: SocketAddr) -> Result<UdpRequest> {
    ensure!(
        raw.len() >= 16 && raw.len() <= 65507,
        "invalid UDP packet size"
    );
    let connection_id = u64_at(raw, 0)?;
    let action = u32_at(raw, 8)?;
    let transaction_id = u32_at(raw, 12)?;
    match action {
        0 => {
            ensure!(connection_id == MAGIC, "invalid connect magic");
            Ok(UdpRequest::Connect { transaction_id })
        }
        1 => {
            ensure!(raw.len() >= 98, "truncated announce packet");
            let info_hash = raw[16..36].try_into()?;
            let peer_id = raw[36..56].try_into()?;
            let downloaded = u64_at(raw, 56)?;
            let left = u64_at(raw, 64)?;
            let uploaded = u64_at(raw, 72)?;
            let event = match u32_at(raw, 80)? {
                0 => AnnounceEvent::None,
                1 => AnnounceEvent::Completed,
                2 => AnnounceEvent::Started,
                3 => AnnounceEvent::Stopped,
                _ => bail!("invalid announce event"),
            };
            let raw_want = i32::from_be_bytes(raw[92..96].try_into()?);
            let numwant = match raw_want {
                -1 => 50,
                0.. => (raw_want as usize).min(100),
                _ => bail!("invalid numwant"),
            };
            let port = u16::from_be_bytes(raw[96..98].try_into()?);
            ensure!(port != 0, "port must be nonzero");
            Ok(UdpRequest::Announce {
                connection_id,
                transaction_id,
                request: AnnounceRequest {
                    info_hash,
                    peer_id,
                    ip: source.ip(),
                    port,
                    uploaded,
                    downloaded,
                    left,
                    event,
                    numwant,
                },
            })
        }
        2 => {
            let payload = &raw[16..];
            ensure!(
                !payload.is_empty() && payload.len().is_multiple_of(20),
                "invalid scrape hashes"
            );
            ensure!(payload.len() / 20 <= MAX_HASHES, "too many scrape hashes");
            let hashes = payload.as_chunks::<20>().0.to_vec();
            Ok(UdpRequest::Scrape {
                connection_id,
                transaction_id,
                hashes,
            })
        }
        _ => bail!("unknown UDP action"),
    }
}

pub fn encode_udp_connect(transaction_id: u32, connection_id: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&transaction_id.to_be_bytes());
    out.extend_from_slice(&connection_id.to_be_bytes());
    out
}

pub fn encode_udp_announce(transaction_id: u32, r: &AnnounceResponse, is_v6: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + MAX_PEERS * if is_v6 { 18 } else { 6 });
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&transaction_id.to_be_bytes());
    out.extend_from_slice(&r.interval.to_be_bytes());
    out.extend_from_slice(&r.incomplete.to_be_bytes());
    out.extend_from_slice(&r.complete.to_be_bytes());
    for peer in r
        .peers
        .iter()
        .filter(|p| p.is_ipv6() == is_v6)
        .take(MAX_PEERS)
    {
        match peer {
            SocketAddr::V4(a) => {
                out.extend_from_slice(&a.ip().octets());
                out.extend_from_slice(&a.port().to_be_bytes());
            }
            SocketAddr::V6(a) => {
                out.extend_from_slice(&a.ip().octets());
                out.extend_from_slice(&a.port().to_be_bytes());
            }
        }
    }
    debug_assert!(out.len() <= MAX_DATAGRAM);
    out
}

pub fn encode_udp_scrape(transaction_id: u32, entries: &[ScrapeEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + entries.len().min(MAX_HASHES) * 12);
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&transaction_id.to_be_bytes());
    for e in entries.iter().take(MAX_HASHES) {
        out.extend_from_slice(&e.complete.to_be_bytes());
        out.extend_from_slice(&e.downloaded.to_be_bytes());
        out.extend_from_slice(&e.incomplete.to_be_bytes());
    }
    out
}

pub fn encode_udp_error(transaction_id: u32, reason: &str) -> Vec<u8> {
    let reason = reason.as_bytes();
    let mut out = Vec::with_capacity(8 + reason.len().min(MAX_DATAGRAM - 8));
    out.extend_from_slice(&3u32.to_be_bytes());
    out.extend_from_slice(&transaction_id.to_be_bytes());
    out.extend_from_slice(&reason[..reason.len().min(MAX_DATAGRAM - 8)]);
    out
}

// HMAC covers the full 64-bit slot, not merely the 16-bit slot hint in the
// cookie. This prevents a cookie becoming valid again after slot wraparound.
fn cookie_at_slot(secret: &[u8], ip: IpAddr, slot: u64) -> u64 {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(b"tracker-cookie-v1");
    match ip {
        IpAddr::V4(a) => {
            mac.update(&[4]);
            mac.update(&a.octets());
        }
        IpAddr::V6(a) => {
            mac.update(&[6]);
            mac.update(&a.octets());
        }
    }
    mac.update(&slot.to_be_bytes());
    let tag = mac.finalize().into_bytes();
    let mut out = [0u8; 8];
    out[..2].copy_from_slice(&(slot as u16).to_be_bytes());
    out[2..].copy_from_slice(&tag[..6]);
    u64::from_be_bytes(out)
}

/// Generate a BEP 15 connection ID. `now` is Unix seconds; `secret` must be
/// a high-entropy server secret and should be rotated on an operational cycle.
pub fn udp_cookie(secret: &[u8], ip: IpAddr, now: u64) -> u64 {
    cookie_at_slot(secret, ip, now / 60)
}

/// Accept the current and two preceding 60-second slots, so an issued cookie
/// remains usable for at least 120 seconds, including across slot boundaries.
pub fn validate_udp_cookie(secret: &[u8], ip: IpAddr, now: u64, cookie: u64) -> bool {
    let slot = now / 60;
    (0..=2)
        .filter_map(|n| slot.checked_sub(n))
        .any(|candidate| cookie_at_slot(secret, ip, candidate) == cookie)
}

pub fn encode_udp_connect_request(transaction_id: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&MAGIC.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&transaction_id.to_be_bytes());
    out
}

pub fn encode_udp_announce_request(
    connection_id: u64,
    transaction_id: u32,
    r: &AnnounceRequest,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(98);
    out.extend_from_slice(&connection_id.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&transaction_id.to_be_bytes());
    out.extend_from_slice(&r.info_hash);
    out.extend_from_slice(&r.peer_id);
    out.extend_from_slice(&r.downloaded.to_be_bytes());
    out.extend_from_slice(&r.left.to_be_bytes());
    out.extend_from_slice(&r.uploaded.to_be_bytes());
    let event: u32 = match r.event {
        AnnounceEvent::None => 0,
        AnnounceEvent::Completed => 1,
        AnnounceEvent::Started => 2,
        AnnounceEvent::Stopped => 3,
    };
    out.extend_from_slice(&event.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // never request an arbitrary source address
    out.extend_from_slice(&r.peer_id[16..20]); // stable per-session key derived from peer ID
    out.extend_from_slice(&(r.numwant.min(i32::MAX as usize) as i32).to_be_bytes());
    out.extend_from_slice(&r.port.to_be_bytes());
    out
}

pub fn decode_udp_connect_response(data: &[u8], expected_tx: u32) -> Result<u64> {
    ensure!(data.len() >= 16, "truncated connect response");
    ensure!(
        u32_at(data, 0)? == 0 && u32_at(data, 4)? == expected_tx,
        "unexpected connect response"
    );
    u64_at(data, 8)
}

pub fn decode_udp_announce_response(
    data: &[u8],
    expected_tx: u32,
    is_v6: bool,
) -> Result<AnnounceResponse> {
    ensure!(
        data.len() >= 20 && data.len() <= 65507,
        "invalid announce response size"
    );
    ensure!(
        u32_at(data, 0)? == 1 && u32_at(data, 4)? == expected_tx,
        "unexpected announce response"
    );
    let width = if is_v6 { 18 } else { 6 };
    ensure!(
        (data.len() - 20).is_multiple_of(width),
        "invalid compact peer bytes"
    );
    let mut peers = Vec::new();
    for chunk in data[20..].chunks_exact(width) {
        let peer = if is_v6 {
            let ip = Ipv6Addr::from(<[u8; 16]>::try_from(&chunk[..16])?);
            SocketAddr::new(
                IpAddr::V6(ip),
                u16::from_be_bytes(chunk[16..18].try_into()?),
            )
        } else {
            let ip = Ipv4Addr::from(<[u8; 4]>::try_from(&chunk[..4])?);
            SocketAddr::new(IpAddr::V4(ip), u16::from_be_bytes(chunk[4..6].try_into()?))
        };
        peers.push(peer);
    }
    Ok(AnnounceResponse {
        interval: u32_at(data, 8)?,
        min_interval: 0,
        incomplete: u32_at(data, 12)?,
        complete: u32_at(data, 16)?,
        peers,
    })
}
