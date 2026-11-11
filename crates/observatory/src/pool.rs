//! Independent, bounded daily candidate collection. All records are private.
//! The caller owns the transaction that commits the whole returned pool and
//! source state together; a failed collection must never partially replace it.
use anyhow::{Context, Result, ensure};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::{Instant, sleep, timeout};
use url::Url;

const MAX_POOL: usize = 100;
const MAX_REQUESTS: usize = 40;
const MAX_PAGE: usize = 2 * 1024 * 1024;
const MAX_TORRENT: usize = 4 * 1024 * 1024;
// Covers the bounded official fetches plus TPB and forum feeds while limiting
// the aggregate response-body volume of a complete collection cycle.
const MAX_COLLECTION_BYTES: usize = 128 * 1024 * 1024;
const AGENT: &str = "TrackerObservatory";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Source {
    Debian,
    Ubuntu,
    Fedora,
    Arch,
    Hjd,
    FbyLt1,
    Fby418,
    TpbRecent,
    TpbAll,
}

impl Source {
    pub fn public_label(self) -> &'static str {
        match self {
            Self::Debian => "Debian",
            Self::Ubuntu => "Ubuntu",
            Self::Fedora => "Fedora",
            Self::Arch => "Arch Linux",
            Self::TpbRecent | Self::TpbAll => "TPB",
            _ => "论坛",
        }
    }
    pub fn is_forum(self) -> bool {
        matches!(self, Self::Hjd | Self::FbyLt1 | Self::Fby418)
    }
    fn is_index(self) -> bool {
        matches!(self, Self::TpbRecent | Self::TpbAll)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ForumNavigationRoots {
    pub fby_lt1: Option<String>,
    pub fby_418: Option<String>,
}

fn parse_navigation_url(value: &str) -> Option<Url> {
    if value.len() > 512 || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path().contains('%')
        || !matches!(url.host(), Some(url::Host::Domain(_)))
    {
        return None;
    }
    Some(url)
}

fn navigation_allowed(roots: &HashMap<Source, Url>, source: Source, url: &Url) -> bool {
    let Some(base) = roots.get(&source) else {
        return false;
    };
    if url.scheme() != "https"
        || url.origin() != base.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path().contains('%')
    {
        return false;
    }
    url.path() == "/robots.txt"
        || url.path() == "/"
        || roots
            .values()
            .any(|other| other.origin() == base.origin() && other.path() == url.path())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SampleRecord {
    pub info_hash: [u8; 20],
    pub pool: PoolKind,
    pub source: Source,
    /// Curated source label; forum titles and source addresses are never used.
    pub name: String,
    pub discovered_at: i64,
    /// None means hash-only, and is not evidence that the torrent is public.
    pub geometry: Option<(usize, u64, u64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PoolKind {
    Official,
    Forum,
}

impl SampleRecord {
    pub fn public_confirmed(&self) -> bool {
        self.geometry.is_some()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoolStatus {
    pub source: Source,
    pub status: String,
    pub retry_at: i64,
    #[serde(default)]
    pub error_category: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct PreviousPool {
    pub samples: Vec<SampleRecord>,
    pub sources: Vec<PoolStatus>,
}

#[derive(Serialize, Deserialize)]
pub struct PoolUpdate {
    pub samples: Vec<SampleRecord>,
    pub sources: Vec<PoolStatus>,
    pub collected_at: i64,
}

const SOURCES: [Source; 9] = [
    Source::Debian,
    Source::Ubuntu,
    Source::Fedora,
    Source::Arch,
    Source::TpbRecent,
    Source::TpbAll,
    Source::Hjd,
    Source::FbyLt1,
    Source::Fby418,
];
fn root(source: Source) -> Option<&'static str> {
    match source {
        Source::Debian => Some("https://cdimage.debian.org/debian-cd/current/amd64/bt-cd/"),
        Source::Ubuntu => Some("https://releases.ubuntu.com/"),
        Source::Fedora => Some("https://torrent.fedoraproject.org/torrents.json"),
        Source::Arch => Some("https://archlinux.org/download/"),
        Source::TpbRecent => Some("https://apibay.org/precompiled/data_top100_48h.json"),
        Source::TpbAll => Some("https://apibay.org/precompiled/data_top100_all.json"),
        Source::Hjd | Source::FbyLt1 | Source::Fby418 => None,
    }
}

/// Strict origin/path allowlist; even URLs extracted from approved pages are untrusted.
fn allowed(source: Source, url: &Url) -> bool {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path().contains('%')
    {
        return false;
    }
    let p = url.path();
    match source {
        Source::Debian => {
            url.host_str() == Some("cdimage.debian.org")
                && p.starts_with("/debian-cd/current/amd64/bt-cd/")
        }
        Source::Ubuntu => url.host_str() == Some("releases.ubuntu.com") && p.starts_with('/'),
        Source::Fedora => {
            url.host_str() == Some("torrent.fedoraproject.org")
                && (p == "/torrents.json" || p.starts_with("/torrents/"))
        }
        Source::Arch => {
            url.host_str() == Some("archlinux.org")
                && (p == "/download/" || p.starts_with("/releng/releases/"))
        }
        Source::Hjd | Source::FbyLt1 | Source::Fby418 => false,
        Source::TpbRecent | Source::TpbAll => {
            url.host_str() == Some("apibay.org")
                && (p == "/robots.txt"
                    || p == "/precompiled/data_top100_48h.json"
                    || p == "/precompiled/data_top100_all.json")
        }
    }
}

fn is_official_torrent(source: Source, url: &Url) -> bool {
    if !allowed(source, url) {
        return false;
    }
    if url.path().ends_with(".torrent") {
        return true;
    }
    if source != Source::Arch {
        return false;
    }
    let Some(date) = url
        .path()
        .strip_prefix("/releng/releases/")
        .and_then(|p| p.strip_suffix("/torrent/"))
    else {
        return false;
    };
    date.len() == 10
        && date.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'.'
            } else {
                b.is_ascii_digit()
            }
        })
}

fn parse_forum_origin(value: &str) -> Option<Url> {
    let url = Url::parse(value.trim()).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.host(), Some(url::Host::Domain(_)))
    {
        return None;
    }
    Some(url)
}

fn forum_api_allowed(base: &Url, url: &Url) -> bool {
    if url.scheme() != "https"
        || url.origin() != base.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.path().contains('%')
    {
        return false;
    }
    if url.path() == "/robots.txt" {
        return url.query().is_none();
    }
    if !matches!(
        url.path(),
        "/torrent/getlist1"
            | "/torrent/getlist2"
            | "/torrent/getlist3"
            | "/torrent/getlist4"
            | "/torrent/getlist5"
            | "/torrent/getlist6"
    ) {
        return false;
    }
    let pairs = url.query_pairs().collect::<Vec<_>>();
    pairs.len() == 2
        && pairs.iter().any(|(k, v)| k == "t" && v == "2")
        && pairs
            .iter()
            .any(|(k, v)| k == "p" && (v == "1" || v == "2"))
}

struct Collector {
    requests: usize,
    last: HashMap<String, Instant>,
    retry_at: HashMap<Source, i64>,
    now: i64,
    forum_api_origin: Option<Url>,
    navigation_roots: HashMap<Source, Url>,
    received_bytes: usize,
}
enum FetchResult {
    Body(Vec<u8>),
    Redirect(Url),
}
impl Collector {
    fn source_url(&self, source: Source) -> Option<Url> {
        match source {
            Source::Hjd => self.forum_api_origin.clone(),
            Source::FbyLt1 | Source::Fby418 => self.navigation_roots.get(&source).cloned(),
            _ => root(source).and_then(|s| Url::parse(s).ok()),
        }
    }
    fn url_allowed(&self, source: Source, url: &Url) -> bool {
        allowed(source, url)
            || (source == Source::Hjd
                && self
                    .forum_api_origin
                    .as_ref()
                    .is_some_and(|base| forum_api_allowed(base, url)))
            || navigation_allowed(&self.navigation_roots, source, url)
    }
    fn account_bytes(&mut self, count: usize) -> Result<()> {
        self.received_bytes = self.received_bytes.saturating_add(count);
        ensure!(
            self.received_bytes <= MAX_COLLECTION_BYTES,
            "collection byte budget exhausted"
        );
        Ok(())
    }
    async fn fetch(&mut self, source: Source, url: &Url, limit: usize) -> Result<Vec<u8>> {
        match self.fetch_once(source, url, limit).await? {
            FetchResult::Body(body) => Ok(body),
            FetchResult::Redirect(_) => anyhow::bail!("redirect deferred"),
        }
    }
    async fn fetch_once(&mut self, source: Source, url: &Url, limit: usize) -> Result<FetchResult> {
        ensure!(self.url_allowed(source, url), "unapproved source URL");
        ensure!(
            self.requests < MAX_REQUESTS,
            "source request budget exhausted"
        );
        let current = current_time(self.now);
        if self.retry_at.get(&source).copied().unwrap_or(0) > current {
            anyhow::bail!("source deferred");
        }
        let host = url.host_str().context("missing host")?;
        if let Some(last) = self.last.get(host) {
            let wait = Duration::from_secs(2).saturating_sub(last.elapsed());
            if !wait.is_zero() {
                sleep(wait).await;
            }
        }
        self.requests += 1;
        self.last.insert(host.to_string(), Instant::now());
        let addresses =
            timeout(Duration::from_secs(5), tokio::net::lookup_host((host, 443))).await??;
        let mut found: Vec<SocketAddr> = Vec::new();
        for address in addresses.take(32) {
            ensure!(
                crate::peer::public_ip(address.ip()),
                "non-public DNS response"
            );
            found.push(address);
        }
        let address = *found.first().context("no public DNS address")?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve(host, address)
            .user_agent(AGENT)
            .timeout(Duration::from_secs(12))
            .build()?;
        let mut response = client.get(url.clone()).send().await?;
        if response.status().as_u16() == 404 && url.path() == "/robots.txt" {
            return Ok(FetchResult::Body(b"User-agent: *\nDisallow:\n".to_vec()));
        }
        if matches!(response.status().as_u16(), 403 | 429 | 502 | 503) {
            let status = response.status().as_u16();
            let raw = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|h| h.to_str().ok());
            let received = current_time(self.now);
            let delay = retry_after(raw, received).max(1800);
            // Same exact host shares a pause across source IDs, including TPB feeds.
            for item in SOURCES {
                if self
                    .source_url(item)
                    .and_then(|x| x.host_str().map(str::to_string))
                    .as_deref()
                    == Some(host)
                {
                    self.retry_at.insert(item, received.saturating_add(delay));
                }
            }
            self.retry_at.insert(source, received.saturating_add(delay));
            if status == 502 {
                anyhow::bail!("http_502");
            }
            anyhow::bail!("source deferred");
        }
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .context("missing redirect location")?;
            ensure!(location.len() <= 2048, "redirect too long");
            return Ok(FetchResult::Redirect(url.join(location)?));
        }
        ensure!(
            response.status().is_success(),
            "source response was not successful"
        );
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            self.account_bytes(chunk.len())?;
            ensure!(
                body.len().saturating_add(chunk.len()) <= limit,
                "source response too large"
            );
            body.extend_from_slice(&chunk);
        }
        Ok(FetchResult::Body(body))
    }
}

