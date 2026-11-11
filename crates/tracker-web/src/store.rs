//! Read-only access to the observatory's explicitly public projection.
use std::{
    collections::HashSet,
    net::{Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    time::Duration,
};

use percent_encoding::percent_decode_str;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use tracker_model::{
    Endpoint, FamilyStatus, Observation, Overview, PoolStatus, StatsHistory, StatsPoint, VERSION,
};
use url::Url;

#[derive(Clone)]
pub(crate) struct PublicStore {
    path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct StoreError;

#[derive(Debug, Clone, Default)]
pub(crate) struct Filters {
    pub query: String,
    pub protocol: String,
    pub page: usize,
    pub limit: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Page {
    pub items: Vec<Endpoint>,
    pub total: Option<u64>,
    pub page: usize,
    pub limit: usize,
    pub warming: bool,
    pub has_snapshot: bool,
}

impl PublicStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join("public.sqlite3"),
        }
    }

    fn open(&self) -> Result<Option<Connection>, StoreError> {
        if !self.path.is_file() {
            return Ok(None);
        }
        let db = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| StoreError)?;
        db.busy_timeout(Duration::from_millis(500))
            .map_err(|_| StoreError)?;
        db.pragma_update(None, "query_only", true)
            .map_err(|_| StoreError)?;
        Ok(Some(db))
    }

    pub fn pools(&self) -> Result<Vec<PoolStatus>, StoreError> {
        let Some(db) = self.open()? else {
            return Ok(Vec::new());
        };
        let payload: Option<String> = db.query_row("SELECT payload FROM documents WHERE key='sample_pools' AND length(CAST(payload AS BLOB))<=16384", [], |r| r.get(0)).optional().map_err(|_| StoreError)?;
        Ok(payload
            .and_then(|p| serde_json::from_str::<Vec<PoolStatus>>(&p).ok())
            .unwrap_or_default()
            .into_iter()
            .take(8)
            .filter_map(|mut pool| {
                if !matches!(pool.pool.as_str(), "software" | "forum")
                    || !matches!(pool.status.as_str(), "ready" | "partial" | "deferred")
                {
                    return None;
                }
                pool.error_category = safe_reason(pool.error_category);
                Some(pool)
            })
            .collect())
    }

    /// A single published minute bucket, never a live request to the native engine.
    pub fn native_summary(&self, now: i64) -> Result<Option<serde_json::Value>, StoreError> {
        let Some(db) = self.open()? else {
            return Ok(None);
        };
        if !has_table(&db, "stats_history")? {
            return Ok(None);
        }
        let raw: Option<String> = db.query_row(
            "SELECT payload FROM stats_history WHERE length(CAST(payload AS BLOB))<=16384 ORDER BY bucket DESC LIMIT 1",
            [], |row| row.get(0)
        ).optional().map_err(|_| StoreError)?;
        let point = raw
            .and_then(|p| serde_json::from_str::<StatsPoint>(&p).ok())
            .and_then(|p| sanitize_point(p, now.saturating_sub(180), now));
        let Some(point) = point else {
            return Ok(None);
        };
        let mut result = serde_json::Map::new();
        for key in ["peers", "swarms"] {
            if let Some(value) = point.gauges.get(key).filter(|v| **v <= u64::MAX as f64) {
                result.insert(key.into(), serde_json::json!(*value as u64));
            }
        }
        if let Some(value) = point.counters.get("accepted_announces") {
            result.insert("announces".into(), serde_json::json!(value));
        }
        result.insert("sampled_at".into(), serde_json::json!(point.at));
        Ok(Some(serde_json::Value::Object(result)))
    }

    pub fn history(&self, window: &str, now: i64) -> Result<StatsHistory, StoreError> {
        let (span, step) = match window {
            "24h" => (86_400, 60),
            "7d" => (604_800, 600),
            "30d" => (2_592_000, 3600),
            _ => return Err(StoreError),
        };
        let mut result = StatsHistory {
            window: window.to_owned(),
            step_seconds: step as u64,
            items: Vec::new(),
        };
        let Some(db) = self.open()? else {
            return Ok(result);
        };
        if !has_table(&db, "stats_history")? {
            return Ok(result);
        }
        let mut q = db.prepare("SELECT payload FROM stats_history WHERE bucket IN (SELECT MAX(bucket) FROM stats_history WHERE bucket>=?1 AND bucket<=?2 GROUP BY CAST(bucket/?3 AS INTEGER)) AND length(CAST(payload AS BLOB))<=16384 ORDER BY bucket LIMIT 1500").map_err(|_| StoreError)?;
        let rows = q
            .query_map(params![now.saturating_sub(span), now, step], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|_| StoreError)?;
        result.items = rows
            .flatten()
            .filter_map(|p| serde_json::from_str::<StatsPoint>(&p).ok())
            .filter_map(|p| sanitize_point(p, now.saturating_sub(span), now))
            .collect();
        Ok(result)
    }

    pub fn overview(&self) -> Result<Overview, StoreError> {
        let warming = || Overview {
            version: VERSION.to_owned(),
            warming: true,
            ..Overview::default()
        };
        let Some(db) = self.open()? else {
            return Ok(warming());
        };
        let payload: Option<Option<String>> = db.query_row(
            "SELECT CASE WHEN length(CAST(payload AS BLOB))<=32768 THEN payload ELSE NULL END FROM documents WHERE key='overview'", [], |r| r.get(0),
        ).optional().map_err(|_| StoreError)?;
        match payload {
            None => Ok(warming()),
            Some(Some(payload)) => {
                let mut overview: Overview =
                    serde_json::from_str(&payload).map_err(|_| StoreError)?;
                overview.version = VERSION.to_owned();
                Ok(overview)
            }
            Some(None) => Err(StoreError),
        }
    }

    pub fn page(&self, filters: Filters, now: i64) -> Result<Page, StoreError> {
        let page = filters.page.clamp(1, 10_000);
        let limit = filters.limit.clamp(1, 100);
        let Some(db) = self.open()? else {
            return Ok(Page {
                items: Vec::new(),
                total: None,
                page,
                limit,
                warming: true,
                has_snapshot: false,
            });
        };
        let overview: Option<Option<String>> = db.query_row("SELECT CASE WHEN length(CAST(payload AS BLOB))<=32768 THEN payload ELSE NULL END FROM documents WHERE key='overview'", [], |r| r.get(0))
            .optional().map_err(|_| StoreError)?;
        let overview = match overview {
            None => None,
            Some(None) => return Err(StoreError),
            Some(Some(payload)) => {
                Some(serde_json::from_str::<Overview>(&payload).map_err(|_| StoreError)?)
            }
        };
        let has_snapshot = overview.as_ref().is_some_and(|o| o.generated_at > 0);
        let warming = overview.is_none_or(|o| o.warming || o.generated_at <= 0);
        let query: String = filters.query.chars().take(128).collect();
        let pattern = format!(
            "%{}%",
            query
                .to_lowercase()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let protocol = if ["udp", "http", "https"].contains(&filters.protocol.as_str()) {
            filters.protocol
        } else {
            String::new()
        };
        let protocol_pattern = if protocol.is_empty() {
            "%".to_owned()
        } else {
            format!("{protocol}://%")
        };
        let total: u64 = db.query_row(
            "SELECT count(*) FROM endpoints WHERE length(CAST(payload AS BLOB))<=65536 AND json_valid(payload) AND lower(url) LIKE ?1 ESCAPE '\\' AND lower(url) LIKE ?2",
            params![pattern, protocol_pattern], |r| r.get(0),
        ).map_err(|_| StoreError)?;
        let offset = (page - 1).saturating_mul(limit);
        let mut statement = db.prepare(
            "SELECT payload FROM endpoints WHERE length(CAST(payload AS BLOB))<=65536 AND json_valid(payload) AND lower(url) LIKE ?1 ESCAPE '\\' AND lower(url) LIKE ?2 ORDER BY COALESCE(CAST(json_extract(payload,'$.evidence.success_rate_24h') AS REAL),-1.0) DESC, COALESCE(CAST(json_extract(payload,'$.evidence.sample_count_7d') AS INTEGER),0) DESC, COALESCE(CAST(json_extract(payload,'$.evidence.median_latency_ms') AS REAL),1.0e99) ASC, url COLLATE NOCASE LIMIT ?3 OFFSET ?4",
        ).map_err(|_| StoreError)?;
        let rows = statement
            .query_map(
                params![pattern, protocol_pattern, limit as i64, offset as i64],
                |r| r.get::<_, String>(0),
            )
            .map_err(|_| StoreError)?;
        let mut items = Vec::new();
        for payload in rows.flatten() {
            if payload.len() > 65_536 {
                continue;
            }
            let Ok(endpoint) = serde_json::from_str::<Endpoint>(&payload) else {
                continue;
            };
            if let Some(endpoint) = sanitize_endpoint(endpoint, now) {
                items.push(endpoint);
            }
        }
        Ok(Page {
            items,
            total: Some(total),
            page,
            limit,
            warming,
            has_snapshot,
        })
    }

    pub fn endpoint(&self, id: &str, now: i64) -> Result<Option<Endpoint>, StoreError> {
        if !valid_id(id) {
            return Ok(None);
        }
        let Some(db) = self.open()? else {
            return Ok(None);
        };
        let payload: Option<String> = db.query_row(
            "SELECT payload FROM endpoints WHERE id=?1 AND length(CAST(payload AS BLOB))<=65536", [id], |r| r.get(0),
        ).optional().map_err(|_| StoreError)?;
        Ok(payload
            .filter(|p| p.len() <= 65_536)
            .and_then(|p| serde_json::from_str::<Endpoint>(&p).ok())
            .and_then(|ep| sanitize_endpoint(ep, now)))
    }

    pub fn observations(
        &self,
        id: &str,
        family: Option<&str>,
        stage: Option<&str>,
        now: i64,
        limit: usize,
    ) -> Result<Vec<Observation>, StoreError> {
        if !valid_id(id) {
            return Ok(Vec::new());
        }
        let Some(db) = self.open()? else {
            return Ok(Vec::new());
        };
        let url: Option<String> = db
            .query_row("SELECT url FROM endpoints WHERE id=?1", [id], |r| r.get(0))
            .optional()
            .map_err(|_| StoreError)?;
        let Some(protocol) = url
            .as_deref()
            .and_then(public_tracker_url)
            .map(|u| u.scheme().to_owned())
        else {
            return Ok(Vec::new());
        };
        let mut statement = db.prepare(
            "SELECT payload FROM observations WHERE endpoint_id=?1 AND observed_at>=?2 AND observed_at<=?3 AND (?4='' OR family=?4) AND length(CAST(payload AS BLOB))<=4096 AND (?6='' OR CASE WHEN json_valid(payload) THEN json_extract(payload,'$.stage') ELSE '' END=?6) ORDER BY observed_at DESC,id DESC LIMIT ?5",
        ).map_err(|_| StoreError)?;
        let rows = statement
            .query_map(
                params![
                    id,
                    now.saturating_sub(86_400),
                    now,
                    family.unwrap_or(""),
                    limit.min(500) as i64,
                    stage.unwrap_or("")
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(|_| StoreError)?;
        let mut observations = Vec::new();
        for payload in rows.flatten() {
            if payload.len() > 4096 {
                continue;
            }
            let Ok(observation) = serde_json::from_str::<Observation>(&payload) else {
                continue;
            };
            if let Some(mut observation) = sanitize_observation(observation, id, now) {
                if protocol == "udp" {
                    observation.http_status = None;
                }
                observations.push(observation);
            }
        }
        Ok(observations)
    }

    /// `None` denotes an unpublished document rather than a verified empty list.
    pub fn feed_published(&self, filename: &str) -> Result<bool, StoreError> {
        let Some(db) = self.open()? else {
            return Ok(false);
        };
        let key = format!("feed:{filename}");
        let status: Option<i64> = db.query_row("SELECT CASE WHEN length(CAST(payload AS BLOB))>16777216 THEN -1 WHEN NOT json_valid(payload) THEN -1 WHEN json_type(payload)!='text' THEN -1 ELSE 1 END FROM documents WHERE key=?1", [key], |r| r.get(0))
            .optional().map_err(|_| StoreError)?;
        match status {
            None => Ok(false),
            Some(1) => Ok(true),
            _ => Err(StoreError),
        }
    }

    pub fn feed(&self, filename: &str) -> Result<Option<Vec<String>>, StoreError> {
        let Some(db) = self.open()? else {
            return Ok(None);
        };
        let key = format!("feed:{filename}");
        let payload: Option<Option<String>> = db.query_row(
            "SELECT CASE WHEN length(CAST(payload AS BLOB))<=16777216 THEN payload ELSE NULL END FROM documents WHERE key=?1", [key], |r| r.get(0),
        ).optional().map_err(|_| StoreError)?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let Some(payload) = payload else {
            return Err(StoreError);
        };
        let text: String = serde_json::from_str(&payload).map_err(|_| StoreError)?;
        drop(payload);
        let mut seen: HashSet<&str> = HashSet::new();
        let entries = text
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter(|s| public_tracker_url(s).is_some())
            .filter(|s| seen.insert(*s))
            .map(str::to_owned)
            .collect();
        Ok(Some(entries))
    }
}

fn has_table(db: &Connection, name: &str) -> Result<bool, StoreError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )
    .map_err(|_| StoreError)
}

