//! Bounded Team Cymru BGP origin enrichment for tracker server addresses.
//!
//! The country is the prefix's RIR registration country, not a physical
//! location. Results never identify or query any peer discovered by a tracker.
//! Callers must pass only a tracker server IP actually selected for a probe.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use hickory_resolver::TokioAsyncResolver;
use tokio::sync::Semaphore;
use tracker_model::{Infrastructure, unix_now};

const CACHE_CAPACITY: usize = 4096;
const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_TXT_RECORDS: usize = 8;
const MAX_TXT_BYTES: usize = 1024;

struct CacheEntry {
    value: Infrastructure,
    expires: Instant,
    order: u64,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<IpAddr, CacheEntry>,
    order: BTreeMap<u64, IpAddr>,
    sequence: u64,
}

impl Cache {
    fn next_order(&mut self) -> u64 {
        // A wrap is practically unreachable, but keeping order unique makes
        // the hard cache bound independent of that assumption.
        if self.sequence == u64::MAX {
            let ordered: Vec<IpAddr> = self.order.values().copied().collect();
            self.order.clear();
            self.sequence = 0;
            for ip in ordered {
                self.sequence += 1;
                if let Some(entry) = self.entries.get_mut(&ip) {
                    entry.order = self.sequence;
                    self.order.insert(self.sequence, ip);
                }
            }
        }
        self.sequence += 1;
        self.sequence
    }

    fn get(&mut self, ip: IpAddr, now: Instant) -> Option<Infrastructure> {
        let entry = self.entries.get(&ip)?;
        if now >= entry.expires {
            let order = entry.order;
            self.entries.remove(&ip);
            self.order.remove(&order);
            return None;
        }
        let old_order = entry.order;
        let value = entry.value.clone();
        self.order.remove(&old_order);
        let order = self.next_order();
        self.entries.get_mut(&ip).expect("cached entry").order = order;
        self.order.insert(order, ip);
        Some(value)
    }

    fn insert(&mut self, ip: IpAddr, value: Infrastructure, now: Instant) {
        if let Some(old) = self.entries.remove(&ip) {
            self.order.remove(&old.order);
        }
        while self.entries.len() >= CACHE_CAPACITY {
            let Some((order, oldest)) = self.order.pop_first() else {
                break;
            };
            let _ = order;
            self.entries.remove(&oldest);
        }
        let order = self.next_order();
        self.order.insert(order, ip);
        self.entries.insert(
            ip,
            CacheEntry {
                value,
                expires: now + CACHE_TTL,
                order,
            },
        );
    }
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

fn permits() -> &'static Arc<Semaphore> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS.get_or_init(|| Arc::new(Semaphore::new(4)))
}

fn cached(ip: IpAddr) -> Option<Infrastructure> {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(ip, Instant::now())
}

fn origin_name(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, d] = ip.octets();
            format!("{d}.{c}.{b}.{a}.origin.asn.cymru.com.")
        }
        IpAddr::V6(ip) => {
            // DNS origin6 uses all 32 hexadecimal nibbles, in reverse order.
            let mut name = String::with_capacity(90);
            for &byte in ip.octets().iter().rev() {
                use std::fmt::Write;
                let _ = write!(&mut name, "{:x}.{:x}.", byte & 0x0f, byte >> 4);
            }
            name.push_str("origin6.asn.cymru.com.");
            name
        }
    }
}

fn parse_asn(value: &str) -> Option<u32> {
    let s = value.trim();
    let digits = s
        .strip_prefix("AS")
        .or_else(|| s.strip_prefix("as"))
        .unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u32>().ok().filter(|v| *v != 0)
}

