//! Bounded source discovery and per-address-family protocol observations.
use crate::{
    Config,
    store::{Store, normalize},
};
use anyhow::{Context, Result};
use bt_protocol::bencode::{Value, decode};
use hickory_resolver::{TokioAsyncResolver, error::ResolveErrorKind};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::{
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};
use tracker_model::{Observation, unix_now};
use url::Url;

/// Four bounded external measurement tasks may coexist with this one small,
/// loopback-only control request. Unknown pressure conservatively pauses work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pressure {
    Clear,
    Active,
    Unknown,
}
type PressureGate = std::sync::Arc<tokio::sync::RwLock<Pressure>>;
fn pressure_from_json(body: &[u8]) -> Pressure {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Pressure::Unknown;
    };
    let Some(state) = value.get("pressure") else {
        return Pressure::Unknown;
    };
    match (
        state.get("active").and_then(|v| v.as_bool()),
        state.get("reason").and_then(|v| v.as_str()),
    ) {
        (Some(false), Some("none")) => Pressure::Clear,
        (Some(true), Some("memory_admission" | "http_concurrency")) => Pressure::Active,
        _ => Pressure::Unknown,
    }
}
async fn pressure_allows(gate: &PressureGate) -> bool {
    *gate.read().await == Pressure::Clear
}
async fn refresh_pressure(client: &reqwest::Client, gate: &PressureGate) {
    let check = async {
        let mut response = client
            .get("http://127.0.0.1:8081/stats.json")
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if body.len() + chunk.len() > 16 * 1024 {
                return None;
            }
            body.extend_from_slice(&chunk);
        }
        Some(pressure_from_json(&body))
    };
    let state = tokio::time::timeout(Duration::from_millis(200), check)
        .await
        .ok()
        .flatten()
        .unwrap_or(Pressure::Unknown);
    *gate.write().await = state;
}
struct StopMonitor(tokio::task::JoinHandle<()>);
impl Drop for StopMonitor {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct AbortTask<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Deserialize)]
struct Sources {
    tracker_lists: Vec<String>,
    #[serde(default)]
    seed_endpoints: Vec<String>,
    #[serde(default)]
    forum_api_origin: Option<String>,
    #[serde(default)]
    forum_navigation_roots: crate::pool::ForumNavigationRoots,
}

fn public_ip(ip: IpAddr) -> bool {
    crate::peer::public_ip(ip)
}
fn deadline(now: i64, delay: u64) -> i64 {
    (i128::from(now) + i128::from(delay)).min(i128::from(i64::MAX)) as i64
}
fn retry_delay(value: Option<&str>, now: std::time::SystemTime) -> u64 {
    value
        .and_then(|s| {
            let s = s.trim();
            if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
                return Some(s.parse::<u64>().unwrap_or(u64::MAX));
            }
            httpdate::parse_http_date(s)
                .ok()?
                .duration_since(now)
                .ok()
                .map(|d| {
                    d.as_secs()
                        .saturating_add(if d.subsec_nanos() > 0 { 1 } else { 0 })
                })
        })
        .unwrap_or(1800)
        .max(1800)
}
fn local_network_error(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut next = Some(error);
    while let Some(e) = next {
        if let Some(io) = e.downcast_ref::<std::io::Error>()
            && matches!(
                io.kind(),
                std::io::ErrorKind::AddrNotAvailable
                    | std::io::ErrorKind::NetworkUnreachable
                    | std::io::ErrorKind::NetworkDown
            )
        {
            return true;
        }
        next = e.source();
    }
    false
}
async fn pinned(host: &str, port: u16, family: Option<bool>) -> Result<Vec<SocketAddr>> {
    if let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        anyhow::ensure!(public_ip(ip), "non-public address");
        return Ok(if family.is_none_or(|v| ip.is_ipv6() == v) {
            vec![SocketAddr::new(ip, port)]
        } else {
            Vec::new()
        });
    }
    let resolver = TokioAsyncResolver::tokio_from_system_conf()?;
    let mut result = Vec::new();
    if family != Some(true) {
        match tokio::time::timeout(Duration::from_secs(5), resolver.ipv4_lookup(host)).await? {
            Ok(answer) => {
                for item in answer.iter() {
                    let ip: IpAddr = item.to_string().parse()?;
                    anyhow::ensure!(public_ip(ip), "non-public A answer");
                    result.push(SocketAddr::new(ip, port));
                }
            }
            Err(e) => {
                if !matches!(e.kind(), ResolveErrorKind::NoRecordsFound { .. }) {
                    return Err(e.into());
                }
            }
        }
    }
    if family != Some(false) {
        match tokio::time::timeout(Duration::from_secs(5), resolver.ipv6_lookup(host)).await? {
            Ok(answer) => {
                for item in answer.iter() {
                    let ip: IpAddr = item.to_string().parse()?;
                    anyhow::ensure!(public_ip(ip), "non-public AAAA answer");
                    result.push(SocketAddr::new(ip, port));
                }
            }
            Err(e) => {
                if !matches!(e.kind(), ResolveErrorKind::NoRecordsFound { .. }) {
                    return Err(e.into());
                }
            }
        }
    }
    Ok(result)
}

/// Read system DNS TXT; malformed/temporary errors defer rather than probing.
async fn policy_allows(host: &str, scheme: &str, port: u16) -> Result<bool> {
    if host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok()
    {
        return Ok(true);
    }
    let resolver = TokioAsyncResolver::tokio_from_system_conf()?;
    let lookup = tokio::time::timeout(Duration::from_secs(3), resolver.txt_lookup(host)).await?;
    let answer = match lookup {
        Ok(a) => a,
        Err(e) => match e.kind() {
            ResolveErrorKind::NoRecordsFound { .. } => return Ok(true),
            _ => return Err(e.into()),
        },
    };
    let mut count = 0;
    for record in answer.iter() {
        count += 1;
        anyhow::ensure!(count <= 32, "too many TXT records");
        let mut data = Vec::new();
        for part in record.txt_data() {
            data.extend_from_slice(part);
        }
        anyhow::ensure!(data.len() <= 4096, "TXT too long");
        let words: Vec<_> = data
            .split(|b| b.is_ascii_whitespace())
            .filter(|w| !w.is_empty())
            .collect();
        if words.first().copied() != Some(b"BITTORRENT".as_slice()) {
            continue;
        }
        anyhow::ensure!(words.len() <= 128, "TXT policy too long");
        let wanted = format!("{}:{}", if scheme == "udp" { "UDP" } else { "TCP" }, port);
        if !words.iter().skip(1).any(|w| *w == wanted.as_bytes()) {
            return Ok(false);
        }
    }
    Ok(true)
}
async fn client_for(
    url: &Url,
    family: Option<bool>,
) -> Result<Option<(reqwest::Client, SocketAddr)>> {
    let host = url.host_str().context("missing host")?;
    let port = url.port_or_known_default().unwrap_or(80);
    let addresses = pinned(host, port, family).await?;
    let Some(address) = addresses.first().copied() else {
        return Ok(None);
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(12))
        .resolve(host, address)
        .build()?;
    Ok(Some((client, address)))
}
async fn source(url: &str) -> Result<(Vec<String>, Option<u64>)> {
    let url = Url::parse(url)?;
    anyhow::ensure!(
        url.scheme() == "https"
            && matches!(
                url.host_str(),
                Some("raw.githubusercontent.com" | "newtrackon.com")
            )
            && url.username().is_empty()
            && url.password().is_none(),
        "unapproved source"
    );
    let (client, _) = client_for(&url, None).await?.context("no source address")?;
    let mut response = client.get(url).send().await?;
    if matches!(response.status().as_u16(), 403 | 429 | 503) {
        let delay = retry_delay(
            response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|x| x.to_str().ok()),
            std::time::SystemTime::now(),
        );
        return Ok((Vec::new(), Some(delay)));
    }
    anyhow::ensure!(
        response.status().is_success(),
        "source returned non-success"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            body.len() + chunk.len() <= 2 * 1024 * 1024,
            "source too large"
        );
        body.extend_from_slice(&chunk);
    }
    Ok((
        String::from_utf8(body)?
            .lines()
            .filter_map(normalize)
            .take(100_000)
            .collect(),
        None,
    ))
}