fn sanitize_point(mut point: StatsPoint, start: i64, now: i64) -> Option<StatsPoint> {
    if !(start..=now).contains(&point.at) || !valid_id(&point.instance) {
        return None;
    }
    point.gauges.retain(|k, v| {
        matches!(k.as_str(), "peers" | "swarms" | "rss_bytes") && v.is_finite() && *v >= 0.0
    });
    point.counters.retain(|k, _| {
        matches!(
            k.as_str(),
            "accepted_announces"
                | "business_failures"
                | "http_announces"
                | "udp_announces"
                | "accepted_ipv4"
                | "accepted_ipv6"
        )
    });
    Some(point)
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn private_token_path(path: &str) -> bool {
    let path = percent_decode_str(path).decode_utf8_lossy().to_lowercase();
    path.split('/').filter(|s| !s.is_empty()).any(|segment| {
        [
            "passkey",
            "token",
            "auth",
            "secret",
            "credential",
            "apikey",
            "api_key",
        ]
        .iter()
        .any(|word| segment.contains(word))
            || (segment.len() >= 32 && segment.bytes().all(|b| b.is_ascii_hexdigit()))
    })
}

fn public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_documentation()
        || a == 0
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 198 && (b == 18 || b == 19))
        || (a == 192 && b == 0 && c == 0))
}