fn prefix_contains(prefix: &str, ip: IpAddr) -> bool {
    let Some((address, bits)) = prefix.split_once('/') else {
        return false;
    };
    let Ok(address) = address.parse::<IpAddr>() else {
        return false;
    };
    let Ok(bits) = bits.parse::<u32>() else {
        return false;
    };
    match (address, ip) {
        (IpAddr::V4(a), IpAddr::V4(b)) if bits <= 32 => {
            let mask = if bits == 0 {
                0
            } else {
                u32::MAX << (32 - bits)
            };
            u32::from(a) & mask == u32::from(b) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(b)) if bits <= 128 => {
            let mask = if bits == 0 {
                0
            } else {
                u128::MAX << (128 - bits)
            };
            u128::from(a) & mask == u128::from(b) & mask
        }
        _ => false,
    }
}

fn parse_origin(value: &str, ip: IpAddr) -> Option<(u32, Option<String>)> {
    let mut fields = value.split('|').map(str::trim);
    let asn = parse_asn(fields.next()?)?;
    let prefix = fields.next()?;
    if !prefix_contains(prefix, ip) {
        return None;
    }
    let country = fields.next().and_then(|s| {
        let upper = s.to_ascii_uppercase();
        (upper.len() == 2 && upper.bytes().all(|c| c.is_ascii_uppercase()) && upper != "ZZ")
            .then_some(upper)
    });
    Some((asn, country))
}

fn parse_organization(value: &str, expected_asn: u32) -> Option<String> {
    let mut fields = value.splitn(5, '|').map(str::trim);
    if parse_asn(fields.next()?)? != expected_asn {
        return None;
    }
    let _country = fields.next()?;
    let _registry = fields.next()?;
    let _allocated = fields.next()?;
    let description = fields.next()?;
    if description.is_empty()
        || description.len() > 256
        || description.chars().any(char::is_control)
    {
        return None;
    }
    Some(description.to_owned())
}

async fn txt(resolver: &TokioAsyncResolver, name: String) -> Option<Vec<String>> {
    let records = tokio::time::timeout(QUERY_TIMEOUT, resolver.txt_lookup(name))
        .await
        .ok()?
        .ok()?;
    let mut out = Vec::new();
    for record in records.iter() {
        if out.len() >= MAX_TXT_RECORDS {
            return None;
        }
        let mut bytes = Vec::new();
        for part in record.txt_data() {
            if bytes.len().checked_add(part.len())? > MAX_TXT_BYTES {
                return None;
            }
            bytes.extend_from_slice(part);
        }
        out.push(String::from_utf8(bytes).ok()?);
    }
    (!out.is_empty()).then_some(out)
}

async fn query(ip: IpAddr) -> Infrastructure {
    let Ok(resolver) = TokioAsyncResolver::tokio_from_system_conf() else {
        return Infrastructure::default();
    };
    let Some(records) = txt(&resolver, origin_name(ip)).await else {
        return Infrastructure::default();
    };
    let mut origin = None;
    for record in records {
        let Some((asn, country)) = parse_origin(&record, ip) else {
            continue;
        };
        if let Some((old_asn, old_country)) = &mut origin {
            if *old_asn != asn {
                return Infrastructure::default();
            }
            if *old_country != country {
                *old_country = None;
            }
        } else {
            origin = Some((asn, country));
        }
    }
    let Some((asn, country)) = origin else {
        return Infrastructure::default();
    };
    let details = txt(&resolver, format!("AS{asn}.asn.cymru.com.")).await;
    let organization = details.as_ref().and_then(|records| {
        records
            .iter()
            .find_map(|record| parse_organization(record, asn))
    });
    let value = Infrastructure {
        asn: Some(format!("AS{asn}")),
        organization,
        country,
        updated_at: Some(unix_now()),
    };
    // A failed/partial AS-name query can be retried on a future probe. Do not
    // turn a transient network failure into a seven-day negative cache.
    if value.organization.is_some() {
        cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ip, value.clone(), Instant::now());
    }
    value
}

