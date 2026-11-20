//! Read-only access to the observatory's explicitly public projection.
use std::{
    collections::{HashSet, VecDeque},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    time::Duration,
};

use percent_encoding::percent_decode_str;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
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
    #[serde(skip)]
    pub matched_ip: Option<String>,
    #[serde(skip)]
    pub ip_match_ids: HashSet<String>,
    #[serde(skip)]
    pub url_ip_match_ids: HashSet<String>,
    #[serde(skip)]
    pub shared_ip_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LogEntry {
    pub id: i64,
    pub observation: Observation,
}

#[derive(Debug, Clone)]
pub(crate) struct DetailLog {
    pub items: Vec<LogEntry>,
    pub timeline: Vec<LogEntry>,
    pub total: u64,
    pub page: usize,
    pub pages: u64,
    pub window: String,
    pub include_no_address: bool,
    pub hidden_no_address: u64,
    pub focus_id: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct TrackerDetail {
    pub endpoint: Option<Endpoint>,
    pub log: Option<DetailLog>,
    pub canonical_id: Option<String>,
}

impl Default for DetailLog {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            timeline: Vec::new(),
            total: 0,
            page: 1,
            pages: 1,
            window: "24h".to_owned(),
            include_no_address: false,
            hidden_no_address: 0,
            focus_id: None,
        }
    }
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

    /// A single published minute bucket plus the trailing 24-hour window, never
    /// a live request to the native engine.
    pub fn native_summary(&self, now: i64) -> Result<Option<serde_json::Value>, StoreError> {
        let Some(db) = self.open()? else {
            return Ok(None);
        };
        native_summary_from(&db, now)
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
        let requested_page = filters.page.clamp(1, 10_000);
        let page = requested_page;
        let limit = filters.limit.clamp(1, 100);
        let Some(mut db) = self.open()? else {
            return Ok(Page {
                items: Vec::new(),
                total: None,
                page,
                limit,
                warming: true,
                has_snapshot: false,
                matched_ip: None,
                ip_match_ids: HashSet::new(),
                url_ip_match_ids: HashSet::new(),
                shared_ip_count: 0,
            });
        };
        let tx = db.transaction().map_err(|_| StoreError)?;
        let overview: Option<Option<String>> = tx.query_row("SELECT CASE WHEN length(CAST(payload AS BLOB))<=32768 THEN payload ELSE NULL END FROM documents WHERE key='overview'", [], |r| r.get(0))
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
        let matched_ip = public_search_ip(&query);
        let ip_match_ids = matched_ip
            .as_deref()
            .map(|ip| latest_target_endpoints(&tx, now, ip))
            .transpose()?
            .unwrap_or_default();
        let pattern = format!(
            "%{}%",
            if matched_ip.is_some() {
                String::new()
            } else {
                query
                    .to_lowercase()
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            }
        );
        let alias_clause = if matched_ip.is_some() {
            "1=1"
        } else {
            "json_extract(payload,'$.alias_of') IS NULL"
        };
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
        let mut statement = tx.prepare(&format!(
            "SELECT payload FROM endpoints WHERE length(CAST(payload AS BLOB))<=65536 AND json_valid(payload) AND {alias_clause} AND lower(url) LIKE ?1 ESCAPE '\\' AND lower(url) LIKE ?2 ORDER BY COALESCE(CAST(json_extract(payload,'$.evidence.success_rate_24h') AS REAL),-1.0) DESC, COALESCE(CAST(json_extract(payload,'$.evidence.sample_count_7d') AS INTEGER),0) DESC, COALESCE(CAST(json_extract(payload,'$.evidence.median_latency_ms') AS REAL),1.0e99) ASC, url COLLATE NOCASE"
        )).map_err(|_| StoreError)?;
        let rows = statement
            .query_map(params![pattern, protocol_pattern], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|_| StoreError)?;

        if matched_ip.is_none() {
            let query_lower = query.to_ascii_lowercase();
            let offset = (page - 1).saturating_mul(limit);
            let end = offset.saturating_add(limit);
            let mut page_items = Vec::with_capacity(limit);
            let mut last_page_items = VecDeque::with_capacity(limit);
            let mut total = 0_u64;
            for payload in rows.flatten() {
                if payload.len() > 65_536 {
                    continue;
                }
                let Ok(endpoint) = serde_json::from_str::<Endpoint>(&payload) else {
                    continue;
                };
                let Some(endpoint) = sanitize_endpoint(endpoint, now) else {
                    continue;
                };
                if endpoint.alias_of.is_some()
                    || (!query_lower.is_empty()
                        && !endpoint.url.to_ascii_lowercase().contains(&query_lower))
                {
                    continue;
                }
                if (offset as u64..end as u64).contains(&total) {
                    page_items.push(endpoint.clone());
                }
                if last_page_items.len() == limit {
                    last_page_items.pop_front();
                }
                last_page_items.push_back(endpoint);
                total = total.saturating_add(1);
            }
            drop(statement);
            tx.commit().map_err(|_| StoreError)?;
            let last_page = total.div_ceil(limit as u64).clamp(1, 10_000) as usize;
            let page = page.min(last_page);
            let items = if page == requested_page {
                page_items
            } else {
                let last_page_len = (total - (last_page as u64 - 1) * limit as u64) as usize;
                let skip = last_page_items.len().saturating_sub(last_page_len);
                last_page_items.into_iter().skip(skip).collect()
            };
            return Ok(Page {
                items,
                total: Some(total),
                page,
                limit,
                warming,
                has_snapshot,
                matched_ip,
                ip_match_ids,
                url_ip_match_ids: HashSet::new(),
                shared_ip_count: 0,
            });
        }

        let mut decoded = Vec::new();
        for payload in rows.flatten() {
            if payload.len() > 65_536 {
                continue;
            }
            let Ok(endpoint) = serde_json::from_str::<Endpoint>(&payload) else {
                continue;
            };
            if let Some(endpoint) = sanitize_endpoint(endpoint, now) {
                decoded.push(endpoint);
            }
        }
        drop(statement);
        tx.commit().map_err(|_| StoreError)?;
        let mut url_ip_alias_ids = HashSet::new();
        let mut url_ip_match_ids = HashSet::new();
        if let Some(ip) = matched_ip.as_deref() {
            for endpoint in &decoded {
                if endpoint_url_ip_matches(&endpoint.url, ip) {
                    if let Some(canonical_id) = endpoint.alias_of.as_deref() {
                        url_ip_alias_ids.insert(canonical_id.to_owned());
                        url_ip_match_ids.insert(canonical_id.to_owned());
                    } else {
                        url_ip_match_ids.insert(endpoint.id.clone());
                    }
                }
            }
        }
        let all_items: Vec<_> = decoded
            .into_iter()
            .filter(|endpoint| {
                if endpoint.alias_of.is_some() {
                    return false;
                }
                if let Some(ip) = matched_ip.as_deref() {
                    endpoint_url_ip_matches(&endpoint.url, ip)
                        || ip_match_ids.contains(&endpoint.id)
                        || url_ip_alias_ids.contains(&endpoint.id)
                } else {
                    query.is_empty()
                        || endpoint
                            .url
                            .to_ascii_lowercase()
                            .contains(&query.to_ascii_lowercase())
                }
            })
            .collect();
        let total = all_items.len() as u64;
        let last_page = total.div_ceil(limit as u64).clamp(1, 10_000) as usize;
        let page = page.min(last_page);
        let offset = (page - 1).saturating_mul(limit);
        let items = all_items.into_iter().skip(offset).take(limit).collect();
        Ok(Page {
            items,
            total: Some(total),
            page,
            limit,
            warming,
            has_snapshot,
            matched_ip,
            shared_ip_count: ip_match_ids.union(&url_ip_match_ids).count(),
            ip_match_ids,
            url_ip_match_ids,
        })
    }

    pub fn tracker_detail(
        &self,
        id: &str,
        now: i64,
        window: &str,
        requested_page: usize,
        include_no_address: bool,
        requested_focus: Option<i64>,
    ) -> Result<TrackerDetail, StoreError> {
        if !valid_id(id) {
            return Ok(TrackerDetail {
                endpoint: None,
                log: None,
                canonical_id: None,
            });
        }
        let Some(mut db) = self.open()? else {
            return Ok(TrackerDetail {
                endpoint: None,
                log: None,
                canonical_id: None,
            });
        };
        let tx = db.transaction().map_err(|_| StoreError)?;
        let mut endpoint = Self::endpoint_from_db(&tx, id, now)?;
        if let Some(canonical_id) = endpoint.as_ref().and_then(|value| value.alias_of.clone())
            && let Some(canonical) = Self::endpoint_from_db(&tx, &canonical_id, now)?
        {
            tx.commit().map_err(|_| StoreError)?;
            return Ok(TrackerDetail {
                endpoint: Some(canonical),
                log: None,
                canonical_id: Some(canonical_id),
            });
        }
        if let Some(current) = endpoint.as_mut() {
            current.alias_of = None;
        }
        let log = if endpoint.is_some() {
            Some(Self::detail_log_in_transaction(
                &tx,
                id,
                now,
                window,
                requested_page,
                include_no_address,
                requested_focus,
            )?)
        } else {
            None
        };
        tx.commit().map_err(|_| StoreError)?;
        Ok(TrackerDetail {
            endpoint,
            log,
            canonical_id: None,
        })
    }

    #[cfg(test)]
    pub fn detail_log(
        &self,
        id: &str,
        now: i64,
        window: &str,
        requested_page: usize,
        include_no_address: bool,
        requested_focus: Option<i64>,
    ) -> Result<DetailLog, StoreError> {
        if !valid_id(id) {
            return Ok(DetailLog::default());
        }
        let Some(mut db) = self.open()? else {
            return Ok(DetailLog {
                window: (if window == "7d" { "7d" } else { "24h" }).to_owned(),
                ..DetailLog::default()
            });
        };
        let tx = db.transaction().map_err(|_| StoreError)?;
        let detail = Self::detail_log_in_transaction(
            &tx,
            id,
            now,
            window,
            requested_page,
            include_no_address,
            requested_focus,
        )?;
        tx.commit().map_err(|_| StoreError)?;
        Ok(detail)
    }

    fn detail_log_in_transaction(
        tx: &Transaction<'_>,
        id: &str,
        now: i64,
        window: &str,
        requested_page: usize,
        include_no_address: bool,
        requested_focus: Option<i64>,
    ) -> Result<DetailLog, StoreError> {
        let (window, span) = match window {
            "7d" => ("7d", 7 * 86_400),
            _ => ("24h", 86_400),
        };
        let url: Option<String> = tx
            .query_row("SELECT url FROM endpoints WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|_| StoreError)?;
        let Some(protocol) = url
            .as_deref()
            .and_then(public_tracker_url)
            .map(|url| url.scheme().to_owned())
        else {
            return Ok(DetailLog {
                window: window.to_owned(),
                ..DetailLog::default()
            });
        };
        let start = now.saturating_sub(span);
        let event_clause = "CASE WHEN json_valid(payload) AND length(CAST(payload AS BLOB))<=4096 THEN json_type(payload,'$.observed_at')='integer' AND json_extract(payload,'$.observed_at')=observed_at AND json_type(payload,'$.endpoint_id')='text' AND json_extract(payload,'$.endpoint_id')=endpoint_id AND json_type(payload,'$.family')='text' AND json_extract(payload,'$.family')=family AND family IN ('ipv4','ipv6') AND json_extract(payload,'$.stage')='announce' AND json_type(payload,'$.result')='text' AND (json_type(payload,'$.protocol_reply') IS NULL OR json_type(payload,'$.protocol_reply') IN ('true','false')) AND (json_type(payload,'$.http_status') IS NULL OR json_type(payload,'$.http_status')='null' OR (json_type(payload,'$.http_status')='integer' AND json_extract(payload,'$.http_status') BETWEEN 100 AND 599)) AND (json_type(payload,'$.latency_ms') IS NULL OR json_type(payload,'$.latency_ms') IN ('null','integer','real')) AND (json_type(payload,'$.error_category') IS NULL OR json_type(payload,'$.error_category') IN ('null','text')) AND (json_type(payload,'$.target_ip') IS NULL OR json_type(payload,'$.target_ip') IN ('null','text')) ELSE 0 END";
        let no_address_clause = "(COALESCE(CASE WHEN json_valid(payload) THEN json_extract(payload,'$.result') END,'')='unsupported' OR COALESCE(CASE WHEN json_valid(payload) THEN json_extract(payload,'$.error_category') END,'') IN ('family_unavailable','unsupported'))";

        let mut focus_id = None;
        let mut focus_rank = None;
        let mut effective_include_no_address = include_no_address;
        if let Some(candidate) = requested_focus.filter(|value| *value > 0) {
            let focus: Option<(i64, bool)> = tx
                .query_row(
                    &format!("SELECT observed_at,{no_address_clause} FROM observations WHERE id=?1 AND endpoint_id=?2 AND observed_at>=?3 AND observed_at<=?4 AND {event_clause}"),
                    params![candidate, id, start, now],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|_| StoreError)?;
            if let Some((at, is_no_address)) = focus {
                if is_no_address {
                    effective_include_no_address = true;
                }
                let rank: u64 = tx
                    .query_row(
                        &format!("SELECT COUNT(*) FROM observations WHERE endpoint_id=?1 AND observed_at>=?2 AND observed_at<=?3 AND {event_clause} AND (?4=1 OR NOT {no_address_clause}) AND (observed_at>?5 OR (observed_at=?5 AND id>=?6))"),
                        params![id, start, now, effective_include_no_address, at, candidate],
                        |row| row.get(0),
                    )
                    .map_err(|_| StoreError)?;
                if rank > 0 {
                    focus_id = Some(candidate);
                    focus_rank = Some(rank);
                }
            }
        }

        let (total, hidden_no_address): (u64, u64) = tx
            .query_row(
                &format!("SELECT COALESCE(SUM(CASE WHEN {no_address_clause} THEN 1 ELSE 0 END),0), COALESCE(SUM(CASE WHEN (?4=1 OR NOT {no_address_clause}) THEN 1 ELSE 0 END),0) FROM observations WHERE endpoint_id=?1 AND observed_at>=?2 AND observed_at<=?3 AND {event_clause}"),
                params![id, start, now, effective_include_no_address],
                |row| Ok((row.get(1)?, row.get(0)?)),
            )
            .map_err(|_| StoreError)?;
        let pages = total.div_ceil(50).max(1);
        let page = focus_rank
            .map(|rank| (rank.saturating_sub(1) / 50 + 1) as usize)
            .unwrap_or(requested_page.clamp(1, 10_000))
            .min(pages as usize);
        let offset = (page - 1).saturating_mul(50);
        let mut statement = tx
            .prepare(&format!("SELECT id,payload FROM observations WHERE endpoint_id=?1 AND observed_at>=?2 AND observed_at<=?3 AND {event_clause} AND (?4=1 OR NOT {no_address_clause}) ORDER BY observed_at DESC,id DESC LIMIT 50 OFFSET ?5"))
            .map_err(|_| StoreError)?;
        let rows = statement
            .query_map(
                params![id, start, now, effective_include_no_address, offset],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|_| StoreError)?;
        let mut items = Vec::new();
        for (event_id, payload) in rows.flatten() {
            let Ok(observation) = serde_json::from_str::<Observation>(&payload) else {
                continue;
            };
            if let Some(mut observation) = sanitize_observation(observation, id, now, span) {
                if protocol == "udp" {
                    observation.http_status = None;
                }
                items.push(LogEntry {
                    id: event_id,
                    observation,
                });
            }
        }
        drop(statement);

        let mut statement = tx
            .prepare(&format!("SELECT id,payload FROM observations WHERE endpoint_id=?1 AND observed_at>=?2 AND observed_at<=?3 AND {event_clause} ORDER BY observed_at DESC,id DESC LIMIT 500"))
            .map_err(|_| StoreError)?;
        let rows = statement
            .query_map(params![id, now.saturating_sub(86_400), now], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|_| StoreError)?;
        let mut timeline = Vec::new();
        for (event_id, payload) in rows.flatten() {
            let Ok(observation) = serde_json::from_str::<Observation>(&payload) else {
                continue;
            };
            if let Some(mut observation) = sanitize_observation(observation, id, now, 86_400) {
                if protocol == "udp" {
                    observation.http_status = None;
                }
                timeline.push(LogEntry {
                    id: event_id,
                    observation,
                });
            }
        }
        drop(statement);
        Ok(DetailLog {
            items,
            timeline,
            total,
            page,
            pages,
            window: window.to_owned(),
            include_no_address: effective_include_no_address,
            hidden_no_address,
            focus_id,
        })
    }

    pub fn endpoint(&self, id: &str, now: i64) -> Result<Option<Endpoint>, StoreError> {
        if !valid_id(id) {
            return Ok(None);
        }
        let Some(db) = self.open()? else {
            return Ok(None);
        };
        Self::endpoint_from_db(&db, id, now)
    }

    fn endpoint_from_db(
        db: &Connection,
        id: &str,
        now: i64,
    ) -> Result<Option<Endpoint>, StoreError> {
        let payload: Option<String> = db.query_row(
            "SELECT payload FROM endpoints WHERE id=?1 AND length(CAST(payload AS BLOB))<=65536", [id], |r| r.get(0),
        ).optional().map_err(|_| StoreError)?;
        Ok(payload
            .filter(|p| p.len() <= 65_536)
            .and_then(|p| serde_json::from_str::<Endpoint>(&p).ok())
            .and_then(|ep| sanitize_endpoint(ep, now)))
    }

    pub fn endpoint_observations(
        &self,
        id: &str,
        family: Option<&str>,
        stage: Option<&str>,
        now: i64,
        limit: usize,
    ) -> Result<Option<Vec<Observation>>, StoreError> {
        if !valid_id(id) {
            return Ok(None);
        }
        let Some(mut db) = self.open()? else {
            return Ok(None);
        };
        let tx = db.transaction().map_err(|_| StoreError)?;
        if Self::endpoint_from_db(&tx, id, now)?.is_none() {
            tx.commit().map_err(|_| StoreError)?;
            return Ok(None);
        }
        let observations = Self::observations_from_db(&tx, id, family, stage, now, limit)?;
        tx.commit().map_err(|_| StoreError)?;
        Ok(Some(observations))
    }

    fn observations_from_db(
        db: &Connection,
        id: &str,
        family: Option<&str>,
        stage: Option<&str>,
        now: i64,
        limit: usize,
    ) -> Result<Vec<Observation>, StoreError> {
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
            if let Some(mut observation) = sanitize_observation(observation, id, now, 86_400) {
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

/// The summary document, read from an already opened read-only connection.
fn native_summary_from(db: &Connection, now: i64) -> Result<Option<serde_json::Value>, StoreError> {
    if !has_table(db, "stats_history")? {
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
    // The window keys are always present, zeros included: the page reads them
    // unconditionally once a summary exists.
    window_metrics(db, now)?.insert_into(&mut result);
    Ok(Some(serde_json::Value::Object(result)))
}

/// Trailing window for the published announce counters.
const SUMMARY_WINDOW_SECS: i64 = 86_400;
/// A wider spacing between two neighbouring buckets means the sampler was down.
const SUMMARY_MAX_GAP_SECS: u64 = 300;
/// One bucket per minute plus headroom: the window read is always bounded.
const SUMMARY_WINDOW_LIMIT: i64 = 2_000;

/// The published counters the window reports, in [`WindowMetrics`] slot order.
const SUMMARY_COUNTERS: [&str; 4] = [
    "accepted_announces",
    "http_announces",
    "udp_announces",
    "accepted_ipv6",
];

/// The trailing window, accumulated from adjacent published buckets.
///
/// The stored counters are process-lifetime totals, so one point says nothing
/// about the last day: only positive deltas between two neighbouring buckets of
/// the same observatory instance are added up, and the coverage is the time
/// those usable pairs actually span.
#[derive(Debug, Default, Clone, Copy)]
struct WindowMetrics {
    announces: u64,
    http_announces: u64,
    udp_announces: u64,
    accepted_ipv6: u64,
    covered_seconds: u64,
}

impl WindowMetrics {
    fn add_pair(&mut self, gap: u64, deltas: [u64; 4]) {
        let [announces, http, udp, ipv6] = deltas;
        self.announces = self.announces.saturating_add(announces);
        self.http_announces = self.http_announces.saturating_add(http);
        self.udp_announces = self.udp_announces.saturating_add(udp);
        self.accepted_ipv6 = self.accepted_ipv6.saturating_add(ipv6);
        self.covered_seconds = self.covered_seconds.saturating_add(gap);
    }

    /// Announces per covered second, rounded to one decimal place.
    fn rate(&self) -> f64 {
        let covered = self.covered_seconds.max(1) as f64;
        (self.announces as f64 / covered * 10.0).round() / 10.0
    }

    fn insert_into(&self, result: &mut serde_json::Map<String, serde_json::Value>) {
        for (key, value) in [
            ("announces_24h", self.announces),
            ("http_announces_24h", self.http_announces),
            ("udp_announces_24h", self.udp_announces),
            ("accepted_ipv6_24h", self.accepted_ipv6),
            ("covered_seconds_24h", self.covered_seconds),
        ] {
            result.insert(key.to_owned(), serde_json::json!(value));
        }
        result.insert("rate_24h".to_owned(), serde_json::json!(self.rate()));
    }
}

/// The increment one pair of adjacent buckets contributes, or `None` when the
/// pair cannot be used: another instance, a sampling gap, a repeated or reversed
/// timestamp, or a counter that went backwards instead of forwards.
fn window_pair(prev: &StatsPoint, next: &StatsPoint) -> Option<(u64, [u64; 4])> {
    if prev.instance != next.instance {
        return None;
    }
    let gap = u64::try_from(next.at.checked_sub(prev.at)?).ok()?;
    if !(1..=SUMMARY_MAX_GAP_SECS).contains(&gap) {
        return None;
    }
    let mut deltas = [0u64; 4];
    for (delta, key) in deltas.iter_mut().zip(SUMMARY_COUNTERS) {
        let before = prev.counters.get(key);
        let after = next.counters.get(key);
        let (Some(before), Some(after)) = (before, after) else {
            // A counter published by only one side has no usable delta.
            continue;
        };
        if *after < *before {
            // The counter was reset inside this pair: neither the increment nor
            // the elapsed time can be attributed to it.
            return None;
        }
        *delta = *after - *before;
    }
    Some((gap, deltas))
}

/// Sums the trailing window in one bounded read of `stats_history`.
fn window_metrics(db: &Connection, now: i64) -> Result<WindowMetrics, StoreError> {
    let start = now.saturating_sub(SUMMARY_WINDOW_SECS);
    let mut statement = db.prepare("SELECT payload FROM stats_history WHERE bucket>=?1 AND length(CAST(payload AS BLOB))<=16384 ORDER BY bucket ASC LIMIT ?2").map_err(|_| StoreError)?;
    let rows = statement
        .query_map(params![start, SUMMARY_WINDOW_LIMIT], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|_| StoreError)?;
    let points: Vec<StatsPoint> = rows
        .flatten()
        .filter_map(|payload| serde_json::from_str::<StatsPoint>(&payload).ok())
        .filter_map(|point| sanitize_point(point, start, now))
        .collect();
    let mut metrics = WindowMetrics::default();
    for pair in points.windows(2) {
        if let Some((gap, deltas)) = window_pair(&pair[0], &pair[1]) {
            metrics.add_pair(gap, deltas);
        }
    }
    Ok(metrics)
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

/// A resolved tracker address may be published only when it is a public IP.
/// Canonicalise the text so IPv6 representations stay comparable.
fn public_ip_text(value: &str) -> Option<String> {
    use std::net::IpAddr;
    match value.parse::<IpAddr>().ok()? {
        IpAddr::V4(ip) => public_ipv4(ip).then(|| IpAddr::V4(ip).to_string()),
        IpAddr::V6(ip) => public_ipv6(ip).then(|| IpAddr::V6(ip).to_string()),
    }
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

fn public_search_ip(query: &str) -> Option<String> {
    let raw = query
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(query);
    match raw.parse::<IpAddr>().ok()? {
        IpAddr::V4(ip) => public_ipv4(ip).then(|| IpAddr::V4(ip).to_string()),
        IpAddr::V6(ip) => public_ipv6(ip).then(|| IpAddr::V6(ip).to_string()),
    }
}

fn endpoint_url_ip_matches(raw_url: &str, expected: &str) -> bool {
    public_tracker_url(raw_url)
        .and_then(|url| match url.host()? {
            url::Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
            url::Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
            url::Host::Domain(_) => None,
        })
        .is_some_and(|ip| ip.to_string() == expected)
}

fn latest_target_endpoints(
    db: &Connection,
    now: i64,
    target_ip: &str,
) -> Result<HashSet<String>, StoreError> {
    let mut statement = db
        .prepare(
            "SELECT recent.endpoint_id FROM (SELECT endpoint_id,family,payload,ROW_NUMBER() OVER (PARTITION BY endpoint_id,family ORDER BY observed_at DESC,id DESC) AS position FROM observations WHERE observed_at>=?1 AND observed_at<=?2 AND length(CAST(payload AS BLOB))<=4096 AND json_valid(payload) AND json_extract(payload,'$.stage')='announce') AS recent JOIN endpoints AS endpoint ON endpoint.id=recent.endpoint_id WHERE recent.position=1 AND json_extract(recent.payload,'$.target_ip')=?3 AND json_valid(endpoint.payload) AND json_extract(endpoint.payload,'$.alias_of') IS NULL",
        )
        .map_err(|_| StoreError)?;
    let rows = statement
        .query_map(
            params![now.saturating_sub(7 * 86_400), now, target_ip],
            |row| row.get::<_, String>(0),
        )
        .map_err(|_| StoreError)?;
    Ok(rows.flatten().collect())
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
                | "handshake_failed"
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
    endpoint.alias_of = endpoint
        .alias_of
        .filter(|id| valid_id(id) && *id != endpoint.id);
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

fn sanitize_observation(
    mut observation: Observation,
    id: &str,
    now: i64,
    span: i64,
) -> Option<Observation> {
    if observation.endpoint_id != id
        || !matches!(observation.family.as_str(), "ipv4" | "ipv6")
        || !(now.saturating_sub(span)..=now).contains(&observation.observed_at)
    {
        return None;
    }
    observation.target_ip = observation.target_ip.as_deref().and_then(public_ip_text);
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

#[cfg(test)]
mod window_tests {
    use super::*;

    /// A minute boundary, so the published bucket of a test point is `at`.
    const NOW: i64 = 1_700_000_040;

    type SqlResult = rusqlite::Result<()>;

    fn memory_db() -> rusqlite::Result<Connection> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE stats_history(bucket INTEGER PRIMARY KEY, instance TEXT NOT NULL, payload TEXT NOT NULL);",
        )?;
        Ok(db)
    }

    /// Publishes one bucket; `counters` is `[accepted, http, udp, ipv6]`.
    fn put_point(db: &Connection, at: i64, boot: &str, counters: [u64; 4]) -> SqlResult {
        let [accepted, http, udp, ipv6] = counters;
        let payload = serde_json::json!({
            "at": at,
            "instance": boot,
            "gauges": {"peers": 2.0, "swarms": 3.0},
            "counters": {
                "accepted_announces": accepted,
                "accepted_ipv4": accepted.saturating_sub(ipv6),
                "accepted_ipv6": ipv6,
                "http_announces": http,
                "udp_announces": udp,
                "business_failures": 0,
            },
        });
        db.execute(
            "INSERT INTO stats_history VALUES(?1,?2,?3)",
            params![at - at.rem_euclid(60), boot, payload.to_string()],
        )?;
        Ok(())
    }

    fn field(summary: &serde_json::Value, key: &str) -> serde_json::Value {
        summary.get(key).cloned().unwrap_or(serde_json::Value::Null)
    }

    fn summary_of(db: &Connection, now: i64) -> anyhow::Result<serde_json::Value> {
        let value = match native_summary_from(db, now) {
            Ok(value) => value,
            Err(error) => anyhow::bail!("{error:?}"),
        };
        value.ok_or_else(|| anyhow::anyhow!("no summary for this window"))
    }

    fn metrics_of(db: &Connection, now: i64) -> anyhow::Result<WindowMetrics> {
        match window_metrics(db, now) {
            Ok(metrics) => Ok(metrics),
            Err(error) => anyhow::bail!("{error:?}"),
        }
    }

    #[test]
    fn continuous_buckets_sum_deltas_and_real_coverage() -> anyhow::Result<()> {
        let db = memory_db()?;
        // Older than the window: never fetched, never counted.
        put_point(&db, NOW - 172_800, "boot-a", [9_000, 4_000, 5_000, 900])?;
        put_point(&db, NOW - 120, "boot-a", [100, 40, 60, 10])?;
        put_point(&db, NOW - 60, "boot-a", [130, 50, 80, 13])?;
        put_point(&db, NOW, "boot-a", [175, 70, 105, 18])?;
        let summary = summary_of(&db, NOW)?;
        assert_eq!(field(&summary, "announces_24h").as_u64(), Some(75));
        assert_eq!(field(&summary, "http_announces_24h").as_u64(), Some(30));
        assert_eq!(field(&summary, "udp_announces_24h").as_u64(), Some(45));
        assert_eq!(field(&summary, "accepted_ipv6_24h").as_u64(), Some(8));
        assert_eq!(field(&summary, "covered_seconds_24h").as_u64(), Some(120));
        assert_eq!(field(&summary, "rate_24h").as_f64(), Some(0.6));
        // The pre-existing keys keep their "since process start" meaning.
        assert_eq!(field(&summary, "announces").as_u64(), Some(175));
        assert_eq!(field(&summary, "sampled_at").as_u64(), Some(1_700_000_040));
        assert_eq!(field(&summary, "peers").as_u64(), Some(2));
        Ok(())
    }

    #[test]
    fn instance_change_skips_the_pair_without_inflating_totals() -> anyhow::Result<()> {
        let db = memory_db()?;
        put_point(&db, NOW - 180, "boot-a", [10, 4, 6, 1])?;
        put_point(&db, NOW - 120, "boot-a", [40, 15, 25, 3])?;
        // A restart publishes a new instance whose counters are already larger:
        // the pair must be dropped instead of producing a 460 announce jump.
        put_point(&db, NOW - 60, "boot-b", [500, 100, 400, 5])?;
        put_point(&db, NOW, "boot-b", [560, 120, 440, 7])?;
        let summary = summary_of(&db, NOW)?;
        assert_eq!(field(&summary, "announces_24h").as_u64(), Some(90));
        assert_eq!(field(&summary, "http_announces_24h").as_u64(), Some(31));
        assert_eq!(field(&summary, "udp_announces_24h").as_u64(), Some(59));
        assert_eq!(field(&summary, "accepted_ipv6_24h").as_u64(), Some(4));
        assert_eq!(field(&summary, "covered_seconds_24h").as_u64(), Some(120));
        Ok(())
    }

    #[test]
    fn empty_and_single_point_windows_still_publish_zero_keys() -> anyhow::Result<()> {
        let db = memory_db()?;
        let mut keys = serde_json::Map::new();
        metrics_of(&db, NOW)?.insert_into(&mut keys);
        for key in [
            "announces_24h",
            "http_announces_24h",
            "udp_announces_24h",
            "accepted_ipv6_24h",
            "covered_seconds_24h",
        ] {
            assert_eq!(keys.get(key).and_then(|value| value.as_u64()), Some(0));
        }
        assert_eq!(
            keys.get("rate_24h").and_then(|value| value.as_f64()),
            Some(0.0)
        );
        // One point has no neighbour to diff against, but the keys must exist.
        put_point(&db, NOW - 30, "boot-a", [9, 3, 6, 1])?;
        let summary = summary_of(&db, NOW)?;
        assert_eq!(field(&summary, "announces_24h").as_u64(), Some(0));
        assert_eq!(field(&summary, "covered_seconds_24h").as_u64(), Some(0));
        assert_eq!(field(&summary, "rate_24h").as_f64(), Some(0.0));
        assert_eq!(field(&summary, "announces").as_u64(), Some(9));
        Ok(())
    }

    #[test]
    fn gaps_over_five_minutes_add_neither_counters_nor_coverage() -> anyhow::Result<()> {
        let db = memory_db()?;
        put_point(&db, NOW - 780, "boot-a", [10, 4, 6, 1])?;
        put_point(&db, NOW - 720, "boot-a", [30, 10, 20, 2])?;
        // Exactly 300s is still one usable pair.
        put_point(&db, NOW - 420, "boot-a", [50, 20, 30, 3])?;
        // 360s is a gap: that pair contributes nothing at all.
        put_point(&db, NOW - 60, "boot-a", [90, 40, 50, 6])?;
        put_point(&db, NOW, "boot-a", [110, 55, 55, 9])?;
        let summary = summary_of(&db, NOW)?;
        assert_eq!(field(&summary, "announces_24h").as_u64(), Some(60));
        assert_eq!(field(&summary, "http_announces_24h").as_u64(), Some(31));
        assert_eq!(field(&summary, "covered_seconds_24h").as_u64(), Some(420));
        assert_eq!(field(&summary, "rate_24h").as_f64(), Some(0.1));
        Ok(())
    }

    #[test]
    fn counter_regression_is_skipped_instead_of_wrapping() -> anyhow::Result<()> {
        let db = memory_db()?;
        put_point(&db, NOW - 180, "boot-a", [100, 40, 60, 10])?;
        put_point(&db, NOW - 120, "boot-a", [150, 60, 90, 14])?;
        // Same instance, counters went backwards: the pair is not a delta.
        put_point(&db, NOW - 60, "boot-a", [20, 5, 15, 1])?;
        put_point(&db, NOW, "boot-a", [60, 15, 45, 4])?;
        let summary = summary_of(&db, NOW)?;
        assert_eq!(field(&summary, "announces_24h").as_u64(), Some(90));
        assert_eq!(field(&summary, "covered_seconds_24h").as_u64(), Some(120));
        Ok(())
    }

    #[test]
    fn window_keys_never_expose_unlisted_counters_or_gauges() -> anyhow::Result<()> {
        let db = memory_db()?;
        let point = serde_json::json!({
            "at": NOW - 60,
            "instance": "boot-a",
            "gauges": {"peers": 1.0, "secret_gauge": 7.0},
            "counters": {"accepted_announces": 5, "secret_counter": 9},
        });
        db.execute(
            "INSERT INTO stats_history VALUES(?1,?2,?3)",
            params![NOW - 60, "boot-a", point.to_string()],
        )?;
        let summary = summary_of(&db, NOW)?;
        assert!(summary.get("secret_counter").is_none());
        assert!(summary.get("secret_gauge").is_none());
        assert_eq!(field(&summary, "announces_24h").as_u64(), Some(0));
        Ok(())
    }
}