fn public_ipv6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    !ip.is_unspecified()
        && !ip.is_loopback()
        && !ip.is_multicast()
        && ip.to_ipv4().is_none()
        && (s[0] & 0xe000) == 0x2000
        && !(s[0] == 0x2001 && (s[1] == 0x0db8 || s[1] == 0x0002 || (s[1] & 0xfff0) == 0x0010))
}

fn public_host(value: &Url) -> bool {
    match value.host() {
        Some(url::Host::Ipv4(ip)) => public_ipv4(ip),
        Some(url::Host::Ipv6(ip)) => public_ipv6(ip),
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            host.contains('.')
                && ![
                    "localhost",
                    ".localhost",
                    ".local",
                    ".lan",
                    ".internal",
                    ".home.arpa",
                ]
                .iter()
                .any(|suffix| host == suffix.trim_start_matches('.') || host.ends_with(suffix))
        }
        None => false,
    }
}

pub(crate) fn public_tracker_url(raw: &str) -> Option<Url> {
    if raw.len() > 512 || raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let value = Url::parse(raw).ok()?;
    if !matches!(value.scheme(), "udp" | "http" | "https")
        || !public_host(&value)
        || !value.username().is_empty()
        || value.password().is_some()
        || value.query().is_some()
        || value.fragment().is_some()
        || private_token_path(value.path())
    {
        return None;
    }
    Some(value)
}