/// Resolve a *tracker server* IP only; the caller must supply the address
/// pinned for the actual health probe, never an address returned as a peer.
/// Unknown, saturation, timeout and DNS failure return default/partial data.
pub async fn lookup(ip: IpAddr) -> Infrastructure {
    if !crate::peer::public_ip(ip) {
        return Infrastructure::default();
    }
    if let Some(value) = cached(ip) {
        return value;
    }
    let Ok(permit) = Arc::clone(permits()).try_acquire_owned() else {
        return Infrastructure::default();
    };
    // A cancelled caller must not release capacity while a lookup is still
    // running. This task owns the permit, and at most four tasks can exist.
    let task = tokio::spawn(async move {
        let _permit = permit;
        if let Some(value) = cached(ip) {
            return value;
        }
        query(ip).await
    });
    task.await.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn ipv4_and_ipv6_origin_names_reverse_exactly() {
        assert_eq!(
            origin_name("216.90.108.31".parse().unwrap()),
            "31.108.90.216.origin.asn.cymru.com."
        );
        assert_eq!(
            origin_name("2001:4860:b002::68".parse().unwrap()),
            "8.6.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.2.0.0.b.0.6.8.4.1.0.0.2.origin6.asn.cymru.com."
        );
    }

    #[test]
    fn txt_parsers_preserve_unknowns_and_reject_ambiguous_or_spoofed_data() {
        let ip = "8.8.8.8".parse().unwrap();
        assert_eq!(
            parse_origin("15169 | 8.8.8.0/24 | US | arin | 1992-12-01", ip),
            Some((15169, Some("US".into())))
        );
        assert_eq!(
            parse_origin("15169 | 8.8.8.0/24 | -- | arin | 1992-12-01", ip),
            Some((15169, None))
        );
        assert!(parse_origin("15169 123 | 8.8.8.0/24 | US", ip).is_none());
        assert!(parse_origin("0 | 8.8.8.0/24 | US", ip).is_none());
        assert!(parse_origin("15169 | 8.8.4.0/24 | US", ip).is_none());
        assert!(prefix_contains(
            "2001:4860::/32",
            "2001:4860:b002::68".parse().unwrap()
        ));
        assert!(!prefix_contains(
            "2001:4860::/129",
            "2001:4860::1".parse().unwrap()
        ));
        assert_eq!(
            parse_organization(
                "15169 | US | arin | 2000-03-30 | GOOGLE - Google LLC, US",
                15169
            ),
            Some("GOOGLE - Google LLC, US".into())
        );
        assert!(parse_organization("15170 | US | arin | 2000-03-30 | wrong AS", 15169).is_none());
        assert!(parse_organization("15169 | US | arin | 2000-03-30 | bad\nname", 15169).is_none());
    }

    #[test]
    fn bounded_lru_expires_and_evicts_least_recently_used() {
        let mut cache = Cache::default();
        let now = Instant::now();
        let ip = |i: u32| IpAddr::V4(Ipv4Addr::from(i));
        for i in 0..CACHE_CAPACITY as u32 {
            cache.insert(
                ip(i),
                Infrastructure {
                    asn: Some(format!("AS{i}")),
                    ..Infrastructure::default()
                },
                now,
            );
        }
        assert_eq!(cache.entries.len(), CACHE_CAPACITY);
        assert!(cache.get(ip(0), now).is_some());
        cache.insert(ip(CACHE_CAPACITY as u32), Infrastructure::default(), now);
        assert_eq!(cache.entries.len(), CACHE_CAPACITY);
        assert!(cache.get(ip(0), now).is_some());
        assert!(cache.get(ip(1), now).is_none());
        assert!(cache.get(ip(0), now + CACHE_TTL).is_none());
        assert_eq!(cache.entries.len(), cache.order.len());
    }

    #[tokio::test]
    async fn non_public_addresses_return_unknown_without_dns() {
        for ip in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            "2001:db8::1".parse().unwrap(),
        ] {
            let value = lookup(ip).await;
            assert!(
                value.asn.is_none()
                    && value.organization.is_none()
                    && value.country.is_none()
                    && value.updated_at.is_none()
            );
        }
    }
}