fn current_time(fallback: i64) -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(fallback)
        .max(fallback)
}

fn retry_after(value: Option<&str>, now: i64) -> i64 {
    let Some(value) = value else { return 1800 };
    if !value.trim().is_empty() && value.trim().bytes().all(|x| x.is_ascii_digit()) {
        return value
            .trim()
            .parse::<u64>()
            .ok()
            .and_then(|seconds| i64::try_from(seconds).ok())
            .unwrap_or(i64::MAX);
    }
    if let Ok(date) = httpdate::parse_http_date(value) {
        return date
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .unwrap_or(now)
            .saturating_sub(now)
            .max(0);
    }
    1800
}

// Conservative robots parser. Unsupported directives or malformed rules defer
// the entire source rather than infer permission. Select the most-specific user
// agent group and the longest applicable path rule, with Allow winning ties.
type RobotsGroup = (Vec<String>, Vec<(bool, String)>);

fn robots_allow(bytes: &[u8], path: &str) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut groups: Vec<RobotsGroup> = Vec::new();
    let mut agents = Vec::new();
    let mut rules = Vec::new();
    let mut had_rules = false;
    for raw in text.lines().take(100_000) {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, val)) = line.split_once(':') else {
            return false;
        };
        let key = key.trim().to_ascii_lowercase();
        let val = val.trim();
        match key.as_str() {
            "user-agent" => {
                if had_rules {
                    groups.push((std::mem::take(&mut agents), std::mem::take(&mut rules)));
                    had_rules = false;
                }
                if val.is_empty() {
                    return false;
                }
                agents.push(val.to_ascii_lowercase());
            }
            "allow" | "disallow" => {
                if agents.is_empty() {
                    return false;
                }
                had_rules = true;
                if !val.is_empty() {
                    if !val.starts_with('/')
                        || val.contains('*')
                        || val.contains('$')
                        || val.contains('%')
                    {
                        return false;
                    }
                    rules.push((key == "allow", val.to_string()));
                }
            }
            "sitemap" | "crawl-delay" | "host" => (),
            _ => return false,
        }
    }
    if !agents.is_empty() {
        groups.push((agents, rules));
    }
    // An empty or comment-only robots file has no disallow rules. Malformed
    // directives have already returned false above.
    if groups.is_empty() {
        return true;
    }
    let agent = AGENT.to_ascii_lowercase();
    let match_len = |names: &[String]| {
        names
            .iter()
            .filter_map(|name| {
                if name == "*" {
                    Some(0)
                } else if agent.contains(name) {
                    Some(name.len())
                } else {
                    None
                }
            })
            .max()
    };
    let Some(best) = groups
        .iter()
        .filter_map(|(names, _)| match_len(names))
        .max()
    else {
        return false;
    };
    let mut result = (0usize, true);
    for (names, rules) in groups {
        if match_len(&names) != Some(best) {
            continue;
        }
        for (allow, prefix) in rules {
            if path.starts_with(&prefix)
                && (prefix.len() > result.0 || (prefix.len() == result.0 && allow))
            {
                result = (prefix.len(), allow);
            }
        }
    }
    result.1
}