fn bounded_label(s: &str, limit: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(limit).collect()
}

fn safe_reason(value: Option<String>) -> Option<String> {
    value.filter(|s| {
        matches!(
            s.as_str(),
            "timeout"
                | "connect_timeout"
                | "response_timeout"
                | "dns_timeout"
                | "dns_error"
                | "transport"
                | "network_unavailable"
                | "ipv6_unavailable"
                | "local_unavailable"
                | "local_restriction"
                | "operator_denied"
                | "family_unavailable"
                | "local_or_network_unknown"
                | "rate_limited"
                | "http_status"
                | "http_error"
                | "invalid_response"
                | "invalid_protocol"
                | "incomplete_protocol"
                | "peers_unusable"
                | "protocol_error"
                | "tracker_rejection"
                | "resource_unregistered"
                | "http_502"
                | "robots_denied"
                | "source_not_configured"
                | "source_deferred"
                | "budget_exhausted"
                | "cancelled"
                | "unsupported"
                | "unknown"
                | "os_error_10060"
        )
    })
}

fn safe_result(value: &str) -> &'static str {
    match value {
        "announce_ok" => "announce_ok",
        "zero_peers" => "zero_peers",
        "accepted" => "accepted",
        "success" => "success",
        "failure" | "failed" => "failure",
        "rejected" => "rejected",
        "tracker_rejection" => "tracker_rejection",
        "resource_unregistered" => "resource_unregistered",
        "timeout" => "timeout",
        "connect_timeout" => "connect_timeout",
        "response_timeout" => "response_timeout",
        "http_error" => "http_error",
        "invalid_protocol" => "invalid_protocol",
        "invalid_response" => "invalid_response",
        "protocol_error" => "protocol_error",
        "transport" => "transport",
        "dns" => "dns",
        "dns_error" => "dns_error",
        "local_unavailable" => "local_unavailable",
        "local_restriction" => "local_restriction",
        "network_unavailable" => "network_unavailable",
        "rate_limited" => "rate_limited",
        "skipped" => "skipped",
        "unsupported" => "unsupported",
        "unavailable" => "unavailable",
        "handshake_success" => "handshake_success",
        "handshake_failed" => "handshake_failed",
        "bitmap_available" => "bitmap_available",
        "bitmap_missing" => "bitmap_missing",
        _ => "unknown",
    }
}