async fn official_body(store: &Store, url: &Url, limit: usize) -> Result<Vec<u8>> {
    anyhow::ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && matches!(
                url.host_str(),
                Some(
                    "cdimage.debian.org"
                        | "releases.ubuntu.com"
                        | "torrent.fedoraproject.org"
                        | "archlinux.org"
                )
            ),
        "not an allowed official source"
    );
    let host = format!("source:{}", url.host_str().unwrap_or_default());
    anyhow::ensure!(!store.paused(&host, unix_now())?, "source paused");
    let (client, _) = client_for(url, None)
        .await?
        .context("no official source address")?;
    let mut response = client.get(url.clone()).send().await?;
    if matches!(response.status().as_u16(), 403 | 429 | 503) {
        let delay = retry_delay(
            response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|x| x.to_str().ok()),
            std::time::SystemTime::now(),
        );
        store.pause(&host, deadline(unix_now(), delay))?;
        anyhow::bail!("source deferred");
    }
    anyhow::ensure!(
        response.status().is_success(),
        "source returned non-success"
    );
    let mut out = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            out.len() + chunk.len() <= limit,
            "official response too large"
        );
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}
fn official_sample_link(source: &str, root: &Url, fragment: &str) -> Option<Url> {
    let link = root.join(fragment).ok()?;
    if link.scheme() != "https"
        || link.host_str() != root.host_str()
        || link.query().is_some()
        || link.fragment().is_some()
    {
        return None;
    }
    if link.path().ends_with(".torrent")
        || (source == "arch"
            && link.path().starts_with("/releng/releases/")
            && link.path().ends_with("/torrent/"))
    {
        Some(link)
    } else {
        None
    }
}
fn fedora_torrent_name(name: &str) -> bool {
    name.len() <= 200
        && name.ends_with(".torrent")
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}
async fn collect_samples(store: &Store, start: usize, gate: &PressureGate) -> bool {
    let roots = [
        (
            "debian",
            "https://cdimage.debian.org/debian-cd/current/amd64/bt-cd/",
        ),
        ("ubuntu", "https://releases.ubuntu.com/"),
        ("fedora", "https://torrent.fedoraproject.org/torrents.json"),
        ("arch", "https://archlinux.org/download/"),
    ];
    for offset in 0..roots.len() {
        if !pressure_allows(gate).await {
            return false;
        }
        let (source, root) = roots[(start + offset) % roots.len()];
        let Ok(root) = Url::parse(root) else { continue };
        if source == "fedora" {
            if let Ok(body) = official_body(store, &root, 2 * 1024 * 1024).await
                && let Ok(serde_json::Value::Array(releases)) = serde_json::from_slice(&body)
            {
                let mut found = 0usize;
                for release in releases.iter().rev() {
                    let Some(torrents) = release
                        .get("torrents")
                        .and_then(serde_json::Value::as_array)
                    else {
                        continue;
                    };
                    for torrent in torrents {
                        if !pressure_allows(gate).await {
                            return false;
                        }
                        let Some(name) = torrent.get("torrent").and_then(serde_json::Value::as_str)
                        else {
                            continue;
                        };
                        if !fedora_torrent_name(name) {
                            continue;
                        }
                        let Ok(link) = root.join(&format!("/torrents/{name}")) else {
                            continue;
                        };
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        if !pressure_allows(gate).await {
                            return false;
                        }
                        if let Ok(bytes) = official_body(store, &link, 4 * 1024 * 1024).await
                            && let Ok(sample) = crate::samples::validate_torrent(&bytes)
                        {
                            let _ = store.save_sample(source, &sample);
                            found += 1;
                            if found >= 4 {
                                break;
                            }
                        }
                    }
                    if found >= 4 {
                        break;
                    }
                }
                if found == 0 {
                    tracing::debug!(source, "no public v1 sample discovered");
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        let mut found = 0usize;
        let mut pages = vec![root.clone()];
        let mut attempted = std::collections::HashSet::new();
        while let Some(page) = pages.pop() {
            if !pressure_allows(gate).await {
                return false;
            }
            let Ok(body) = official_body(store, &page, 2 * 1024 * 1024).await else {
                continue;
            };
            let text = String::from_utf8_lossy(&body);
            let mut subpages = Vec::new();
            for fragment in text.split(['\"', '\'']).take(2048) {
                let Ok(link) = page.join(fragment) else {
                    continue;
                };
                if link.host_str() != root.host_str()
                    || link.query().is_some()
                    || link.fragment().is_some()
                {
                    continue;
                }
                if official_sample_link(source, &root, link.as_str()).is_some() {
                    if !pressure_allows(gate).await {
                        return false;
                    }
                    if attempted.len() >= 32 || !attempted.insert(link.to_string()) {
                        continue;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    if !pressure_allows(gate).await {
                        return false;
                    }
                    if let Ok(bytes) = official_body(store, &link, 4 * 1024 * 1024).await
                        && let Ok(sample) = crate::samples::validate_torrent(&bytes)
                    {
                        let _ = store.save_sample(source, &sample);
                        found += 1;
                        if found >= 4 {
                            break;
                        }
                    }
                } else if page == root
                    && link.path().starts_with(root.path())
                    && link.path() != root.path()
                    && link.path().ends_with('/')
                {
                    subpages.push(link);
                }
            }
            if found >= 4 {
                break;
            }
            // At most two same-origin subdirectories; newest paths first.
            subpages.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            subpages.dedup();
            pages.extend(subpages.into_iter().rev().take(2));
        }
        if found == 0 {
            tracing::debug!(source, "no public v1 sample discovered");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    true
}

const MAX_HEALTH_BODY: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
enum HttpHealthReply {
    Accepted { interval: u64, min_interval: u64 },
    Rejected,
    Incomplete,
    Invalid,
}

fn valid_expanded_peer(value: &Value) -> bool {
    let Value::Dict(peer) = value else {
        return false;
    };
    let Some(Value::Bytes(raw_ip)) = peer.get(b"ip".as_slice()) else {
        return false;
    };
    let Ok(host) = std::str::from_utf8(raw_ip) else {
        return false;
    };
    if host.is_empty()
        || host.len() > 253
        || host.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return false;
    }
    if host.parse::<IpAddr>().is_err() && url::Host::parse(host).is_err() {
        return false;
    }
    if !matches!(peer.get(b"port".as_slice()), Some(Value::Int(port)) if (1..=65535).contains(port))
    {
        return false;
    }
    match peer.get(b"peer id".as_slice()) {
        None => true,
        Some(Value::Bytes(id)) => id.len() == 20,
        _ => false,
    }
}

fn classify_http_health(data: &[u8]) -> HttpHealthReply {
    if data.len() > MAX_HEALTH_BODY {
        return HttpHealthReply::Invalid;
    }
    let Ok(Value::Dict(dict)) = decode(data) else {
        return HttpHealthReply::Invalid;
    };
    if let Some(reason) = dict.get(b"failure reason".as_slice()) {
        return if matches!(reason, Value::Bytes(_)) {
            HttpHealthReply::Rejected
        } else {
            HttpHealthReply::Invalid
        };
    }
    let Some(Value::Int(interval)) = dict.get(b"interval".as_slice()) else {
        return HttpHealthReply::Invalid;
    };
    if *interval <= 0 {
        return HttpHealthReply::Invalid;
    }
    if let Some(peers6) = dict.get(b"peers6".as_slice())
        && !matches!(peers6, Value::Bytes(bytes) if bytes.len().is_multiple_of(18))
    {
        return HttpHealthReply::Invalid;
    }
    match dict.get(b"peers".as_slice()) {
        None => return HttpHealthReply::Incomplete,
        Some(Value::Bytes(bytes)) if bytes.len().is_multiple_of(6) => (),
        Some(Value::List(peers)) if peers.iter().all(valid_expanded_peer) => (),
        _ => return HttpHealthReply::Invalid,
    }
    let min_interval = match dict.get(b"min interval".as_slice()) {
        Some(Value::Int(value)) if *value > 0 => *value as u64,
        _ => 0,
    };
    HttpHealthReply::Accepted {
        interval: *interval as u64,
        min_interval,
    }
}

/// Apply exactly the classification used for persisted health observations.
fn apply_http_health(data: &[u8], obs: &mut Observation) -> Option<u64> {
    match classify_http_health(data) {
        HttpHealthReply::Accepted {
            interval,
            min_interval,
        } => {
            obs.result = "accepted".into();
            obs.protocol_reply = true;
            Some(interval.max(min_interval))
        }
        HttpHealthReply::Rejected => {
            obs.result = "unknown".into();
            obs.error_category = Some("tracker_rejection".into());
            obs.protocol_reply = true;
            None
        }
        HttpHealthReply::Incomplete => {
            // numwant=0 trackers omitting peers are tolerated as unknown,
            // not credited as a complete announce or penalized as offline.
            obs.result = "unknown".into();
            obs.error_category = Some("incomplete_protocol".into());
            obs.protocol_reply = false;
            None
        }
        HttpHealthReply::Invalid => {
            obs.result = "failed".into();
            obs.error_category = Some("invalid_protocol".into());
            obs.protocol_reply = false;
            None
        }
    }
}

async fn probe_http(id: &str, url: &Url, ipv6: bool) -> (Observation, u64, u64, Option<IpAddr>) {
    let started = Instant::now();
    let mut defer_secs = 1800u64;
    let mut host_backoff = 0u64;
    let mut actual_ip = None;
    let mut classified_at = None;
    let mut obs = Observation {
        observed_at: unix_now(),
        endpoint_id: id.into(),
        family: if ipv6 { "ipv6" } else { "ipv4" }.into(),
        stage: "announce".into(),
        result: "unknown".into(),
        ..Default::default()
    };
    let result: Result<()> = async {
        let host=url.host_str().context("host")?; let port=url.port_or_known_default().unwrap_or(80);
        if !policy_allows(host,url.scheme(),port).await? { obs.error_category=Some("operator_denied".into()); return Ok(()); }
        let Some((client,address))=client_for(url,Some(ipv6)).await? else { obs.error_category=Some("family_unavailable".into()); return Ok(()); };
        actual_ip=Some(address.ip());
        let digest=Sha1::digest(url.as_str().as_bytes());
        let hash = digest.iter().map(|b|format!("%{b:02X}")).collect::<String>();
        let peer_id=if ipv6 {"-TO1000-abcdefghijk6"} else {"-TO1000-abcdefghijk4"};
        let target=format!("{}?info_hash={}&peer_id={}&port=6881&uploaded=0&downloaded=0&left=1&compact=1&numwant=0",url,hash,peer_id);
        let mut response=match client.get(&target).send().await { Ok(v)=>v,Err(e)=>{ if local_network_error(&e) {obs.error_category=Some("local_unavailable".into());} else if e.is_timeout() {obs.result="failed".into();obs.error_category=Some("timeout".into());} else if e.is_connect() {obs.result="failed".into();obs.error_category=Some("transport".into());} else {obs.error_category=Some("local_or_network_unknown".into());} return Ok(()); } };
        let status=response.status().as_u16(); obs.http_status=Some(status);
        if let Some(value)=response.headers().get(reqwest::header::RETRY_AFTER).and_then(|x|x.to_str().ok()) {host_backoff=retry_delay(Some(value),std::time::SystemTime::now());defer_secs=defer_secs.max(host_backoff);}
        if status==429 || status==503 || status==403 {
            defer_secs=retry_delay(response.headers().get(reqwest::header::RETRY_AFTER).and_then(|x|x.to_str().ok()),std::time::SystemTime::now());
            host_backoff=defer_secs;
            obs.error_category=Some(if status==429 { "rate_limited" } else { "http_error" }.into());
            if status!=429 { obs.result="failed".into(); }
            return Ok(());
        }
        if !(200..300).contains(&status) { obs.result="failed".into(); obs.error_category=Some("http_error".into()); return Ok(()); }
        let mut data=Vec::new(); while let Some(chunk)=response.chunk().await? { anyhow::ensure!(data.len()+chunk.len()<=MAX_HEALTH_BODY,"response too large");data.extend_from_slice(&chunk); }
        if let Some(interval) = apply_http_health(&data, &mut obs) {
            defer_secs=defer_secs.max(interval);
            classified_at=Some(unix_now());
            obs.latency_ms=Some(started.elapsed().as_secs_f64()*1000.0);
            // Best-effort stopped for this exact synthetic identity.
            if host_backoff==0 { let _ = client.get(format!("{target}&event=stopped")).send().await; }
        }
        Ok(())
    }.await;
    if result.is_err() {
        obs.error_category = Some("local_or_network_unknown".into());
    }
    obs.observed_at = classified_at.unwrap_or_else(unix_now);
    if obs.latency_ms.is_none() {
        obs.latency_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
    }
    (obs, defer_secs, host_backoff, actual_ip)
}

async fn probe_udp(id: &str, url: &Url, ipv6: bool) -> (Observation, u64, u64, Option<IpAddr>) {
    use bt_protocol::{
        AnnounceEvent, AnnounceRequest, decode_udp_announce_response, decode_udp_connect_response,
        encode_udp_announce_request, encode_udp_connect_request,
    };
    let started = Instant::now();
    let mut obs = Observation {
        observed_at: unix_now(),
        endpoint_id: id.into(),
        family: if ipv6 { "ipv6" } else { "ipv4" }.into(),
        stage: "announce".into(),
        result: "unknown".into(),
        ..Default::default()
    };
    let mut defer = 1800u64;
    let mut classified_at = None;
    let mut actual_ip = None;
    let mut sent = false;
    let result: Result<()> = async {
        let host = url.host_str().context("missing host")?;
        let port = url.port().unwrap_or(80);
        if !policy_allows(host, "udp", port).await? {
            obs.error_category = Some("operator_denied".into());
            return Ok(());
        }
        let Some(address) = pinned(host, port, Some(ipv6)).await?.first().copied() else {
            obs.error_category = Some("family_unavailable".into());
            return Ok(());
        };
        actual_ip = Some(address.ip());
        let sock = tokio::net::UdpSocket::bind(if ipv6 { "[::]:0" } else { "0.0.0.0:0" }).await?;
        sock.connect(address).await?;
        let mut data = [0u8; 65507];
        let tx = rand::random::<u32>();
        sock.send(&encode_udp_connect_request(tx)).await?;
        sent = true;
        let n = tokio::time::timeout(Duration::from_secs(5), sock.recv(&mut data)).await??;
        let conn = decode_udp_connect_response(&data[..n], tx)?;
        obs.protocol_reply = true;
        let tx2 = rand::random::<u32>();
        let request = AnnounceRequest {
            info_hash: Sha1::digest(url.as_str().as_bytes()).into(),
            peer_id: if ipv6 {
                *b"-TO1000-abcdefghijk6"
            } else {
                *b"-TO1000-abcdefghijk4"
            },
            ip: sock.local_addr()?.ip(),
            port: 6881,
            uploaded: 0,
            downloaded: 0,
            left: 1,
            event: AnnounceEvent::None,
            numwant: 0,
        };
        sock.send(&encode_udp_announce_request(conn, tx2, &request))
            .await?;
        let n = tokio::time::timeout(Duration::from_secs(5), sock.recv(&mut data)).await??;
        if n >= 8
            && u32::from_be_bytes(data[0..4].try_into()?) == 3
            && u32::from_be_bytes(data[4..8].try_into()?) == tx2
        {
            obs.error_category = Some("tracker_rejection".into());
            return Ok(());
        }
        let response = decode_udp_announce_response(&data[..n], tx2, ipv6)?;
        defer = defer.max(response.interval as u64);
        obs.result = "accepted".into();
        classified_at = Some(unix_now());
        obs.latency_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
        let mut stop = request;
        stop.event = AnnounceEvent::Stopped;
        let _ = sock
            .send(&encode_udp_announce_request(conn, rand::random(), &stop))
            .await;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        if sent
            && error
                .downcast_ref::<tokio::time::error::Elapsed>()
                .is_some()
        {
            obs.result = "failed".into();
            obs.error_category = Some("timeout".into());
        } else {
            obs.error_category = Some("local_or_network_unknown".into());
        }
    }
    obs.observed_at = classified_at.unwrap_or_else(unix_now);
    if obs.latency_ms.is_none() {
        obs.latency_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
    }
    (obs, defer, 0, actual_ip)
}

fn compact_peers(data: &[u8], v6: bool) -> Vec<SocketAddr> {
    let width = if v6 { 18 } else { 6 };
    if !data.len().is_multiple_of(width) {
        return Vec::new();
    }
    data.chunks_exact(width)
        .filter_map(|part| {
            let addr = if v6 {
                IpAddr::V6(std::net::Ipv6Addr::from(
                    <[u8; 16]>::try_from(&part[..16]).ok()?,
                ))
            } else {
                IpAddr::V4(std::net::Ipv4Addr::new(part[0], part[1], part[2], part[3]))
            };
            let port = u16::from_be_bytes([part[width - 2], part[width - 1]]);
            (port > 0 && public_ip(addr)).then_some(SocketAddr::new(addr, port))
        })
        .take(32)
        .collect()
}

#[derive(Debug)]
struct ActivityHttpReply {
    interval: u64,
    peers: Vec<SocketAddr>,
    has_reported_peers: bool,
}

// Validate with the same policy as health; extraction deliberately never
// resolves a peer hostname or connects to a peer.
fn activity_http_body(body: &[u8]) -> std::result::Result<ActivityHttpReply, HttpHealthReply> {
    let interval = match classify_http_health(body) {
        HttpHealthReply::Accepted {
            interval,
            min_interval,
        } => interval.max(min_interval),
        other => return Err(other),
    };
    let Ok(Value::Dict(dict)) = decode(body) else {
        return Err(HttpHealthReply::Invalid);
    };
    let mut peers = Vec::new();
    let mut has_reported_peers = false;
    for (field, v6) in [(b"peers".as_slice(), false), (b"peers6".as_slice(), true)] {
        match dict.get(field) {
            Some(Value::Bytes(raw)) => {
                has_reported_peers |= !raw.is_empty();
                peers.extend(compact_peers(raw, v6));
            }
            Some(Value::List(rows)) if !v6 => {
                has_reported_peers |= !rows.is_empty();
                for row in rows.iter().take(32) {
                    if let Value::Dict(peer) = row
                        && let (Some(Value::Bytes(ip)), Some(Value::Int(port))) =
                            (peer.get(b"ip".as_slice()), peer.get(b"port".as_slice()))
                        && let Ok(ip) = std::str::from_utf8(ip).unwrap_or("").parse::<IpAddr>()
                        && (1..=65535).contains(port)
                        && public_ip(ip)
                    {
                        peers.push(SocketAddr::new(ip, *port as u16));
                    }
                }
            }
            None => (),
            // classify_http_health already checked the type and lengths.
            _ => return Err(HttpHealthReply::Invalid),
        }
    }
    peers.truncate(32);
    Ok(ActivityHttpReply {
        interval,
        peers,
        has_reported_peers,
    })
}

#[derive(Debug)]
struct ActivityFault {
    category: &'static str,
    result: &'static str,
}
impl std::fmt::Display for ActivityFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.category)
    }
}
impl std::error::Error for ActivityFault {}
fn activity_fault(category: &'static str, result: &'static str) -> anyhow::Error {
    ActivityFault { category, result }.into()
}
fn apply_activity_http(body: &[u8], announce: &mut Observation) -> Result<ActivityHttpReply> {
    match activity_http_body(body) {
        Ok(parsed) => {
            announce.protocol_reply = true;
            Ok(parsed)
        }
        Err(HttpHealthReply::Rejected) => {
            announce.protocol_reply = true;
            Err(activity_fault("tracker_rejection", "unknown"))
        }
        Err(HttpHealthReply::Incomplete) => Err(activity_fault("incomplete_protocol", "unknown")),
        Err(_) => Err(activity_fault("invalid_protocol", "failed")),
    }
}
fn activity_success(announce: &mut Observation, peers: &[SocketAddr], has_reported_peers: bool) {
    if peers.is_empty() && has_reported_peers {
        announce.error_category = Some("peers_unusable".into());
    }
    announce.result = if peers.is_empty() && !has_reported_peers {
        "empty"
    } else {
        "accepted"
    }
    .into();
}
fn activity_http_status(status: u16) -> Option<(&'static str, &'static str)> {
    match status {
        429 => Some(("rate_limited", "unknown")),
        200..=299 => None,
        _ => Some(("http_error", "failed")),
    }
}
struct ActivityOutcome {
    delay: u64,
    backoff: bool,
    peers: Vec<SocketAddr>,
    bitmaps: usize,
    coverage: Option<f64>,
    stages: Vec<Observation>,
}
fn activity_event(id: &str, family: bool, stage: &str) -> Observation {
    Observation {
        endpoint_id: id.into(),
        family: if family { "ipv6" } else { "ipv4" }.into(),
        stage: stage.into(),
        observed_at: unix_now(),
        result: "unknown".into(),
        ..Default::default()
    }
}
async fn observe_sample(
    id: &str,
    url: &Url,
    sample: &crate::samples::Sample,
    ipv6: bool,
    gate: &PressureGate,
) -> ActivityOutcome {
    use bt_protocol::{
        AnnounceEvent, AnnounceRequest, decode_udp_announce_response, decode_udp_connect_response,
        encode_http_announce_query, encode_udp_announce_request, encode_udp_connect_request,
    };
    let mut announce = activity_event(id, ipv6, "sample_announce");
    let started = Instant::now();
    let mut delay = 1800u64;
    let mut backoff = false;
    let run = async {
        let host = url
            .host_str()
            .ok_or_else(|| activity_fault("local_unavailable", "unknown"))?;
        let port = url.port_or_known_default().unwrap_or(80);
        let policy = policy_allows(host, url.scheme(), port)
            .await
            .map_err(|_| activity_fault("local_unavailable", "unknown"))?;
        if !policy {
            return Err(activity_fault("operator_denied", "unknown"));
        }
        let mut request = AnnounceRequest {
            info_hash: sample.info_hash,
            peer_id: *b"-TO1000-abcdefghijkl",
            ip: if ipv6 {
                "::".parse().unwrap()
            } else {
                "0.0.0.0".parse().unwrap()
            },
            port: 6881,
            uploaded: 0,
            downloaded: 0,
            left: 1,
            event: AnnounceEvent::None,
            numwant: 20,
        };
        if url.scheme() == "udp" {
            let address = pinned(host, port, Some(ipv6))
                .await
                .map_err(|_| activity_fault("local_unavailable", "unknown"))?
                .first()
                .copied()
                .ok_or_else(|| activity_fault("family_unavailable", "unknown"))?;
            let socket = tokio::net::UdpSocket::bind(if ipv6 { "[::]:0" } else { "0.0.0.0:0" })
                .await
                .map_err(|_| activity_fault("local_unavailable", "unknown"))?;
            socket
                .connect(address)
                .await
                .map_err(|_| activity_fault("local_unavailable", "unknown"))?;
            let mut buf = [0u8; 65507];
            let tx = rand::random();
            socket
                .send(&encode_udp_connect_request(tx))
                .await
                .map_err(|_| activity_fault("transport", "failed"))?;
            let n = tokio::time::timeout(Duration::from_secs(5), socket.recv(&mut buf))
                .await
                .map_err(|_| activity_fault("timeout", "failed"))?
                .map_err(|_| activity_fault("transport", "failed"))?;
            let conn = decode_udp_connect_response(&buf[..n], tx)
                .map_err(|_| activity_fault("invalid_protocol", "failed"))?;
            announce.protocol_reply = true;
            let tx = rand::random();
            socket
                .send(&encode_udp_announce_request(conn, tx, &request))
                .await
                .map_err(|_| activity_fault("transport", "failed"))?;
            let n = tokio::time::timeout(Duration::from_secs(5), socket.recv(&mut buf))
                .await
                .map_err(|_| activity_fault("timeout", "failed"))?
                .map_err(|_| activity_fault("transport", "failed"))?;
            if n >= 8
                && u32::from_be_bytes(buf[..4].try_into().unwrap()) == 3
                && u32::from_be_bytes(buf[4..8].try_into().unwrap()) == tx
            {
                return Err(activity_fault("tracker_rejection", "unknown"));
            }
            let response = decode_udp_announce_response(&buf[..n], tx, ipv6)
                .map_err(|_| activity_fault("invalid_protocol", "failed"))?;
            delay = u64::from(response.interval).max(1800);
            announce.observed_at = unix_now();
            announce.latency_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
            request.event = AnnounceEvent::Stopped;
            let _ = socket
                .send(&encode_udp_announce_request(conn, rand::random(), &request))
                .await;
            let has_reported_peers = !response.peers.is_empty();
            Ok((
                response
                    .peers
                    .into_iter()
                    .filter(|p| p.port() > 0 && public_ip(p.ip()))
                    .take(32)
                    .collect::<Vec<_>>(),
                has_reported_peers,
            ))
        } else {
            let (client, _) = client_for(url, Some(ipv6))
                .await
                .map_err(|_| activity_fault("local_unavailable", "unknown"))?
                .ok_or_else(|| activity_fault("family_unavailable", "unknown"))?;
            let target = format!("{}?{}", url, encode_http_announce_query(&request));
            let mut response = client.get(&target).send().await.map_err(|e| {
                if local_network_error(&e) {
                    activity_fault("local_unavailable", "unknown")
                } else if e.is_timeout() {
                    activity_fault("timeout", "failed")
                } else if e.is_connect() {
                    activity_fault("transport", "failed")
                } else {
                    activity_fault("unknown", "unknown")
                }
            })?;
            let status = response.status().as_u16();
            announce.http_status = Some(status);
            let explicit = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|x| x.to_str().ok())
                .map(|s| retry_delay(Some(s), std::time::SystemTime::now()));
            if let Some(d) = explicit {
                delay = delay.max(d);
                backoff = true;
            }
            if matches!(status, 429 | 403 | 503) {
                delay = delay.max(explicit.unwrap_or(1800));
                backoff = true;
            }
            if let Some((category, result)) = activity_http_status(status) {
                return Err(activity_fault(category, result));
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|e| {
                if e.is_timeout() {
                    activity_fault("timeout", "failed")
                } else {
                    activity_fault("transport", "failed")
                }
            })? {
                if body.len() + chunk.len() > MAX_HEALTH_BODY {
                    return Err(activity_fault("invalid_protocol", "failed"));
                }
                body.extend_from_slice(&chunk);
            }
            let parsed = apply_activity_http(&body, &mut announce)?;
            announce.observed_at = unix_now();
            announce.latency_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
            delay = delay.max(parsed.interval);
            if !backoff {
                request.event = AnnounceEvent::Stopped;
                let _ = client
                    .get(format!("{}?{}", url, encode_http_announce_query(&request)))
                    .send()
                    .await;
            }
            Ok((parsed.peers, parsed.has_reported_peers))
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(20), run).await;
    let peers = match result {
        Ok(Ok((peers, has_reported_peers))) => {
            activity_success(&mut announce, &peers, has_reported_peers);
            peers
        }
        Ok(Err(error)) => {
            let fault = error.downcast_ref::<ActivityFault>();
            announce.result = fault.map(|f| f.result).unwrap_or("unknown").into();
            announce.error_category = Some(fault.map(|f| f.category).unwrap_or("unknown").into());
            Vec::new()
        }
        Err(_) => {
            announce.error_category = Some("budget_exhausted".into());
            Vec::new()
        }
    };
    if announce.latency_ms.is_none() {
        announce.observed_at = unix_now();
        announce.latency_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
    }
    let mut stages = vec![announce];
    let mut seen = std::collections::HashSet::new();
    let mut handshakes = Vec::new();
    let mut bitmaps = Vec::new();
    let mut attempted = [0usize; 2];
    let mut succeeded = [0usize; 2];
    let mut timed_out = [false; 2];
    for peer in peers.into_iter().filter(|p| seen.insert(*p)).take(8) {
        if !pressure_allows(gate).await {
            break;
        }
        let family = usize::from(peer.is_ipv6());
        attempted[family] += 1;
        match crate::peer::observe_peer(
            peer,
            sample.info_hash,
            *b"-TO1000-abcdefghijkl",
            sample.piece_count,
            Duration::from_secs(3),
        )
        .await
        {
            Ok(e) => {
                if e.handshake {
                    succeeded[family] += 1;
                    handshakes.push(peer);
                }
                if let Some(bits) = e.bitmap {
                    bitmaps.push(bits);
                }
            }
            Err(e) => {
                if e.downcast_ref::<tokio::time::error::Elapsed>().is_some() {
                    timed_out[family] = true;
                }
            }
        }
    }
    for family in 0..2 {
        if attempted[family] > 0 {
            let mut handshake = activity_event(id, family == 1, "handshake");
            handshake.result = if succeeded[family] == 0 {
                "failed"
            } else {
                "accepted"
            }
            .into();
            if succeeded[family] == 0 {
                handshake.error_category = Some(
                    if timed_out[family] {
                        "timeout"
                    } else {
                        "handshake_failed"
                    }
                    .into(),
                );
            }
            stages.push(handshake);
        }
    }
    let coverage = crate::peer::union_coverage(
        &bitmaps,
        sample.piece_count,
        sample.piece_length,
        sample.total_length,
    );
    ActivityOutcome {
        delay,
        backoff,
        peers: handshakes,
        bitmaps: bitmaps.len(),
        coverage,
        stages,
    }
}

async fn discover(config: &Config, store: &mut Store, gate: &PressureGate) -> Result<()> {
    if !pressure_allows(gate).await {
        return Ok(());
    }
    let now = unix_now();
    if store
        .value("sources_checked_at")?
        .is_none_or(|then| now - then >= 6 * 3600)
        && let Ok(text) = std::fs::read_to_string(&config.settings.sources_file)
    {
        store.set_value("sources_checked_at", now)?;
        let sources: Sources = toml::from_str(&text)?;
        let mut urls = sources.seed_endpoints.clone();
        store.ingest(&urls, &config.settings.service_aliases)?;
        for s in &sources.tracker_lists {
            if !pressure_allows(gate).await {
                store.set_value("sources_checked_at", 0)?;
                return Ok(());
            }
            let host = Url::parse(s)
                .ok()
                .and_then(|u| u.host_str().map(|h| format!("source:{h}")))
                .unwrap_or_default();
            if store.paused(&host, now)? {
                continue;
            }
            match source(s).await {
                Ok((items, delay)) => {
                    urls.extend(items);
                    if let Some(seconds) = delay {
                        store.pause(&host, deadline(unix_now(), seconds))?;
                    }
                }
                Err(e) => tracing::warn!(%e,"source deferred"),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        urls.sort();
        urls.dedup();
        urls.truncate(20_000);
        if !urls.is_empty() {
            store.ingest(&urls, &config.settings.service_aliases)?;
        }
        let source_cursor = store
            .value("official_source_cursor")?
            .unwrap_or(0)
            .rem_euclid(4) as usize;
        store.set_value("official_source_cursor", ((source_cursor + 1) % 4) as i64)?;
        if !collect_samples(store, source_cursor, gate).await {
            store.set_value("sources_checked_at", 0)?;
        }
    }
    Ok(())
}

async fn health_task(
    id: String,
    url: Url,
    families: Vec<bool>,
    gate: PressureGate,
) -> (
    String,
    Vec<(Observation, u64)>,
    u64,
    Option<tracker_model::Infrastructure>,
) {
    let host = url.host_str().unwrap_or_default().to_string();
    let mut results = Vec::new();
    let mut backoff = 5;
    let mut infrastructure = None;
    for family in families {
        let (obs, defer, host_delay, ip) = if url.scheme() == "udp" {
            probe_udp(&id, &url, family).await
        } else {
            probe_http(&id, &url, family).await
        };
        if infrastructure.is_none()
            && (obs.protocol_reply || obs.http_status.is_some())
            && let Some(ip) = ip
            && pressure_allows(&gate).await
        {
            infrastructure = Some(crate::infrastructure::lookup(ip).await);
        }
        let limited = host_delay > 0;
        if limited {
            backoff = host_delay;
        }
        results.push((obs, defer));
        if limited {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    (host, results, backoff, infrastructure)
}

async fn health_batch(
    config: &Config,
    store: &mut Store,
    batch: &[(String, Url, Vec<bool>)],
    gate: &PressureGate,
) -> Result<()> {
    let mut tasks = tokio::task::JoinSet::new();
    for (id, url, families) in batch {
        let host = url.host_str().unwrap_or_default();
        if store.paused(host, unix_now())? {
            continue;
        }
        store.pause(host, deadline(unix_now(), 5))?;
        for family in families {
            store.defer_announce(
                id,
                if *family { "ipv6" } else { "ipv4" },
                "health",
                deadline(unix_now(), config.settings.interval_seconds),
            )?;
        }
        tasks.spawn(health_task(
            id.clone(),
            url.clone(),
            families.clone(),
            gate.clone(),
        ));
    }
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    tick.tick().await;
    let mut settled = 0usize;
    while !tasks.is_empty() {
        tokio::select! {
            result=tasks.join_next()=>if let Some(Ok((host,events,backoff,infrastructure)))=result {
                if let Some(infra)=infrastructure {store.save_infrastructure(&host,&infra)?;}
                for (event,defer) in events {store.defer_announce(&event.endpoint_id,&event.family,"health",deadline(unix_now(),defer.max(config.settings.interval_seconds)))?;store.record(&event)?;}
                store.pause(&host,deadline(unix_now(),backoff))?;settled+=1;
            },
            _=tick.tick()=>{publish(config,store).await?;settled=0;},
        }
        if settled >= 10 {
            publish(config, store).await?;
            settled = 0;
        }
    }
    Ok(())
}

type Activity = (String, Url, String, crate::samples::Sample, String, bool);
fn choose_due_family(prefer_v6: bool, ipv4_due: bool, ipv6_due: bool) -> Option<bool> {
    match (ipv4_due, ipv6_due) {
        (false, false) => None,
        (true, false) => Some(false),
        (false, true) => Some(true),
        (true, true) => Some(prefer_v6),
    }
}
fn health_candidates(store: &Store, now: i64) -> Result<Vec<(String, Url, Vec<bool>)>> {
    let mut hosts = std::collections::HashSet::new();
    let mut health = Vec::new();
    for (id, raw, _) in store.due_targets(now)? {
        let url = Url::parse(&raw)?;
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if store.paused(&host, now)? || hosts.contains(&host) {
            continue;
        }
        let mut families = Vec::new();
        for family in [false, true] {
            if store.announce_allowed(&id, if family { "ipv6" } else { "ipv4" }, "health", now)? {
                families.push(family);
            }
        }
        if !families.is_empty() {
            hosts.insert(host);
            health.push((id, url, families));
            if health.len() >= 256 {
                break;
            }
        }
    }
    Ok(health)
}
async fn activity_batch(
    config: &Config,
    store: &mut Store,
    batch: &[Activity],
    gate: &PressureGate,
) -> Result<()> {
    let mut tasks = tokio::task::JoinSet::new();
    for (id, url, source, item, resource, family) in batch {
        if !pressure_allows(gate).await {
            break;
        }
        if store.paused(url.host_str().unwrap_or_default(), unix_now())? {
            continue;
        }
        let id = id.clone();
        let url = url.clone();
        let source = source.clone();
        let item = item.clone();
        let resource = resource.clone();
        let family = *family;
        let gate = gate.clone();
        store.mark_activity(&id, unix_now())?;
        store.pause(url.host_str().unwrap_or_default(), deadline(unix_now(), 5))?;
        store.defer_announce(
            &id,
            if family { "ipv6" } else { "ipv4" },
            &resource,
            deadline(unix_now(), config.settings.interval_seconds),
        )?;
        tasks.spawn(async move {
            let outcome = tokio::time::timeout(
                Duration::from_secs(50),
                observe_sample(&id, &url, &item, family, &gate),
            )
            .await;
            (id, url, source, item, resource, family, outcome)
        });
    }
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    tick.tick().await;
    while !tasks.is_empty() {
        tokio::select! {
            result=tasks.join_next()=>if let Some(Ok((id,url,source,item,resource,family,outcome)))=result {
                let report=match outcome {Ok(v)=>v,Err(_)=>{let mut event=activity_event(&id,family,"sample_announce");event.error_category=Some("budget_exhausted".into());ActivityOutcome{delay:1800,backoff:false,peers:Vec::new(),bitmaps:0,coverage:None,stages:vec![event]}}};
                store.sample_evidence(crate::store::ActivityEvidence {endpoint:&id,source:&source,sample:&item,peers:&report.peers,bitmaps:report.bitmaps as u64,coverage:report.coverage,stages:&report.stages})?;
                store.defer_announce(&id,if family {"ipv6"}else{"ipv4"},&resource,deadline(unix_now(),report.delay.max(config.settings.interval_seconds)))?;
                if report.backoff {store.pause(url.host_str().unwrap_or_default(),deadline(unix_now(),report.delay))?;}
            },
            _=tick.tick()=>{publish(config,store).await?;},
        }
    }
    Ok(())
}

async fn publish(config: &Config, store: &mut Store) -> Result<()> {
    let _ = crate::stats::collect(store).await;
    store.publish(config.settings.interval_seconds, &config.settings.own_hosts)
}

pub async fn cycle(config: &Config, store: &mut Store) -> Result<()> {
    let gate: PressureGate = std::sync::Arc::new(tokio::sync::RwLock::new(Pressure::Unknown));
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(200))
        .build();
    let _monitor = if let Ok(client) = client {
        refresh_pressure(&client, &gate).await;
        let monitor_gate = gate.clone();
        Some(StopMonitor(tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(14)).await;
                refresh_pressure(&client, &monitor_gate).await;
            }
        })))
    } else {
        None
    };
    if tokio::time::timeout(Duration::from_secs(60), discover(config, store, &gate))
        .await
        .is_err()
        && !pressure_allows(&gate).await
    {
        store.set_value("sources_checked_at", 0)?;
    }
    let now = unix_now();
    let daily_due = store
        .value("pool_updated_at")?
        .is_none_or(|at| now.saturating_sub(at) >= 86400);
    let retry_due = store
        .value("pool_last_attempt")?
        .is_none_or(|at| now.saturating_sub(at) >= 1800);
    let mut pool_task = if daily_due && retry_due && pressure_allows(&gate).await {
        let previous = store.previous_pool()?;
        let source_settings = std::fs::read_to_string(&config.settings.sources_file)
            .ok()
            .and_then(|text| toml::from_str::<Sources>(&text).ok());
        let forum_api_origin = source_settings
            .as_ref()
            .and_then(|sources| sources.forum_api_origin.clone());
        let navigation = source_settings
            .map(|sources| sources.forum_navigation_roots)
            .unwrap_or_default();
        store.set_value("pool_last_attempt", now)?;
        Some(AbortTask(tokio::spawn(async move {
            crate::pool::collect_daily(&previous, now, forum_api_origin.as_deref(), &navigation)
                .await
        })))
    } else {
        None
    };
    store.finish_baseline()?;
    publish(config, store).await?;
    let mut health = health_candidates(store, unix_now())?;
    // Build independent oldest-first lists before launching either lane. Each
    // wave has at most four network tasks, and the lanes alternate per wave.
    let mut activity = Vec::new();
    let mut activity_hosts = std::collections::HashSet::new();
    for (id, raw) in store.activity_targets()? {
        let url = Url::parse(&raw)?;
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if store.paused(&host, unix_now())? || activity_hosts.contains(&host) {
            continue;
        }
        let Some((source, item)) = store.next_sample(&id)? else {
            continue;
        };
        let resource = hex::encode(item.info_hash);
        let now = unix_now();
        let ipv4_due = store.announce_allowed(&id, "ipv4", &resource, now)?;
        let ipv6_due = store.announce_allowed(&id, "ipv6", &resource, now)?;
        let chosen = choose_due_family(rand::random(), ipv4_due, ipv6_due);
        let Some(family) = chosen else {
            store.mark_activity(&id, unix_now())?;
            continue;
        };
        activity_hosts.insert(host);
        activity.push((id, url, source, item, resource, family));
        if activity.len() >= 256 {
            break;
        }
    }
    let mut hi = 0usize;
    let mut ai = 0usize;
    let mut refreshed = Instant::now();
    while hi < health.len() || ai < activity.len() || pool_task.is_some() {
        if pool_task.as_ref().is_some_and(|task| task.0.is_finished())
            && let Some(mut task) = pool_task.take()
            && let Ok(Ok(update)) = (&mut task.0).await
        {
            store.replace_pool(update)?;
        }
        if pool_task.is_some() && !pressure_allows(&gate).await {
            drop(pool_task.take());
        }
        // The collector counts as one of four external network tasks.
        let width = if pool_task.is_some() { 3 } else { 4 };
        if hi < health.len() {
            let end = (hi + width).min(health.len());
            health_batch(config, store, &health[hi..end], &gate).await?;
            hi = end;
        }
        if ai < activity.len() && pressure_allows(&gate).await {
            let end = (ai + width).min(activity.len());
            activity_batch(config, store, &activity[ai..end], &gate).await?;
            ai = end;
        }
        publish(config, store).await?;
        // Activities may run for minutes; re-read newly due health work rather
        // than waiting for the complete activity backlog to drain.
        if refreshed.elapsed() >= Duration::from_secs(30) {
            health = health_candidates(store, unix_now())?;
            hi = 0;
            refreshed = Instant::now();
        }
        if hi >= health.len() && ai < activity.len() && !pressure_allows(&gate).await {
            break;
        }
        if hi >= health.len() && ai >= activity.len() && pool_task.is_some() {
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    }
    publish(config, store).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activity_family_selection_never_bypasses_due_and_can_pick_both() {
        assert_eq!(choose_due_family(false, true, true), Some(false));
        assert_eq!(choose_due_family(true, true, true), Some(true));
        assert_eq!(choose_due_family(true, true, false), Some(false));
        assert_eq!(choose_due_family(false, false, true), Some(true));
        assert_eq!(choose_due_family(false, false, false), None);
    }

    fn health_dict(entries: Vec<(&str, Value)>) -> Value {
        Value::Dict(
            entries
                .into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    }

    fn health_body(peers: Value) -> Vec<u8> {
        bt_protocol::bencode::encode(&health_dict(vec![
            ("interval", Value::Int(1800)),
            ("peers", peers),
        ]))
    }

    fn peer(ip: &str, port: i64, id: Option<Value>) -> Value {
        let mut entries = vec![
            ("ip", Value::Bytes(ip.as_bytes().to_vec())),
            ("port", Value::Int(port)),
        ];
        if let Some(id) = id {
            entries.push(("peer id", id));
        }
        health_dict(entries)
    }

    #[test]
    fn sample_http_uses_health_classification_and_distinguishes_unusable_peers() {
        let cases = [
            (
                bt_protocol::bencode::encode(&health_dict(vec![("interval", Value::Int(1800))])),
                "incomplete_protocol",
                "unknown",
                false,
            ),
            (
                bt_protocol::bencode::encode(&health_dict(vec![
                    ("interval", Value::Int(1800)),
                    ("peers6", Value::Bytes(vec![])),
                ])),
                "incomplete_protocol",
                "unknown",
                false,
            ),
            (
                bt_protocol::bencode::encode(&health_dict(vec![(
                    "failure reason",
                    Value::Bytes(b"no".to_vec()),
                )])),
                "tracker_rejection",
                "unknown",
                true,
            ),
            (
                bt_protocol::bencode::encode(&health_dict(vec![
                    ("failure reason", Value::Int(1)),
                    ("interval", Value::Int(1800)),
                    ("peers", Value::Bytes(vec![])),
                ])),
                "invalid_protocol",
                "failed",
                false,
            ),
            (
                health_body(Value::List(vec![
                    peer("1.1.1.1", 6881, None),
                    peer("example.org", 0, None),
                ])),
                "invalid_protocol",
                "failed",
                false,
            ),
            (
                health_body(Value::Bytes(vec![0; 5])),
                "invalid_protocol",
                "failed",
                false,
            ),
            (
                bt_protocol::bencode::encode(&health_dict(vec![
                    ("interval", Value::Int(1800)),
                    ("peers", Value::Bytes(vec![])),
                    ("peers6", Value::Bytes(vec![0; 17])),
                ])),
                "invalid_protocol",
                "failed",
                false,
            ),
        ];
        for (body, category, result, protocol_reply) in cases {
            let mut obs = Observation::default();
            let err = apply_activity_http(&body, &mut obs).unwrap_err();
            let fault = err.downcast_ref::<ActivityFault>().unwrap();
            assert_eq!(
                (fault.category, fault.result, obs.protocol_reply),
                (category, result, protocol_reply)
            );
        }

        let success_cases = [
            (health_body(Value::Bytes(vec![])), false, 0, "empty", None),
            (health_body(Value::List(vec![])), false, 0, "empty", None),
            (
                health_body(Value::List(vec![peer("example.org", 6881, None)])),
                true,
                0,
                "accepted",
                Some("peers_unusable"),
            ),
            (
                health_body(Value::Bytes(vec![127, 0, 0, 1, 0x1a, 0xe1])),
                true,
                0,
                "accepted",
                Some("peers_unusable"),
            ),
            (
                health_body(Value::List(vec![peer(
                    "1.1.1.1",
                    6881,
                    Some(Value::Bytes(vec![0; 20])),
                )])),
                true,
                1,
                "accepted",
                None,
            ),
            (
                health_body(Value::Bytes(vec![1, 1, 1, 1, 0x1a, 0xe1])),
                true,
                1,
                "accepted",
                None,
            ),
            (
                bt_protocol::bencode::encode(&health_dict(vec![
                    ("interval", Value::Int(1800)),
                    ("peers", Value::Bytes(vec![])),
                    ("peers6", Value::Bytes(vec![0; 18])),
                ])),
                true,
                0,
                "accepted",
                Some("peers_unusable"),
            ),
        ];
        for (body, reported, count, result, category) in success_cases {
            let mut obs = Observation::default();
            let parsed = apply_activity_http(&body, &mut obs).unwrap();
            assert!(obs.protocol_reply);
            assert_eq!(parsed.has_reported_peers, reported);
            assert_eq!(parsed.peers.len(), count);
            activity_success(&mut obs, &parsed.peers, parsed.has_reported_peers);
            assert_eq!(obs.result, result);
            assert_eq!(obs.error_category.as_deref(), category);
        }
    }

    #[test]
    fn http_health_classifies_complete_rejected_incomplete_and_invalid() {
        let accepted = [
            health_body(Value::Bytes(vec![])),
            health_body(Value::Bytes(vec![0; 6])),
            health_body(Value::List(vec![])),
            health_body(Value::List(vec![
                peer("example.org", 6881, None),
                peer("2001:db8::1", 1, Some(Value::Bytes(vec![0; 20]))),
            ])),
            bt_protocol::bencode::encode(&health_dict(vec![
                ("interval", Value::Int(5)),
                ("min interval", Value::Int(9)),
                ("peers", Value::Bytes(vec![])),
                ("peers6", Value::Bytes(vec![0; 18])),
            ])),
        ];
        for body in accepted {
            let mut obs = Observation::default();
            assert!(apply_http_health(&body, &mut obs).is_some());
            assert_eq!(obs.result, "accepted");
            assert!(obs.protocol_reply);
        }
        let incomplete = [
            bt_protocol::bencode::encode(&health_dict(vec![("interval", Value::Int(1800))])),
            bt_protocol::bencode::encode(&health_dict(vec![
                ("interval", Value::Int(1800)),
                ("peers6", Value::Bytes(vec![])),
            ])),
        ];
        for body in incomplete {
            let mut obs = Observation::default();
            assert_eq!(apply_http_health(&body, &mut obs), None);
            assert_eq!(obs.result, "unknown");
            assert_eq!(obs.error_category.as_deref(), Some("incomplete_protocol"));
            assert!(!obs.protocol_reply);
        }
        let rejection = bt_protocol::bencode::encode(&health_dict(vec![(
            "failure reason",
            Value::Bytes(b"rejected".to_vec()),
        )]));
        let mut obs = Observation::default();
        assert_eq!(apply_http_health(&rejection, &mut obs), None);
        assert_eq!(obs.result, "unknown");
        assert_eq!(obs.error_category.as_deref(), Some("tracker_rejection"));
        assert!(obs.protocol_reply);

        let invalid = vec![
            health_body(Value::Bytes(vec![0; 5])),
            health_body(Value::Int(1)),
            health_body(Value::List(vec![Value::Int(1)])),
            health_body(Value::List(vec![peer("", 6881, None)])),
            health_body(Value::List(vec![peer("example.org", 0, None)])),
            health_body(Value::List(vec![peer("example.org", 65536, None)])),
            health_body(Value::List(vec![peer(
                "example.org",
                6881,
                Some(Value::Bytes(vec![0; 19])),
            )])),
            health_body(Value::List(vec![peer(
                "example.org",
                6881,
                Some(Value::Int(20)),
            )])),
            bt_protocol::bencode::encode(&health_dict(vec![
                ("interval", Value::Int(0)),
                ("peers", Value::Bytes(vec![])),
            ])),
            bt_protocol::bencode::encode(&health_dict(vec![
                ("interval", Value::Int(1800)),
                ("peers", Value::Bytes(vec![])),
                ("peers6", Value::Bytes(vec![0; 17])),
            ])),
            bt_protocol::bencode::encode(&health_dict(vec![
                ("interval", Value::Int(1800)),
                ("peers", Value::Bytes(vec![])),
                ("peers6", Value::Int(1)),
            ])),
            bt_protocol::bencode::encode(&health_dict(vec![
                ("failure reason", Value::Int(1)),
                ("interval", Value::Int(1800)),
                ("peers", Value::Bytes(vec![])),
            ])),
            vec![b'x'; MAX_HEALTH_BODY + 1],
        ];
        for body in invalid {
            let mut obs = Observation::default();
            assert_eq!(apply_http_health(&body, &mut obs), None);
            assert_eq!(obs.result, "failed");
            assert_eq!(obs.error_category.as_deref(), Some("invalid_protocol"));
            assert!(!obs.protocol_reply);
        }
    }

    #[test]
    fn only_complete_health_enters_all_and_handshakes_remain_required_for_standard() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-http-health-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let urls = [
            "https://good.example/announce",
            "https://incomplete.example/announce",
            "https://bad.example/announce",
        ];
        store.ingest(
            &urls.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &Default::default(),
        )?;
        for (id, url, _) in store.targets()? {
            let body = if url == urls[0] {
                health_body(Value::Bytes(vec![]))
            } else if url == urls[1] {
                bt_protocol::bencode::encode(&health_dict(vec![("interval", Value::Int(1800))]))
            } else {
                health_body(Value::Bytes(vec![0; 5]))
            };
            let repeats = if url == urls[0] { 5 } else { 1 };
            for n in 0..repeats {
                let mut obs = Observation {
                    endpoint_id: id.clone(),
                    family: if n == 4 { "ipv6" } else { "ipv4" }.into(),
                    stage: "announce".into(),
                    observed_at: unix_now(),
                    http_status: Some(200),
                    ..Default::default()
                };
                apply_http_health(&body, &mut obs);
                store.record(&obs)?;
            }
        }
        store.publish(1800, &[])?;
        let public = rusqlite::Connection::open(dir.join("public.sqlite3"))?;
        let payload: String = public.query_row(
            "SELECT payload FROM documents WHERE key='feed:trackers_all.txt'",
            [],
            |r| r.get(0),
        )?;
        let all: String = serde_json::from_str(&payload)?;
        assert!(all.contains(urls[0]));
        assert!(!all.contains(urls[1]));
        assert!(!all.contains(urls[2]));
        let payload: String = public.query_row(
            "SELECT payload FROM endpoints WHERE url=?1",
            [urls[0]],
            |r| r.get(0),
        )?;
        let endpoint: tracker_model::Endpoint = serde_json::from_str(&payload)?;
        assert_eq!(endpoint.evidence.successes_24h, 5);
        assert!(!endpoint.evidence.standard_eligible);
        drop(public);
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[tokio::test]
    async fn pressure_gates_noncritical_work_and_unknown_is_conservative() {
        assert_eq!(
            pressure_from_json(br#"{"pressure":{"active":false,"reason":"none"}}"#),
            Pressure::Clear
        );
        assert_eq!(
            pressure_from_json(br#"{"pressure":{"active":true,"reason":"memory_admission"}}"#),
            Pressure::Active
        );
        assert_eq!(
            pressure_from_json(br#"{"pressure":{"active":true,"reason":"http_concurrency"}}"#),
            Pressure::Active
        );
        assert_eq!(pressure_from_json(b"unavailable"), Pressure::Unknown);
        assert_eq!(
            pressure_from_json(br#"{"pressure":{"active":false}}"#),
            Pressure::Unknown
        );
        let gate: PressureGate = std::sync::Arc::new(tokio::sync::RwLock::new(Pressure::Unknown));
        assert!(!pressure_allows(&gate).await);
        *gate.write().await = Pressure::Active;
        assert!(!pressure_allows(&gate).await);
        *gate.write().await = Pressure::Clear;
        assert!(pressure_allows(&gate).await);
    }
    #[tokio::test]
    async fn paused_or_unavailable_pressure_never_adds_failure_evidence() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-pressure-gate-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        store.ingest(
            &["https://example.org/announce".into()],
            &Default::default(),
        )?;
        let id = store.targets()?[0].0.clone();
        let now = unix_now();
        store.record(&Observation {
            endpoint_id: id.clone(),
            family: "ipv4".into(),
            stage: "announce".into(),
            observed_at: now,
            result: "accepted".into(),
            protocol_reply: true,
            ..Default::default()
        })?;
        let sample = crate::samples::Sample {
            info_hash: [7; 20],
            piece_count: 1,
            piece_length: 10,
            total_length: 10,
        };
        let activity = vec![(
            id.clone(),
            Url::parse("https://example.org/announce")?,
            "test".into(),
            sample,
            "sample".into(),
            false,
        )];
        let config = Config {
            data_dir: dir.clone(),
            settings: Default::default(),
        };
        let gate: PressureGate = std::sync::Arc::new(tokio::sync::RwLock::new(Pressure::Unknown));
        activity_batch(&config, &mut store, &activity, &gate).await?;
        *gate.write().await = Pressure::Active;
        activity_batch(&config, &mut store, &activity, &gate).await?;
        assert_eq!(health_candidates(&store, now)?.len(), 1);
        store.publish(1800, &[])?;
        let public = rusqlite::Connection::open(dir.join("public.sqlite3"))?;
        let count: i64 = public.query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))?;
        assert_eq!(count, 1);
        let payload: String =
            public.query_row("SELECT payload FROM endpoints WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
        let endpoint: tracker_model::Endpoint = serde_json::from_str(&payload)?;
        assert_eq!(endpoint.evidence.checks_24h, 1);
        assert_eq!(endpoint.evidence.successes_24h, 1);
        drop(public);
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn public_address_guard() {
        assert!(!public_ip("127.0.0.1".parse().unwrap()));
        assert!(!public_ip("192.168.1.1".parse().unwrap()));
        assert!(!public_ip("::1".parse().unwrap()));
        assert!(public_ip("1.1.1.1".parse().unwrap()));
    }
    #[test]
    fn retry_after_seconds_and_dates() {
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(retry_delay(Some("7200"), now), 7200);
        let date = httpdate::fmt_http_date(now + Duration::from_secs(3600));
        assert_eq!(retry_delay(Some(&date), now), 3600);
        assert_eq!(retry_delay(Some("not a date"), now), 1800);
    }
    #[test]
    fn upstream_long_intervals_and_deadlines_are_never_shortened() -> Result<()> {
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let ten_days = 10 * 86400;
        assert_eq!(retry_delay(Some("0"), now), 1800);
        assert_eq!(retry_delay(Some("864000"), now), ten_days);
        let date = httpdate::fmt_http_date(now + Duration::from_secs(ten_days));
        assert_eq!(retry_delay(Some(&date), now), ten_days);
        assert_eq!(retry_delay(Some("18446744073709551616000"), now), u64::MAX);
        assert_eq!(deadline(1_700_000_000, 1800), 1_700_001_800);
        assert_eq!(deadline(1_700_000_000, ten_days), 1_700_864_000);
        assert_eq!(deadline(1_700_000_000, u64::MAX), i64::MAX);
        assert_eq!(deadline(i64::MAX - 1, 1800), i64::MAX);
        assert_eq!(deadline(-100, 1800), 1700);
        let body = bt_protocol::bencode::encode(&health_dict(vec![
            ("interval", Value::Int(i64::MAX)),
            ("min interval", Value::Int(ten_days as i64)),
            ("peers", Value::Bytes(vec![])),
        ]));
        let mut obs = Observation::default();
        assert_eq!(apply_http_health(&body, &mut obs), Some(i64::MAX as u64));
        assert_eq!(activity_http_body(&body).unwrap().interval, i64::MAX as u64);
        let body = bt_protocol::bencode::encode(&health_dict(vec![
            ("interval", Value::Int(1800)),
            ("min interval", Value::Int(ten_days as i64)),
            ("peers", Value::List(vec![])),
        ]));
        assert_eq!(activity_http_body(&body).unwrap().interval, ten_days);
        assert_eq!(
            apply_http_health(&body, &mut Observation::default()),
            Some(ten_days)
        );

        let dir =
            std::env::temp_dir().join(format!("tracker-long-deadline-{}", rand::random::<u64>()));
        let store = Store::open(&dir)?;
        store.pause("example.org", deadline(1_700_000_000, u64::MAX))?;
        store.defer_announce(
            "example",
            "ipv4",
            "health",
            deadline(1_700_000_000, u64::MAX),
        )?;
        assert!(store.paused("example.org", i64::MAX - 1)?);
        assert!(!store.announce_allowed("example", "ipv4", "health", i64::MAX - 1)?);
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn reject_private_and_truncated_compact_peers() {
        assert!(compact_peers(&[127, 0, 0, 1, 0x1a, 0xe1], false).is_empty());
        assert!(compact_peers(&[1, 2, 3], false).is_empty());
        assert_eq!(compact_peers(&[1, 1, 1, 1, 0x1a, 0xe1], false).len(), 1);
    }
    #[test]
    fn official_sources_allow_expected_routes_but_not_external_urls() {
        let arch = Url::parse("https://archlinux.org/download/").unwrap();
        assert!(
            official_sample_link("arch", &arch, "/releng/releases/2026.11.01/torrent/").is_some()
        );
        assert!(official_sample_link("arch", &arch, "https://example.org/a.torrent").is_none());
        assert!(
            official_sample_link("arch", &arch, "/releng/releases/latest/torrent/?secret=1")
                .is_none()
        );
        assert!(fedora_torrent_name(
            "Fedora-Workstation-Live-x86_64-45.torrent"
        ));
        assert!(!fedora_torrent_name("../other.torrent"));
    }
    #[test]
    fn newly_due_health_is_visible_during_activity_backlog() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-health-refresh-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        store.ingest(
            &["https://example.org/announce".into()],
            &Default::default(),
        )?;
        let id = store.targets()?[0].0.clone();
        store.defer_announce(&id, "ipv4", "health", 200)?;
        store.defer_announce(&id, "ipv6", "health", 200)?;
        assert!(health_candidates(&store, 199)?.is_empty());
        assert_eq!(health_candidates(&store, 200)?.len(), 1);
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn same_host_not_due_endpoint_does_not_hide_due_sibling() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-health-host-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        store.ingest(
            &[
                "https://example.org/announce".into(),
                "udp://example.org:6969/announce".into(),
            ],
            &Default::default(),
        )?;
        let ordered = store.targets()?;
        let not_due = ordered[0].0.clone();
        let expected = ordered[1].0.clone();
        store.defer_announce(&not_due, "ipv4", "health", 200)?;
        store.defer_announce(&not_due, "ipv6", "health", 200)?;
        let candidates = health_candidates(&store, 150)?;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, expected);
        assert_eq!(health_candidates(&store, 200)?.len(), 1);
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn activity_http_rate_limit_is_neutral_and_gateway_failure_is_not_empty() {
        assert_eq!(activity_http_status(429), Some(("rate_limited", "unknown")));
        assert_eq!(activity_http_status(502), Some(("http_error", "failed")));
        assert_eq!(activity_http_status(200), None);
    }
}