fn sample_from_torrent(source: Source, bytes: &[u8], now: i64) -> Option<SampleRecord> {
    let geometry = crate::samples::validate_torrent(bytes).ok()?;
    let name = official_name(bytes).unwrap_or_else(|| source.public_label().into());
    Some(SampleRecord {
        info_hash: geometry.info_hash,
        pool: PoolKind::Official,
        source,
        name,
        discovered_at: now,
        geometry: Some((
            geometry.piece_count,
            geometry.piece_length,
            geometry.total_length,
        )),
    })
}

fn safe_name(raw: &str) -> Option<String> {
    let clean: String = raw
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '+' | '(' | ')'))
        .take(80)
        .collect();
    let clean = clean.trim();
    (!clean.is_empty()).then(|| clean.to_string())
}
fn official_name(bytes: &[u8]) -> Option<String> {
    use bt_protocol::bencode::Value;
    let Value::Dict(top) = bt_protocol::bencode::decode(bytes).ok()? else {
        return None;
    };
    let Some(Value::Dict(info)) = top.get(b"info".as_slice()) else {
        return None;
    };
    let Some(Value::Bytes(name)) = info.get(b"name".as_slice()) else {
        return None;
    };
    safe_name(std::str::from_utf8(name).ok()?)
}
fn hash_sample(source: Source, value: &str, now: i64) -> Option<SampleRecord> {
    if value.len() != 40 || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut info_hash = [0u8; 20];
    hex::decode_to_slice(value, &mut info_hash).ok()?;
    let (pool, name) = if source.is_forum() {
        (PoolKind::Forum, "论坛")
    } else {
        (PoolKind::Official, "TPB")
    };
    Some(SampleRecord {
        info_hash,
        pool,
        source,
        name: name.into(),
        discovered_at: now,
        geometry: None,
    })
}