fn timing_unavailable(reason: Option<&str>) -> bool {
    matches!(
        reason,
        Some(
            "timeout"
                | "connect_timeout"
                | "response_timeout"
                | "dns_timeout"
                | "dns_error"
                | "transport"
                | "local_unavailable"
                | "local_or_network_unknown"
                | "network_unavailable"
                | "family_unavailable"
                | "operator_denied"
                | "local_restriction"
                | "budget_exhausted"
                | "incomplete_protocol"
                | "cancelled"
        )
    )
}

fn sanitize_family(mut f: FamilyStatus, now: i64) -> Option<FamilyStatus> {
    if !matches!(f.family.as_str(), "ipv4" | "ipv6") {
        return None;
    }
    f.state = match f.state.as_str() {
        "success" | "accepted" | "online" | "active" => "success",
        "failure" | "failed" | "failing" | "offline" => "failure",
        "partial" | "degraded" => "partial",
        "unsupported" | "not_published" => "unsupported",
        "local_unavailable" | "family_unavailable" | "unavailable" => "local_unavailable",
        "stale" => "stale",
        _ => "unknown",
    }
    .to_owned();
    if f.checked_at.is_some() && f.valid_until.is_some_and(|t| t < now) {
        f.state = "stale".to_owned();
    }
    f.protocol_result = f
        .protocol_result
        .as_deref()
        .map(|v| safe_result(v).to_owned());
    f.error_category = safe_reason(f.error_category);
    f.http_status = f.http_status.filter(|n| (100..=599).contains(n));
    f.latency_ms = f
        .latency_ms
        .filter(|x| x.is_finite() && *x >= 0.0 && *x <= 600_000.0);
    Some(f)
}

fn sanitize_endpoint(mut endpoint: Endpoint, now: i64) -> Option<Endpoint> {
    if !valid_id(&endpoint.id) || endpoint.service_id.len() > 255 {
        return None;
    }
    let url = public_tracker_url(&endpoint.url)?;
    endpoint.protocol = url.scheme().to_owned();
    endpoint.host = bounded_label(url.host_str()?, 253);
    if endpoint
        .service_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
        && !(endpoint.service_id.len() == 40
            && endpoint.service_id.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        endpoint.service_id = bounded_label(&endpoint.service_id, 255);
    } else {
        endpoint.service_id = endpoint.host.clone();
    }
    endpoint.families = endpoint
        .families
        .into_iter()
        .take(2)
        .filter_map(|f| sanitize_family(f, now))
        .collect();
    for family in &mut endpoint.families {
        if endpoint.protocol == "udp" {
            family.http_status = None;
        }
        let replied = family.http_status.is_some()
            || matches!(
                family.protocol_result.as_deref(),
                Some(
                    "announce_ok"
                        | "accepted"
                        | "success"
                        | "zero_peers"
                        | "tracker_rejection"
                        | "resource_unregistered"
                )
            );
        if timing_unavailable(family.error_category.as_deref()) || !replied {
            family.latency_ms = None;
        }
    }
    endpoint.infrastructure.asn = endpoint.infrastructure.asn.map(|x| bounded_label(&x, 24));
    endpoint.infrastructure.organization = endpoint
        .infrastructure
        .organization
        .map(|x| bounded_label(&x, 128));
    endpoint.infrastructure.country = endpoint
        .infrastructure
        .country
        .map(|x| bounded_label(&x, 16));
    Some(endpoint)
}

fn sanitize_observation(mut observation: Observation, id: &str, now: i64) -> Option<Observation> {
    if observation.endpoint_id != id
        || !matches!(observation.family.as_str(), "ipv4" | "ipv6")
        || !(now.saturating_sub(86_400)..=now).contains(&observation.observed_at)
    {
        return None;
    }
    observation.stage = match observation.stage.as_str() {
        "announce" => "announce",
        "sample_announce" => "sample_announce",
        "handshake" => "handshake",
        "bitmap" => "bitmap",
        _ => "unknown",
    }
    .to_owned();
    observation.result = match observation.stage.as_str() {
        "sample_announce" => match observation.result.as_str() {
            "accepted" => "accepted",
            "empty" => "empty",
            "failed" => "failed",
            _ => "unknown",
        },
        "handshake" => match observation.result.as_str() {
            "accepted" => "accepted",
            "failed" => "failed",
            _ => "unknown",
        },
        _ => safe_result(&observation.result),
    }
    .to_owned();
    observation.error_category = safe_reason(observation.error_category);
    observation.http_status = observation.http_status.filter(|n| (100..=599).contains(n));
    observation.latency_ms = observation
        .latency_ms
        .filter(|x| x.is_finite() && *x >= 0.0 && *x <= 600_000.0);
    if observation.stage == "handshake" {
        // Peer wire handshakes run over TCP; these are not Tracker protocol
        // replies and cannot have an HTTP status.
        observation.http_status = None;
        observation.protocol_reply = false;
    }
    let confirmed = if observation.stage == "handshake" {
        observation.result == "accepted"
    } else {
        observation.protocol_reply || observation.http_status.is_some()
    };
    if timing_unavailable(observation.error_category.as_deref()) || !confirmed {
        observation.latency_ms = None;
    }
    Some(observation)
}