fn forum_page_number(value: &Value) -> Option<u64> {
    let n = match value {
        Value::Number(n) => n.as_u64()?,
        Value::String(s)
            if !s.is_empty() && s.len() <= 5 && s.bytes().all(|b| b.is_ascii_digit()) =>
        {
            s.parse().ok()?
        }
        _ => return None,
    };
    (1..=10_000).contains(&n).then_some(n)
}

fn parse_forum_feed(body: &[u8], requested_page: u64, now: i64) -> Result<Vec<SampleRecord>> {
    ensure!(body.len() <= MAX_PAGE, "source response too large");
    let data: Value = serde_json::from_slice(body)?;
    let page = forum_page_number(data.get("pagecurrent").context("invalid page")?)
        .context("invalid page")?;
    let pages = forum_page_number(data.get("pagetotal").context("invalid page total")?)
        .context("invalid page total")?;
    ensure!(
        page == requested_page && page <= pages,
        "mismatched pagination"
    );
    let rows = data
        .get("listav")
        .and_then(Value::as_array)
        .context("invalid source list")?;
    ensure!(rows.len() <= 100, "source list too large");
    let mut output = Vec::new();
    for row in rows {
        let Some(id) = row.get("tId").and_then(Value::as_str) else {
            continue;
        };
        if id.len() != 40 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let Some(raw) = row.get("tLinkM").and_then(Value::as_str) else {
            continue;
        };
        if raw.len() > 65536 {
            continue;
        }
        let Ok(magnet) = Url::parse(raw) else {
            continue;
        };
        if magnet.scheme() != "magnet" {
            continue;
        }
        let mut topics = magnet
            .query_pairs()
            .filter(|(k, _)| k.eq_ignore_ascii_case("xt"))
            .filter_map(|(_, v)| {
                if v.get(..9)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("urn:btih:"))
                {
                    v.get(9..).map(str::to_ascii_lowercase)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        topics.sort();
        topics.dedup();
        if topics.len() != 1 || !topics[0].eq_ignore_ascii_case(id) {
            continue;
        }
        if let Some(sample) = hash_sample(Source::Hjd, id, now) {
            output.push(sample);
        }
    }
    Ok(output)
}

async fn forum_api(c: &mut Collector, now: i64) -> Result<Vec<SampleRecord>> {
    let origin = c
        .forum_api_origin
        .clone()
        .context("source_not_configured")?;
    let robots = origin.join("/robots.txt")?;
    let policy = c.fetch(Source::Hjd, &robots, 256 * 1024).await?;
    let mut out = Vec::new();
    for board in choose_forum_boards() {
        let path = format!("/torrent/getlist{board}");
        ensure!(robots_allow(&policy, &path), "robots_denied");
        if c.requests >= MAX_REQUESTS {
            break;
        }
        let mut url = origin.join(&path)?;
        url.query_pairs_mut()
            .append_pair("t", "2")
            .append_pair("p", "1");
        let body = c.fetch(Source::Hjd, &url, MAX_PAGE).await?;
        out.extend(parse_forum_feed(&body, 1, now)?);
    }
    ensure!(!out.is_empty(), "source_empty");
    Ok(out)
}

fn choose_forum_boards() -> [u8; 3] {
    let mut boards = [1, 2, 3, 4, 5, 6];
    boards.shuffle(&mut rand::rng());
    [boards[0], boards[1], boards[2]]
}

async fn forum(c: &mut Collector, source: Source, now: i64) -> Result<Vec<SampleRecord>> {
    if source == Source::Hjd {
        return forum_api(c, now).await;
    }
    let url = c.source_url(source).context("source_not_configured")?;
    let robots = url.join("/robots.txt")?;
    let policy = c.fetch(source, &robots, 256 * 1024).await?;
    ensure!(
        robots_allow(&policy, url.path()),
        "robots policy not confirmed"
    );
    let body = if matches!(source, Source::FbyLt1 | Source::Fby418) {
        let mut current = url.clone();
        let mut visited = HashSet::new();
        let mut result = None;
        for _ in 0..=2 {
            ensure!(
                c.url_allowed(source, &current) && robots_allow(&policy, current.path()),
                "redirect denied"
            );
            ensure!(visited.insert(current.to_string()), "redirect loop");
            match c.fetch_once(source, &current, MAX_PAGE).await? {
                FetchResult::Body(body) => {
                    result = Some(body);
                    break;
                }
                FetchResult::Redirect(target) => {
                    ensure!(
                        c.url_allowed(source, &target),
                        "redirect target not allowlisted"
                    );
                    current = target;
                }
            }
        }
        result.context("navigation redirect limit")?
    } else {
        c.fetch(source, &url, MAX_PAGE).await?
    };
    if matches!(source, Source::Hjd | Source::FbyLt1 | Source::Fby418) {
        // The legacy page used a changing client-side API; do not guess API
        // routes, circumvent challenges, or publish any fetched page text.
        let text = std::str::from_utf8(&body)?;
        let mut out = Vec::new();
        for part in text.split("urn:btih:").skip(1).take(1000) {
            let hash: String = part
                .chars()
                .take_while(|c| c.is_ascii_hexdigit())
                .take(40)
                .collect();
            if let Some(sample) = hash_sample(source, &hash, now) {
                out.push(sample);
            }
        }
        ensure!(!out.is_empty(), "forum index format unsupported");
        return Ok(out);
    }
    let values: Vec<Value> = serde_json::from_slice(&body)?;
    let items: Vec<_> = values
        .into_iter()
        .take(1000)
        .filter_map(|v| {
            let mut item = hash_sample(source, v.get("info_hash").and_then(Value::as_str)?, now)?;
            item.name = v
                .get("name")
                .and_then(Value::as_str)
                .and_then(safe_name)
                .unwrap_or_else(|| "TPB".into());
            Some(item)
        })
        .collect();
    ensure!(!items.is_empty(), "no recognized forum samples");
    Ok(items)
}

async fn official(c: &mut Collector, source: Source, now: i64) -> Result<Vec<SampleRecord>> {
    let root = Url::parse(root(source).context("source_not_configured")?)?;
    let body = c.fetch(source, &root, MAX_PAGE).await?;
    let mut links = Vec::new();
    if source == Source::Fedora {
        let releases: Vec<Value> = serde_json::from_slice(&body)?;
        for release in releases.iter().rev().take(200) {
            if let Some(torrents) = release.get("torrents").and_then(Value::as_array) {
                for item in torrents.iter().take(200) {
                    if let Some(name) = item.get("torrent").and_then(Value::as_str)
                        && name.len() <= 200
                        && name.ends_with(".torrent")
                        && name
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
                    {
                        links.push(root.join(&format!("/torrents/{name}"))?);
                    }
                }
            }
        }
    } else {
        let text = std::str::from_utf8(&body)?;
        let mut subpages = Vec::new();
        for part in text.split(['\"', '\'']).take(4096) {
            if let Ok(link) = root.join(part)
                && allowed(source, &link)
            {
                if is_official_torrent(source, &link) {
                    links.push(link);
                } else if link.path().ends_with('/') && link != root {
                    subpages.push(link);
                }
            }
        }
        subpages.sort_by(|a, b| b.as_str().cmp(a.as_str()));
        subpages.dedup();
        for page in subpages.into_iter().take(2) {
            if let Ok(bytes) = c.fetch(source, &page, MAX_PAGE).await
                && let Ok(text) = std::str::from_utf8(&bytes)
            {
                for part in text.split(['\"', '\'']).take(4096) {
                    if let Ok(link) = page.join(part)
                        && allowed(source, &link)
                        && is_official_torrent(source, &link)
                    {
                        links.push(link);
                    }
                }
            }
        }
    }
    links.sort_by(|a, b| b.as_str().cmp(a.as_str()));
    links.dedup();
    let mut samples = Vec::new();
    let mut seen = HashSet::new();
    for link in links.into_iter().take(4) {
        if let Ok(bytes) = c.fetch(source, &link, MAX_TORRENT).await
            && let Some(item) = sample_from_torrent(source, &bytes, now)
            && seen.insert(item.info_hash)
        {
            samples.push(item);
        }
    }
    ensure!(!samples.is_empty(), "no validated public samples");
    Ok(samples)
}

/// Build a complete daily candidate in memory. Preserve old records for failed
/// or paused sources; calling code must persist the returned pool and retry
/// state atomically and honor its own scheduler/pressure controls.
pub async fn collect_daily(
    previous: &PreviousPool,
    now: i64,
    forum_api_origin: Option<&str>,
    navigation: &ForumNavigationRoots,
) -> Result<PoolUpdate> {
    ensure!(now >= 0, "invalid clock");
    let mut collector = Collector {
        requests: 0,
        last: HashMap::new(),
        retry_at: previous
            .sources
            .iter()
            .map(|x| (x.source, x.retry_at))
            .collect(),
        now,
        forum_api_origin: forum_api_origin.and_then(parse_forum_origin),
        navigation_roots: [
            (Source::FbyLt1, navigation.fby_lt1.as_deref()),
            (Source::Fby418, navigation.fby_418.as_deref()),
        ]
        .into_iter()
        .filter_map(|(source, value)| {
            value
                .and_then(parse_navigation_url)
                .map(|url| (source, url))
        })
        .collect(),
        received_bytes: 0,
    };
    let mut old_first: HashMap<[u8; 20], i64> = HashMap::new();
    for item in &previous.samples {
        old_first
            .entry(item.info_hash)
            .and_modify(|x| *x = (*x).min(item.discovered_at))
            .or_insert(item.discovered_at);
    }
    let mut all = Vec::new();
    let mut states = Vec::new();
    for source in SOURCES {
        let mut result = if source.is_forum() || source.is_index() {
            forum(&mut collector, source, now).await
        } else {
            official(&mut collector, source, now).await
        };
        if result.is_ok()
            && collector.retry_at.get(&source).copied().unwrap_or(0) > current_time(now)
        {
            result = Err(anyhow::anyhow!("source deferred"));
        }
        let status = if result.is_ok() { "ok" } else { "deferred" };
        let error_category = result.as_ref().err().map(|e| {
            match e.to_string().as_str() {
                "http_502" => "http_502",
                "robots_denied" => "robots_denied",
                "source_not_configured" => "source_not_configured",
                _ => "source_deferred",
            }
            .to_string()
        });
        let items = match result {
            Ok(items) => items,
            Err(_) => previous
                .samples
                .iter()
                .filter(|x| x.source == source)
                .cloned()
                .collect(),
        };
        all.extend(items);
        states.push(PoolStatus {
            source,
            status: status.into(),
            retry_at: 0,
            error_category,
        });
    }
    for state in &mut states {
        state.retry_at = collector.retry_at.get(&state.source).copied().unwrap_or(0);
    }
    Ok(PoolUpdate {
        samples: select_daily(all, &old_first),
        sources: states,
        collected_at: now,
    })
}

fn select_daily(all: Vec<SampleRecord>, old_first: &HashMap<[u8; 20], i64>) -> Vec<SampleRecord> {
    let (mut official, mut forum, mut index) = (Vec::new(), Vec::new(), Vec::new());
    for item in all {
        if item.source.is_forum() {
            forum.push(item);
        } else if item.source.is_index() {
            index.push(item);
        } else {
            official.push(item);
        }
    }
    let mut rng = rand::rng();
    official.shuffle(&mut rng);
    forum.shuffle(&mut rng);
    index.shuffle(&mut rng);
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    let mut counts = [0usize; 2];
    // Official distributions get first claim; forum gets second; TPB fills
    // remaining software slots only after both earlier sets are fixed.
    for group in [official, forum, index] {
        for mut item in group {
            let is_forum = item.source.is_forum();
            let slot = usize::from(is_forum);
            if counts[slot] >= MAX_POOL || !seen.insert(item.info_hash) {
                continue;
            }
            item.pool = if is_forum {
                PoolKind::Forum
            } else {
                PoolKind::Official
            };
            item.name = if is_forum {
                "论坛".into()
            } else {
                safe_name(&item.name).unwrap_or_else(|| item.source.public_label().into())
            };
            if let Some(first) = old_first.get(&item.info_hash) {
                item.discovered_at = item.discovered_at.min(*first);
            }
            counts[slot] += 1;
            selected.push(item);
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arch_extensionless_torrents_are_downloads_not_directories() {
        let valid =
            Url::parse("https://archlinux.org/releng/releases/2026.11.01/torrent/").unwrap();
        assert!(is_official_torrent(Source::Arch, &valid));
        for value in [
            "https://archlinux.org/releng/releases/2026.11.01/",
            "https://archlinux.org/releng/releases/latest/torrent/",
            "https://archlinux.org/releng/releases/2026.11.01/torrent/?token=x",
            "https://example.org/releng/releases/2026.11.01/torrent/",
        ] {
            assert!(!is_official_torrent(
                Source::Arch,
                &Url::parse(value).unwrap()
            ));
        }
        assert!(!is_official_torrent(Source::Debian, &valid));
    }
    #[test]
    fn daily_boards_are_distinct_and_body_budget_is_global() {
        let boards = choose_forum_boards();
        assert_eq!(boards.iter().copied().collect::<HashSet<_>>().len(), 3);
        assert!(boards.iter().all(|board| (1..=6).contains(board)));
        let mut collector = Collector {
            requests: 0,
            last: HashMap::new(),
            retry_at: HashMap::new(),
            now: 0,
            forum_api_origin: None,
            navigation_roots: HashMap::new(),
            received_bytes: 0,
        };
        assert!(collector.account_bytes(MAX_COLLECTION_BYTES - 1).is_ok());
        assert!(collector.account_bytes(1).is_ok());
        assert!(collector.account_bytes(1).is_err());
    }
    #[tokio::test]
    async fn unconfigured_forum_sources_never_request_network() {
        let mut c = Collector {
            requests: 0,
            last: HashMap::new(),
            retry_at: HashMap::new(),
            now: 0,
            forum_api_origin: None,
            navigation_roots: HashMap::new(),
            received_bytes: 0,
        };
        for source in [Source::Hjd, Source::FbyLt1, Source::Fby418] {
            let error = forum(&mut c, source, 0).await.unwrap_err();
            assert_eq!(error.to_string(), "source_not_configured");
        }
        assert_eq!(c.requests, 0);
    }
    #[test]
    fn navigation_config_limits_origin_path_and_redirects() {
        let mut roots = HashMap::new();
        let first = parse_navigation_url("https://example.invalid/board-a").unwrap();
        let second = parse_navigation_url("https://example.invalid/board-b").unwrap();
        roots.insert(Source::FbyLt1, first.clone());
        roots.insert(Source::Fby418, second.clone());
        assert!(navigation_allowed(&roots, Source::FbyLt1, &first));
        assert!(navigation_allowed(&roots, Source::FbyLt1, &second));
        assert!(navigation_allowed(
            &roots,
            Source::FbyLt1,
            &Url::parse("https://example.invalid/robots.txt").unwrap()
        ));
        for target in [
            "https://other.invalid/board-a",
            "https://example.invalid/elsewhere",
            "https://example.invalid/board-a?token=x",
            "https://example.invalid/board-a#secret",
        ] {
            assert!(!navigation_allowed(
                &roots,
                Source::FbyLt1,
                &Url::parse(target).unwrap()
            ));
        }
        for value in [
            "",
            "http://example.invalid/board",
            "https://user:password@example.invalid/board",
            "https://127.0.0.1/board",
            "https://example.invalid:8443/board",
            "https://example.invalid/board?token=x",
            "https://example.invalid/%2fboard",
        ] {
            assert!(parse_navigation_url(value).is_none());
        }
        assert!(!navigation_allowed(&HashMap::new(), Source::FbyLt1, &first));
    }
    #[test]
    fn forum_api_origin_and_paths_are_strict() {
        let base = parse_forum_origin("https://feed.example.org").unwrap();
        assert!(forum_api_allowed(
            &base,
            &Url::parse("https://feed.example.org/torrent/getlist6?t=2&p=1").unwrap()
        ));
        for value in [
            "https://user@feed.example.org",
            "http://feed.example.org",
            "https://feed.example.org/a",
            "https://feed.example.org?token=x",
            "https://127.0.0.1",
            "https://feed.example.org:8443",
        ] {
            assert!(parse_forum_origin(value).is_none());
        }
        for value in [
            "https://other.example.org/torrent/getlist1?t=2&p=1",
            "https://feed.example.org/torrent/getlist7?t=2&p=1",
            "https://feed.example.org/torrent/getlist1?t=2&p=3",
            "https://feed.example.org/torrent/getlist1?t=2&p=1&token=secret",
            "https://feed.example.org/torrent/getlist1?t=2&p=1&t=2",
        ] {
            assert!(!forum_api_allowed(&base, &Url::parse(value).unwrap()));
        }
    }

    #[test]
    fn forum_feed_rejects_mismatches_and_preserves_no_title_or_url() -> Result<()> {
        let hash = "0123456789012345678901234567890123456789";
        let good =
            format!("magnet:?xt=urn:btih:{hash}&dn=private-title&tr=https%3A%2F%2Fexample.org");
        let mismatch = "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let duplicate = format!(
            "magnet:?xt=urn:btih:{hash}&xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        let body = serde_json::to_vec(
            &serde_json::json!({"listav":[{"tId":hash,"tLinkM":good,"title":"private-title"},{"tId":hash,"tLinkM":mismatch},{"tId":hash,"tLinkM":duplicate}],"pagecurrent":"1","pagetotal":2}),
        )?;
        let samples = parse_forum_feed(&body, 1, 123)?;
        assert_eq!(samples.len(), 1);
        assert_eq!(hex::encode(samples[0].info_hash), hash);
        assert_eq!(samples[0].source, Source::Hjd);
        assert_eq!(samples[0].name, "论坛");
        assert!(samples[0].geometry.is_none());
        assert!(parse_forum_feed(&body, 2, 123).is_err());
        let oversized = serde_json::to_vec(
            &serde_json::json!({"listav":vec![serde_json::json!({});101],"pagecurrent":1,"pagetotal":1}),
        )?;
        assert!(parse_forum_feed(&oversized, 1, 123).is_err());
        Ok(())
    }
    #[test]
    fn robots_group_and_longest_rule() {
        assert!(robots_allow(b"", "/precompiled/data_top100_48h.json"));
        assert!(robots_allow(
            b"# one\n\n # another\n",
            "/precompiled/data_top100_48h.json"
        ));
        assert!(!robots_allow(b"User-agent: *\nDisallow: /\n", "/bt/"));
        assert!(robots_allow(
            b"User-agent: *\nDisallow: /\nAllow: /bt/\n",
            "/bt/"
        ));
        assert!(!robots_allow(b"User-agent: *\nDisallow: /bt/*\n", "/bt/"));
        assert!(!robots_allow(b"garbage", "/bt/"));
    }
    #[test]
    fn approved_paths_and_hashes() {
        assert!(!allowed(
            Source::TpbAll,
            &Url::parse("https://apibay.org/precompiled/data_top100_all.json?x=1").unwrap()
        ));
        assert!(!allowed(
            Source::TpbAll,
            &Url::parse("https://apibay.org@evil.example/precompiled/data_top100_all.json")
                .unwrap()
        ));
        assert!(
            hash_sample(
                Source::TpbAll,
                "0123456789012345678901234567890123456789",
                1
            )
            .is_some()
        );
        assert!(hash_sample(Source::TpbAll, "xyz", 1).is_none());
        assert_eq!(Source::TpbAll.public_label(), "TPB");
    }
    #[test]
    fn daily_selection_prioritizes_official_then_forum_then_tpb() {
        let sample = |source, byte| SampleRecord {
            info_hash: [byte; 20],
            pool: PoolKind::Official,
            source,
            name: "label".into(),
            discovered_at: 20,
            geometry: None,
        };
        let result = select_daily(
            vec![
                sample(Source::TpbAll, 1),
                sample(Source::TpbRecent, 2),
                sample(Source::Debian, 1),
                sample(Source::Hjd, 2),
                sample(Source::TpbAll, 3),
            ],
            &HashMap::from([([1; 20], 10)]),
        );
        assert_eq!(result.len(), 3);
        let one = result.iter().find(|s| s.info_hash == [1; 20]).unwrap();
        assert_eq!(one.source, Source::Debian);
        assert_eq!(one.discovered_at, 10);
        let two = result.iter().find(|s| s.info_hash == [2; 20]).unwrap();
        assert_eq!(two.source, Source::Hjd);
        assert_eq!(two.pool, PoolKind::Forum);
        assert_eq!(two.name, "论坛");
        let three = result.iter().find(|s| s.info_hash == [3; 20]).unwrap();
        assert_eq!(three.pool, PoolKind::Official);
        assert_eq!(three.source, Source::TpbAll);
    }
}
