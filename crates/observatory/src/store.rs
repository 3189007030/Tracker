use anyhow::Result;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::path::Path;
use tracker_model::{
    Endpoint, Evidence, FamilyStatus, Infrastructure, Observation, Overview, PUBLIC_SCHEMA,
    PoolStatus, StatsPoint, VERSION, unix_now,
};
use url::Url;

pub struct Store {
    private: Connection,
    public: Connection,
}
pub struct ActivityEvidence<'a> {
    pub endpoint: &'a str,
    pub source: &'a str,
    pub sample: &'a crate::samples::Sample,
    pub peers: &'a [std::net::SocketAddr],
    pub bitmaps: u64,
    pub coverage: Option<f64>,
    pub stages: &'a [Observation],
}

fn public_sample_id(hash: &[u8; 20]) -> String {
    hex::encode(Sha256::digest(hash))
}
fn pool_has_success(
    samples: &[crate::pool::SampleRecord],
    sources: &[crate::pool::PoolStatus],
    forum: bool,
) -> bool {
    samples.iter().any(|sample| {
        (sample.pool == crate::pool::PoolKind::Forum) == forum
            && sources
                .iter()
                .any(|source| source.source == sample.source && source.status == "ok")
    })
}
fn publisher(source: crate::pool::Source) -> &'static str {
    use crate::pool::Source::*;
    match source {
        Debian => "debian",
        Ubuntu => "ubuntu",
        Fedora => "fedora",
        Arch => "arch",
        Hjd => "hjd",
        FbyLt1 | Fby418 => "fby",
        TpbRecent | TpbAll => "tpb",
    }
}

pub fn normalize(raw: &str) -> Option<String> {
    if raw.len() > 512 || raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let u = Url::parse(raw.trim()).ok()?;
    if !matches!(u.scheme(), "udp" | "http" | "https")
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || u.path().is_empty()
        || u.host_str().is_none()
    {
        return None;
    }
    let host = u
        .host_str()?
        .trim_matches(['[', ']'])
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.contains("durukanbal.com") {
        return None;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if !crate::peer::public_ip(ip) {
            return None;
        }
    } else if !host.contains('.')
        || [".local", ".localhost", ".lan", ".internal"]
            .iter()
            .any(|suffix| host.ends_with(*suffix))
    {
        return None;
    }
    let decoded = percent_encoding::percent_decode_str(u.path())
        .decode_utf8()
        .ok()?;
    let path = decoded.to_ascii_lowercase();
    if path.chars().any(char::is_control)
        || ["passkey", "auth", "token", "secret", "apikey", "api_key"]
            .iter()
            .any(|part| path.contains(*part))
        || path
            .split('/')
            .any(|s| s.len() >= 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return None;
    }
    Some(u.to_string())
}
fn id(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}
fn canonical_host(host: &str) -> Option<String> {
    url::Host::parse(host.trim_end_matches('.'))
        .ok()
        .map(|h| h.to_string().to_ascii_lowercase())
}
fn protocol_reply(o: &Observation) -> bool {
    o.protocol_reply
}
fn attempted_protocol(o: &Observation) -> bool {
    o.result == "accepted" || o.result == "failed" || o.protocol_reply
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let private = Connection::open(dir.join("private.sqlite3"))?;
        let public = Connection::open(dir.join("public.sqlite3"))?;
        public.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        private.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY,v TEXT NOT NULL); CREATE TABLE IF NOT EXISTS endpoints(id TEXT PRIMARY KEY,service_id TEXT NOT NULL,url TEXT NOT NULL UNIQUE,first_seen INTEGER NOT NULL,baseline INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS observations(id INTEGER PRIMARY KEY AUTOINCREMENT,endpoint_id TEXT NOT NULL,observed_at INTEGER NOT NULL,family TEXT NOT NULL,payload TEXT NOT NULL); CREATE INDEX IF NOT EXISTS observations_lookup ON observations(endpoint_id,observed_at); CREATE TABLE IF NOT EXISTS daily(endpoint_id TEXT NOT NULL, day INTEGER NOT NULL, family TEXT NOT NULL, checks INTEGER NOT NULL, successes INTEGER NOT NULL, replies INTEGER NOT NULL, PRIMARY KEY(endpoint_id,day,family)); CREATE TABLE IF NOT EXISTS samples(id INTEGER PRIMARY KEY AUTOINCREMENT,endpoint_id TEXT NOT NULL,sample_id TEXT NOT NULL,source TEXT NOT NULL,observed_at INTEGER NOT NULL,handshakes INTEGER NOT NULL,bitmap_observations INTEGER NOT NULL,coverage REAL); CREATE TABLE IF NOT EXISTS pauses(host TEXT PRIMARY KEY, until INTEGER NOT NULL);")?;
        private.execute_batch("CREATE TABLE IF NOT EXISTS sample_catalog(sample_id TEXT PRIMARY KEY,source TEXT NOT NULL,info_hash BLOB NOT NULL,piece_count INTEGER NOT NULL,piece_length INTEGER NOT NULL,total_length INTEGER NOT NULL,observed_at INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS observed_peers(endpoint_id TEXT NOT NULL,sample_id TEXT NOT NULL,peer_key TEXT NOT NULL,observed_at INTEGER NOT NULL,PRIMARY KEY(endpoint_id,sample_id,peer_key)); CREATE INDEX IF NOT EXISTS sample_endpoint_time ON samples(endpoint_id,observed_at); CREATE INDEX IF NOT EXISTS sample_lookup ON samples(endpoint_id,sample_id,observed_at); CREATE INDEX IF NOT EXISTS observed_peers_time ON observed_peers(endpoint_id,observed_at); CREATE INDEX IF NOT EXISTS sample_age ON samples(observed_at); CREATE INDEX IF NOT EXISTS observed_peers_age ON observed_peers(observed_at); CREATE INDEX IF NOT EXISTS observations_age ON observations(observed_at); CREATE INDEX IF NOT EXISTS observation_revision ON observations(endpoint_id,id DESC);")?;
        private.execute_batch(
            "CREATE INDEX IF NOT EXISTS sample_revision ON samples(endpoint_id,id DESC);",
        )?;
        private.execute_batch("CREATE INDEX IF NOT EXISTS health_endpoint_time ON observations(endpoint_id,observed_at) WHERE json_extract(payload,'$.stage')='announce';")?;
        private.execute_batch("CREATE TABLE IF NOT EXISTS announce_due(endpoint_id TEXT NOT NULL,family TEXT NOT NULL,resource TEXT NOT NULL,until INTEGER NOT NULL,PRIMARY KEY(endpoint_id,family,resource)); CREATE INDEX IF NOT EXISTS announce_due_expiry ON announce_due(until);")?;
        private.execute_batch("CREATE TABLE IF NOT EXISTS activity_cursor(endpoint_id TEXT PRIMARY KEY,last_attempt INTEGER NOT NULL); CREATE INDEX IF NOT EXISTS activity_cursor_order ON activity_cursor(last_attempt);")?;
        private.execute_batch("CREATE TABLE IF NOT EXISTS endpoint_alias(alias_id TEXT PRIMARY KEY,canonical_id TEXT NOT NULL,target_ip TEXT NOT NULL,updated_at INTEGER NOT NULL);")?;
        private.execute_batch("CREATE TABLE IF NOT EXISTS infrastructure(host TEXT PRIMARY KEY,payload TEXT NOT NULL,updated_at INTEGER NOT NULL); CREATE INDEX IF NOT EXISTS infrastructure_age ON infrastructure(updated_at);")?;
        private.execute_batch("CREATE TABLE IF NOT EXISTS pool_registry(sample_id TEXT PRIMARY KEY,hash BLOB NOT NULL,pool TEXT NOT NULL,source TEXT NOT NULL,payload TEXT NOT NULL,active INTEGER NOT NULL,updated_at INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS native_history(bucket INTEGER PRIMARY KEY,instance TEXT NOT NULL,payload TEXT NOT NULL); DROP TABLE IF EXISTS probe_traces;")?;
        {
            let mut columns = private.prepare("PRAGMA table_info(samples)")?;
            let names = columns
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
            drop(columns);
            for name in [
                "announce_result",
                "announce_error",
                "handshake_result",
                "handshake_error",
            ] {
                if !names.contains(name) {
                    private.execute(&format!("ALTER TABLE samples ADD COLUMN {name} TEXT"), [])?;
                }
            }
        }
        private.execute(
            "INSERT OR IGNORE INTO meta(k,v) VALUES ('peer_salt',?1)",
            [hex::encode(rand::random::<[u8; 32]>())],
        )?;
        public.execute_batch(PUBLIC_SCHEMA)?;
        // Upgrade old public databases in place; aggregate evidence and history
        // remain intact while sample identities and per-node traces disappear.
        public.execute_batch("PRAGMA secure_delete=ON; BEGIN; DROP TABLE IF EXISTS public_runs; DROP TABLE IF EXISTS public_samples; COMMIT;")?;
        public.execute_batch(
            "CREATE TABLE IF NOT EXISTS publication_meta(k TEXT PRIMARY KEY,v INTEGER NOT NULL);",
        )?;
        public.execute_batch("CREATE INDEX IF NOT EXISTS public_observations_age ON observations(observed_at); CREATE TABLE IF NOT EXISTS publication_cache(endpoint_id TEXT PRIMARY KEY,event_revision INTEGER NOT NULL,sample_revision INTEGER NOT NULL,next_refresh INTEGER NOT NULL,service_id TEXT NOT NULL,colocated INTEGER NOT NULL,all_eligible INTEGER NOT NULL,interval_sec INTEGER NOT NULL);")?;
        // Rebuild alias policy on the first publication after a restart, even
        // when an earlier process refreshed it less than ten minutes ago.
        private.execute("DELETE FROM meta WHERE k='alias_checked_at'", [])?;
        Ok(Self { private, public })
    }
    pub fn ingest(
        &mut self,
        urls: &[String],
        aliases: &std::collections::BTreeMap<String, String>,
    ) -> Result<usize> {
        let now = unix_now();
        let baseline = !self.private.query_row(
            "SELECT EXISTS(SELECT 1 FROM meta WHERE k='baseline')",
            [],
            |r| r.get::<_, bool>(0),
        )?;
        let tx = self.private.transaction()?;
        let mut total: i64 = tx.query_row("SELECT COUNT(*) FROM endpoints", [], |r| r.get(0))?;
        let mut added = 0;
        for raw in urls {
            let Some(url) = normalize(raw) else {
                continue;
            };
            let parsed = Url::parse(&url)?;
            let Some(host) = parsed.host_str().and_then(canonical_host) else {
                continue;
            };
            // Aliases must be explicit; never merge based on shared DNS addresses.
            let service = aliases
                .iter()
                .find_map(|(alias, canonical)| {
                    (canonical_host(alias).as_deref() == Some(host.as_str()))
                        .then(|| canonical_host(canonical))
                        .flatten()
                })
                .unwrap_or(host);
            if total < 20_000 {
                let inserted = tx.execute(
                    "INSERT OR IGNORE INTO endpoints VALUES (?1,?2,?3,?4,?5)",
                    params![id(&url), id(&service), url, now, baseline as i64],
                )?;
                added += inserted;
                total += inserted as i64;
            }
            tx.execute(
                "UPDATE endpoints SET service_id=?1 WHERE id=?2",
                params![id(&service), id(&url)],
            )?;
        }
        if added > 0 {
            // Newly discovered canonical or legacy endpoints can change aliases.
            tx.execute("DELETE FROM meta WHERE k='alias_checked_at'", [])?;
        }
        tx.commit()?;
        Ok(added)
    }
    pub fn finish_baseline(&self) -> Result<()> {
        self.private.execute("INSERT OR IGNORE INTO meta SELECT 'baseline','complete' WHERE EXISTS(SELECT 1 FROM endpoints)",[])?;
        Ok(())
    }
    pub fn targets(&self) -> Result<Vec<(String, String, Option<i64>)>> {
        let mut q = self.private.prepare("SELECT e.id,e.url,MAX(o.observed_at) FROM endpoints e LEFT JOIN observations o ON o.endpoint_id=e.id AND json_extract(o.payload,'$.stage')='announce' WHERE NOT EXISTS (SELECT 1 FROM endpoint_alias a WHERE a.alias_id=e.id) GROUP BY e.id ORDER BY MAX(o.observed_at) ASC LIMIT 256")?;
        let rows = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    /// One eligible family per endpoint, with the earliest due jobs first.
    pub fn due_targets(&self, now: i64) -> Result<Vec<(String, String, bool)>> {
        let mut q = self.private.prepare(
            "WITH due AS (
                SELECT e.id, e.url,
                    CASE WHEN COALESCE(v4.until, 0) <= ?1 THEN COALESCE(v4.until, 0) END AS ipv4,
                    CASE WHEN COALESCE(v6.until, 0) <= ?1 THEN COALESCE(v6.until, 0) END AS ipv6
                FROM endpoints e
                LEFT JOIN announce_due v4 ON v4.endpoint_id=e.id AND v4.family='ipv4' AND v4.resource='health'
                LEFT JOIN announce_due v6 ON v6.endpoint_id=e.id AND v6.family='ipv6' AND v6.resource='health'
                WHERE NOT EXISTS (SELECT 1 FROM endpoint_alias a WHERE a.alias_id=e.id)
            )
            SELECT id, url, ipv4 IS NULL OR (ipv6 IS NOT NULL AND ipv6 < ipv4)
            FROM due WHERE ipv4 IS NOT NULL OR ipv6 IS NOT NULL
            ORDER BY COALESCE(MIN(ipv4, ipv6), ipv4, ipv6), id LIMIT 20000",
        )?;
        let rows = q
            .query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    pub fn health_wake_delay(&self, now: i64) -> Result<u64> {
        let horizon = now.saturating_add(60);
        let mut earliest = horizon;
        let mut check = self.private.prepare_cached(
            "SELECT MAX(
                COALESCE((SELECT until FROM announce_due WHERE endpoint_id=?1 AND family=?2 AND resource='health'), 0),
                COALESCE((SELECT until FROM pauses WHERE host=?3), 0)
            )",
        )?;
        for (endpoint, raw, ipv6) in self.due_targets(horizon)? {
            let url = Url::parse(&raw)?;
            let host = url.host_str().unwrap_or_default();
            let family = if ipv6 { "ipv6" } else { "ipv4" };
            let allowed: i64 = check.query_row(params![endpoint, family, host], |r| r.get(0))?;
            earliest = earliest.min(allowed);
            if earliest <= now.saturating_add(5) {
                break;
            }
        }
        Ok(earliest.saturating_sub(now).clamp(5, 60) as u64)
    }
    pub fn activity_targets(&self) -> Result<Vec<(String, String)>> {
        let mut q=self.private.prepare("SELECT e.id,e.url FROM endpoints e LEFT JOIN activity_cursor c ON c.endpoint_id=e.id WHERE NOT EXISTS (SELECT 1 FROM endpoint_alias a WHERE a.alias_id=e.id) ORDER BY COALESCE(c.last_attempt,0)/3600, RANDOM() LIMIT 20000")?;
        let rows = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    pub fn mark_activity(&self, endpoint: &str, now: i64) -> Result<()> {
        self.private.execute("INSERT INTO activity_cursor VALUES (?1,?2) ON CONFLICT(endpoint_id) DO UPDATE SET last_attempt=excluded.last_attempt",params![endpoint,now])?;
        Ok(())
    }
    pub fn previous_pool(&self) -> Result<crate::pool::PreviousPool> {
        use rusqlite::OptionalExtension;
        let value: Option<String> = self
            .private
            .query_row("SELECT v FROM meta WHERE k='pool_state'", [], |r| r.get(0))
            .optional()?;
        Ok(value
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default())
    }
    pub fn replace_pool(&mut self, update: crate::pool::PoolUpdate) -> Result<()> {
        let tx = self.private.transaction()?;
        tx.execute("UPDATE pool_registry SET active=0", [])?;
        for item in &update.samples {
            let sample_id = public_sample_id(&item.info_hash);
            let pool = if item.pool == crate::pool::PoolKind::Forum {
                "forum"
            } else {
                "software"
            };
            let source = publisher(item.source);
            tx.execute("INSERT INTO pool_registry VALUES (?1,?2,?3,?4,'{}',1,?5) ON CONFLICT(sample_id) DO UPDATE SET hash=excluded.hash,pool=excluded.pool,source=excluded.source,payload='{}',active=1,updated_at=excluded.updated_at", params![sample_id,item.info_hash.as_slice(),pool,source,update.collected_at])?;
            let (pieces, length, total) = item.geometry.unwrap_or((0, 0, 0));
            tx.execute(
                "INSERT OR REPLACE INTO sample_catalog VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    hex::encode(item.info_hash),
                    source,
                    item.info_hash.as_slice(),
                    pieces as i64,
                    length as i64,
                    total as i64,
                    update.collected_at
                ],
            )?;
        }
        for (key, forum) in [
            ("pool_software_fresh_at", false),
            ("pool_forum_fresh_at", true),
        ] {
            if pool_has_success(&update.samples, &update.sources, forum) {
                tx.execute(
                    "INSERT OR REPLACE INTO meta(k,v) VALUES (?1,?2)",
                    params![key, update.collected_at],
                )?;
            }
        }
        let previous = crate::pool::PreviousPool {
            samples: update.samples,
            sources: update.sources,
        };
        tx.execute(
            "INSERT OR REPLACE INTO meta(k,v) VALUES ('pool_state',?1)",
            [serde_json::to_string(&previous)?],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO meta(k,v) VALUES ('pool_updated_at',?1)",
            [update.collected_at],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn save_infrastructure(&self, host: &str, value: &Infrastructure) -> Result<()> {
        if value.asn.is_some() || value.organization.is_some() || value.country.is_some() {
            self.private.execute("INSERT INTO infrastructure VALUES (?1,?2,?3) ON CONFLICT(host) DO UPDATE SET payload=excluded.payload,updated_at=excluded.updated_at",params![host,serde_json::to_string(value)?,unix_now()])?;
        }
        Ok(())
    }
    pub fn value(&self, key: &str) -> Result<Option<i64>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .private
            .query_row(
                "SELECT CAST(v AS INTEGER) FROM meta WHERE k=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn set_value(&self, key: &str, value: i64) -> Result<()> {
        self.private.execute(
            "INSERT OR REPLACE INTO meta VALUES (?1,?2)",
            params![key, value.to_string()],
        )?;
        Ok(())
    }
    pub fn pause(&self, host: &str, until: i64) -> Result<()> {
        self.private.execute("INSERT INTO pauses VALUES (?1,?2) ON CONFLICT(host) DO UPDATE SET until=MAX(until,excluded.until)", params![host,until])?;
        Ok(())
    }
    pub fn paused(&self, host: &str, now: i64) -> Result<bool> {
        Ok(self.private.query_row(
            "SELECT EXISTS(SELECT 1 FROM pauses WHERE host=?1 AND until>?2)",
            params![host, now],
            |r| r.get(0),
        )?)
    }
    pub fn announce_allowed(
        &self,
        endpoint: &str,
        family: &str,
        resource: &str,
        now: i64,
    ) -> Result<bool> {
        Ok(!self.private.query_row("SELECT EXISTS(SELECT 1 FROM announce_due WHERE endpoint_id=?1 AND family=?2 AND resource=?3 AND until>?4)",params![endpoint,family,resource,now],|r|r.get::<_,bool>(0))?)
    }
    pub fn defer_announce(
        &self,
        endpoint: &str,
        family: &str,
        resource: &str,
        until: i64,
    ) -> Result<()> {
        self.private.execute("INSERT INTO announce_due VALUES (?1,?2,?3,?4) ON CONFLICT(endpoint_id,family,resource) DO UPDATE SET until=MAX(until,excluded.until)",params![endpoint,family,resource,until])?;
        Ok(())
    }
    pub fn save_sample(&self, source: &str, sample: &crate::samples::Sample) -> Result<()> {
        self.private.execute(
            "INSERT OR REPLACE INTO sample_catalog VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                hex::encode(sample.info_hash),
                source,
                sample.info_hash.as_slice(),
                sample.piece_count as i64,
                sample.piece_length as i64,
                sample.total_length as i64,
                unix_now()
            ],
        )?;
        Ok(())
    }
    pub fn next_sample(&self, endpoint: &str) -> Result<Option<(String, crate::samples::Sample)>> {
        use rusqlite::OptionalExtension;
        let first = if rand::random::<bool>() {
            "software"
        } else {
            "forum"
        };
        let other = if first == "software" {
            "forum"
        } else {
            "software"
        };
        for pool in [first, other] {
            let item = self.private.query_row("SELECT p.source,c.info_hash,c.piece_count,c.piece_length,c.total_length FROM pool_registry p JOIN sample_catalog c ON c.info_hash=p.hash WHERE p.active=1 AND p.pool=?3 AND (NOT EXISTS(SELECT 1 FROM announce_due d WHERE d.endpoint_id=?1 AND d.family='ipv4' AND d.resource=c.sample_id AND d.until>?2) OR NOT EXISTS(SELECT 1 FROM announce_due d WHERE d.endpoint_id=?1 AND d.family='ipv6' AND d.resource=c.sample_id AND d.until>?2)) ORDER BY RANDOM() LIMIT 1",params![endpoint,unix_now(),pool],|r| {
            let raw:Vec<u8>=r.get(1)?; let hash:[u8;20]=raw.try_into().map_err(|_|rusqlite::Error::InvalidQuery)?;
            Ok((r.get(0)?,crate::samples::Sample { info_hash:hash,piece_count:r.get::<_,i64>(2)? as usize,piece_length:r.get::<_,i64>(3)? as u64,total_length:r.get::<_,i64>(4)? as u64 }))
        }).optional()?;
            if item.is_some() {
                return Ok(item);
            }
        }
        Ok(None)
    }
    pub fn sample_evidence(&mut self, evidence: ActivityEvidence<'_>) -> Result<()> {
        let ActivityEvidence {
            endpoint,
            source,
            sample,
            peers,
            bitmaps,
            coverage,
            stages,
        } = evidence;
        let tx = self.private.transaction()?;
        let salt: String =
            tx.query_row("SELECT v FROM meta WHERE k='peer_salt'", [], |r| r.get(0))?;
        let sample_id = hex::encode(sample.info_hash);
        let now = unix_now();
        for peer in peers {
            let mut digest = Sha256::new();
            digest.update(salt.as_bytes());
            digest.update(peer.to_string().as_bytes());
            let key = hex::encode(digest.finalize());
            tx.execute("INSERT INTO observed_peers VALUES (?1,?2,?3,?4) ON CONFLICT(endpoint_id,sample_id,peer_key) DO UPDATE SET observed_at=excluded.observed_at",params![endpoint,sample_id,key,now])?;
        }
        let announce = stages.iter().find(|o| o.stage == "sample_announce");
        let handshake = stages
            .iter()
            .find(|o| o.stage == "handshake" && o.result == "accepted")
            .or_else(|| stages.iter().find(|o| o.stage == "handshake"));
        tx.execute("INSERT INTO samples(endpoint_id,sample_id,source,observed_at,handshakes,bitmap_observations,coverage,announce_result,announce_error,handshake_result,handshake_error) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![endpoint,sample_id,source,now,peers.len() as i64,bitmaps as i64,coverage,announce.map(|o|o.result.as_str()),announce.and_then(|o|o.error_category.as_deref()),handshake.map(|o|o.result.as_str()),handshake.and_then(|o|o.error_category.as_deref())])?;
        for observation in stages {
            tx.execute("INSERT INTO observations(endpoint_id,observed_at,family,payload) VALUES (?1,?2,?3,?4)",params![endpoint,observation.observed_at,observation.family,serde_json::to_string(observation)?])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn record_stats(&self, point: &StatsPoint) -> Result<()> {
        let bucket = point.at - point.at.rem_euclid(60);
        self.private.execute("INSERT INTO native_history VALUES (?1,?2,?3) ON CONFLICT(bucket) DO UPDATE SET instance=excluded.instance,payload=excluded.payload WHERE instance!=excluded.instance",params![bucket,point.instance,serde_json::to_string(point)?])?;
        Ok(())
    }
    pub fn record(&mut self, obs: &Observation) -> Result<()> {
        let encoded = serde_json::to_string(obs)?;
        self.private.execute(
            "INSERT INTO observations(endpoint_id,observed_at,family,payload) VALUES (?1,?2,?3,?4)",
            params![obs.endpoint_id, obs.observed_at, obs.family, encoded],
        )?;
        Ok(())
    }
    /// Duplicate endpoint -> canonical endpoint mapping maintained by
    /// [`Store::refresh_aliases`]. Aliases stay published (resolvable by id)
    /// but are never handed out as probe or activity work.
    pub fn alias_map(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut q = self
            .private
            .prepare("SELECT alias_id,canonical_id FROM endpoint_alias")?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    fn retired_own_aliases(
        &self,
        own_hosts: &[String],
    ) -> Result<std::collections::HashMap<String, String>> {
        use rusqlite::OptionalExtension;
        let mut result = std::collections::HashMap::new();
        for (host, retired, canonical) in [
            (
                "tracker.augrains.com",
                "http://announce.augrains.com/announce",
                "http://tracker.augrains.com/announce",
            ),
            (
                "tracker.augrains.com",
                "https://announce.augrains.com/announce",
                "https://tracker.augrains.com/announce",
            ),
            (
                "udp.augrains.com",
                "udp://announce.augrains.com:6969/announce",
                "udp://udp.augrains.com:6969/announce",
            ),
        ] {
            if !own_hosts.iter().any(|own| own.eq_ignore_ascii_case(host)) {
                continue;
            }
            let pair: Option<(String, String)> = self
                .private
                .query_row(
                    "SELECT old.id, canonical.id FROM endpoints old JOIN endpoints canonical ON canonical.url=?2 WHERE old.url=?1",
                    params![retired, canonical],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((old, canonical)) = pair {
                result.insert(old, canonical);
            }
        }
        Ok(result)
    }
    /// Recompute the duplicate-origin mapping from recent evidence.
    ///
    /// Endpoints are the same origin when the address the probe actually
    /// contacts and the scheme/port/announce path are identical: a literal-IP
    /// URL and the hostname resolving to that address would otherwise announce
    /// twice. For HTTP(S) only literal-IP duplicates are merged, so two
    /// hostname vhosts sharing an address stay distinct; UDP has no Host
    /// header, so a shared address is a duplicate. Returns whether the
    /// alias -> canonical mapping changed.
    pub fn refresh_aliases(&mut self, now: i64, own_hosts: &[String]) -> Result<bool> {
        struct Member {
            id: String,
            url: String,
            literal: bool,
            target_ip: String,
            latest_accepted: bool,
            rate: Option<f64>,
            successes: u64,
            median: Option<f64>,
            checks: u64,
        }
        struct OldEvidence {
            rate: Option<f64>,
            successes: u64,
            median: Option<f64>,
            checks: u64,
        }
        let since = now - 86400;
        // The latest announce verdict per endpoint and address family. Judging
        // an endpoint by its single newest announce would make a dual-stack
        // hostname unhealthy whenever that check hit the family with no public
        // address, and the canonical choice would flip between the hostname
        // and its literal IP; any family with a fresh accepted announce is
        // enough, while a family whose latest check failed stays unhealthy.
        let mut latest = std::collections::HashMap::<String, Vec<(String, i64)>>::new();
        {
            let mut q = self.private.prepare(
                "SELECT endpoint_id,family,result,observed_at FROM (SELECT endpoint_id,family,json_extract(payload,'$.result') AS result,observed_at,ROW_NUMBER() OVER (PARTITION BY endpoint_id,family ORDER BY observed_at DESC,id DESC) AS rn FROM observations WHERE observed_at>=?1 AND json_extract(payload,'$.stage')='announce') WHERE rn=1",
            )?;
            let rows = q.query_map([since], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (endpoint, _family, result, observed_at) = row?;
                latest
                    .entry(endpoint)
                    .or_default()
                    .push((result.unwrap_or_default(), observed_at));
            }
        }
        // Most recent announce that recorded the address actually contacted,
        // kept separately per family so a dual-stack hostname can match both an
        // IPv4 and an IPv6 literal endpoint without alternating between them.
        let mut observed_ip = std::collections::HashMap::<String, Vec<String>>::new();
        {
            let mut q = self.private.prepare(
                "SELECT endpoint_id,family,target_ip FROM (SELECT endpoint_id,family,json_extract(payload,'$.target_ip') AS target_ip,observed_at,ROW_NUMBER() OVER (PARTITION BY endpoint_id,family ORDER BY observed_at DESC,id DESC) AS rn FROM observations WHERE observed_at>=?1 AND json_extract(payload,'$.stage')='announce' AND json_extract(payload,'$.target_ip') IS NOT NULL) WHERE rn=1",
            )?;
            let rows = q.query_map([since], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (endpoint, family, target_ip) = row?;
                if !matches!(family.as_str(), "ipv4" | "ipv6") {
                    continue;
                }
                // Canonical text form keeps IPv6 representations comparable.
                if let Ok(ip) = target_ip.parse::<std::net::IpAddr>() {
                    let list = observed_ip.entry(endpoint).or_default();
                    let text = ip.to_string();
                    if !list.contains(&text) {
                        list.push(text);
                    }
                }
            }
        }
        // Health evidence from the previous publication for tie-breaking.
        let mut old_evidence = std::collections::HashMap::<String, OldEvidence>::new();
        {
            let mut q = self.public.prepare(
                "SELECT id,json_extract(payload,'$.evidence.success_rate_24h'),json_extract(payload,'$.evidence.successes_24h'),json_extract(payload,'$.evidence.median_latency_ms'),json_extract(payload,'$.evidence.checks_24h') FROM endpoints WHERE json_valid(payload)",
            )?;
            let rows = q.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<f64>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<f64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            })?;
            for row in rows {
                let (endpoint, rate, successes, median, checks) = row?;
                old_evidence.insert(
                    endpoint,
                    OldEvidence {
                        rate,
                        successes: successes.unwrap_or(0).max(0) as u64,
                        median,
                        checks: checks.unwrap_or(0).max(0) as u64,
                    },
                );
            }
        }
        let mut endpoints = Vec::new();
        {
            let mut q = self.private.prepare("SELECT id,url FROM endpoints")?;
            let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                endpoints.push(row?);
            }
        }
        let mut buckets =
            std::collections::BTreeMap::<(String, u16, String, String), Vec<Member>>::new();
        for (endpoint_id, url) in endpoints {
            let Ok(parsed) = Url::parse(&url) else {
                continue;
            };
            let scheme = parsed.scheme().to_string();
            if !matches!(scheme.as_str(), "udp" | "http" | "https") {
                continue;
            }
            let Some(host) = parsed.host_str() else {
                continue;
            };
            let host = host.trim_matches(['[', ']']);
            // Configured public entry points are not duplicate community
            // trackers, even when a retired name resolves to the same address.
            if own_hosts.iter().any(|own| own.eq_ignore_ascii_case(host)) {
                continue;
            }
            let literal = host.parse::<std::net::IpAddr>().ok();
            let targets: Vec<String> = match literal {
                // A literal host is its own target even before any observation.
                Some(ip) => vec![ip.to_string()],
                None => observed_ip.remove(&endpoint_id).unwrap_or_default(),
            };
            if targets.is_empty() {
                continue;
            }
            let latest_accepted = latest.get(&endpoint_id).is_some_and(|families| {
                families
                    .iter()
                    .any(|(result, at)| result == "accepted" && *at >= since)
            });
            let evidence = old_evidence.remove(&endpoint_id).unwrap_or(OldEvidence {
                rate: None,
                successes: 0,
                median: None,
                checks: 0,
            });
            // Missing UDP ports are probed on 80, mirroring probe_udp.
            let port = parsed.port_or_known_default().unwrap_or(80);
            let path = parsed.path().to_string();
            for target_ip in targets {
                let key = (scheme.clone(), port, path.clone(), target_ip.clone());
                buckets.entry(key).or_default().push(Member {
                    id: endpoint_id.clone(),
                    url: url.clone(),
                    literal: literal.is_some(),
                    target_ip,
                    latest_accepted,
                    rate: evidence.rate,
                    successes: evidence.successes,
                    median: evidence.median,
                    checks: evidence.checks,
                });
            }
        }
        let previous = self.alias_map()?;
        let retired = self.retired_own_aliases(own_hosts)?;
        let previous_canonicals: std::collections::HashSet<&String> = previous.values().collect();
        let mut mapping = std::collections::HashMap::<String, (String, String)>::new();
        for ((scheme, _, _, _), members) in buckets {
            if members.len() < 2 {
                continue;
            }
            if scheme != "udp" && !members.iter().any(|m| m.literal) {
                // Distinct hostname vhosts on one address are not duplicates.
                continue;
            }
            let healthy = members
                .iter()
                .filter(|m| {
                    m.latest_accepted && (m.checks == 0 || m.rate.is_some_and(|rate| rate >= 0.8))
                })
                .collect::<Vec<_>>();
            if healthy.is_empty() {
                // Never hide a working address behind a failing canonical URL.
                continue;
            }
            // Prefer a hostname as the canonical public URL; a literal address
            // is only canonical when no healthy hostname resolves there.
            let mut candidates = healthy
                .iter()
                .copied()
                .filter(|m| !m.literal)
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                candidates = healthy.clone();
            }
            candidates.sort_by(|a, b| {
                b.rate
                    .unwrap_or(f64::NEG_INFINITY)
                    .total_cmp(&a.rate.unwrap_or(f64::NEG_INFINITY))
                    .then_with(|| b.successes.cmp(&a.successes))
                    .then_with(|| {
                        a.median
                            .unwrap_or(f64::INFINITY)
                            .total_cmp(&b.median.unwrap_or(f64::INFINITY))
                    })
                    .then_with(|| a.literal.cmp(&b.literal))
                    .then_with(|| a.url.cmp(&b.url))
            });
            // Keep the previous canonical while it is still a healthy candidate:
            // evidence changes must not make the public URL flip on every cycle.
            let canonical = candidates
                .iter()
                .find(|m| previous_canonicals.contains(&m.id))
                .copied()
                .unwrap_or(candidates[0]);
            for alias in members
                .iter()
                .filter(|m| m.id != canonical.id && (scheme == "udp" || m.literal))
            {
                mapping.insert(
                    alias.id.clone(),
                    (canonical.id.clone(), alias.target_ip.clone()),
                );
            }
        }
        // Explicit retirement takes precedence over evidence-based aliases.
        // Redirect any existing duplicate of the retired name directly to the
        // current entry point so no alias chain can send clients backwards.
        for (canonical, _) in mapping.values_mut() {
            if let Some(replacement) = retired.get(canonical) {
                *canonical = replacement.clone();
            }
        }
        for (alias, canonical) in retired {
            mapping.insert(alias, (canonical, String::new()));
        }
        let changed = previous.len() != mapping.len()
            || mapping
                .iter()
                .any(|(alias, (canonical, _))| previous.get(alias) != Some(canonical));
        if changed {
            let tx = self.private.transaction()?;
            tx.execute("DELETE FROM endpoint_alias", [])?;
            for (alias, (canonical, target_ip)) in &mapping {
                tx.execute(
                    "INSERT INTO endpoint_alias VALUES (?1,?2,?3,?4)",
                    params![alias, canonical, target_ip, now],
                )?;
            }
            tx.commit()?;
            tracing::info!(aliases = mapping.len(), "endpoint aliases updated");
        }
        self.set_value("alias_checked_at", now)?;
        Ok(changed)
    }
    pub fn publish(&mut self, interval: u64, own_hosts: &[String]) -> Result<()> {
        let now = unix_now();
        if self
            .value("alias_checked_at")?
            .is_none_or(|at| now.saturating_sub(at) > 600)
        {
            self.refresh_aliases(now, own_hosts)?;
        }
        let aliases = self.alias_map()?;
        // Summarize the soon-to-expire detail before pruning; rebuild is idempotent.
        {
            let prune = self.private.transaction()?;
            prune.execute("INSERT INTO daily SELECT endpoint_id,CAST(observed_at/86400 AS INTEGER),family,SUM(CASE WHEN json_extract(payload,'$.result') IN ('accepted','failed') THEN 1 ELSE 0 END),SUM(CASE WHEN json_extract(payload,'$.result')='accepted' THEN 1 ELSE 0 END),SUM(CASE WHEN json_extract(payload,'$.protocol_reply')=1 THEN 1 ELSE 0 END) FROM observations WHERE observed_at<?1 AND json_extract(payload,'$.stage')='announce' GROUP BY endpoint_id,CAST(observed_at/86400 AS INTEGER),family ON CONFLICT(endpoint_id,day,family) DO UPDATE SET checks=checks+excluded.checks,successes=successes+excluded.successes,replies=replies+excluded.replies", [now-7*86400])?;
            prune.execute(
                "DELETE FROM observations WHERE observed_at<?1",
                [now - 7 * 86400],
            )?;
            prune.execute(
                "DELETE FROM daily WHERE day<?1",
                [(now - 90 * 86400) / 86400],
            )?;
            prune.execute(
                "DELETE FROM samples WHERE observed_at<?1",
                [now - 7 * 86400],
            )?;
            prune.execute(
                "DELETE FROM observed_peers WHERE observed_at<?1",
                [now - 7 * 86400],
            )?;
            prune.execute(
                "DELETE FROM pool_registry WHERE active=0 AND updated_at<?1",
                [now - 7 * 86400],
            )?;
            prune.execute(
                "DELETE FROM native_history WHERE bucket<?1",
                [now - 30 * 86400],
            )?;
            prune.execute("DELETE FROM native_history WHERE bucket<?1 AND bucket NOT IN (SELECT MAX(bucket) FROM native_history WHERE bucket<?1 GROUP BY bucket/3600)",[now-7*86400])?;
            prune.execute(
                "DELETE FROM sample_catalog WHERE observed_at<?1",
                [now - 10 * 86400],
            )?;
            prune.execute("DELETE FROM pauses WHERE until<?1", [now])?;
            prune.execute("DELETE FROM announce_due WHERE until<?1", [now - 86400])?;
            prune.execute(
                "DELETE FROM infrastructure WHERE updated_at<?1",
                [now - 7 * 86400],
            )?;
            prune.commit()?;
        }
        let mut query = self.private.prepare(
            "SELECT e.id,e.service_id,e.url,e.first_seen,e.baseline,v4.until,v6.until
             FROM endpoints e
             LEFT JOIN announce_due v4 ON v4.endpoint_id=e.id AND v4.family='ipv4' AND v4.resource='health'
             LEFT JOIN announce_due v6 ON v6.endpoint_id=e.id AND v6.family='ipv6' AND v6.resource='health'
             ORDER BY e.url",
        )?;
        let rows = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(query);
        let mut query = self
            .private
            .prepare("SELECT host,until FROM pauses WHERE until>?1")?;
        let paused_hosts = query
            .query_map([now], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()?;
        drop(query);
        // Public NEW describes the stable service, not a newly added endpoint.
        // Keep the endpoint's own first_seen and baseline in the private table.
        let mut service_discovery = std::collections::HashMap::<&str, (i64, bool)>::new();
        for (_, service_id, _, first_seen, baseline, ..) in &rows {
            service_discovery
                .entry(service_id.as_str())
                .and_modify(|(first, base)| {
                    *first = (*first).min(*first_seen);
                    *base |= *baseline != 0;
                })
                .or_insert((*first_seen, *baseline != 0));
        }
        let previous_pool = self.previous_pool()?;
        let pool_updated_at = self.value("pool_updated_at")?.unwrap_or(0);
        let pool_last_attempt = self.value("pool_last_attempt")?.unwrap_or(0);
        let pool_fresh_at = [
            self.value("pool_software_fresh_at")?,
            self.value("pool_forum_fresh_at")?,
        ];
        let tx = self.public.transaction()?;
        // A cursor is committed with the public rows, so an interrupted pass
        // can be retried without gaps or duplication. IDs never reuse after prune.
        let cursor: i64 =
            tx.query_row("SELECT COALESCE(MAX(id),0) FROM observations", [], |r| {
                r.get(0)
            })?;
        {
            let mut events=self.private.prepare("SELECT id,endpoint_id,observed_at,family,payload FROM observations WHERE id>?1 AND observed_at>=?2 ORDER BY id")?;
            let mut rows = events.query(params![cursor, now - 7 * 86400])?;
            while let Some(row) = rows.next()? {
                tx.execute("INSERT OR IGNORE INTO observations(id,endpoint_id,observed_at,family,payload) VALUES (?1,?2,?3,?4,?5)",params![row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?])?;
            }
        }
        tx.execute(
            "DELETE FROM observations WHERE observed_at<?1",
            [now - 7 * 86400],
        )?;
        {
            let last: i64 = tx.query_row(
                "SELECT COALESCE(MAX(bucket),0) FROM stats_history",
                [],
                |r| r.get(0),
            )?;
            let mut q = self
                .private
                .prepare("SELECT bucket,instance,payload FROM native_history WHERE bucket>=?1")?;
            for row in q.query_map([last], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (bucket, instance, payload) = row?;
                tx.execute("INSERT INTO stats_history VALUES (?1,?2,?3) ON CONFLICT(bucket) DO UPDATE SET instance=excluded.instance,payload=excluded.payload WHERE instance!=excluded.instance",params![bucket,instance,payload])?;
            }
            tx.execute(
                "DELETE FROM stats_history WHERE bucket<?1",
                [now - 30 * 86400],
            )?;
            tx.execute("DELETE FROM stats_history WHERE bucket<?1 AND bucket NOT IN (SELECT MAX(bucket) FROM stats_history WHERE bucket<?1 GROUP BY bucket/3600)",[now-7*86400])?;
        }
        {
            let previous = &previous_pool;
            let mut statuses = Vec::new();
            for (name, forum) in [("software", false), ("forum", true)] {
                let target = if forum {
                    crate::pool::GHOST_SAMPLE_COUNT
                } else {
                    crate::pool::MAX_POOL
                };
                let count = previous
                    .samples
                    .iter()
                    .filter(|s| (s.pool == crate::pool::PoolKind::Forum) == forum)
                    .count();
                let sources: Vec<_> = previous
                    .sources
                    .iter()
                    .filter(|s| s.source.is_forum() == forum)
                    .collect();
                let successful = pool_has_success(&previous.samples, &previous.sources, forum);
                // Existing snapshots predate per-pool freshness timestamps.
                let at = if count == 0 {
                    0
                } else {
                    pool_fresh_at[usize::from(forum)].unwrap_or(if successful {
                        pool_updated_at
                    } else {
                        0
                    })
                };
                let stale = count > 0 && (at == 0 || now.saturating_sub(at) > 86400);
                let failed = sources.iter().any(|s| s.status != "ok");
                statuses.push(PoolStatus {
                    pool: name.into(),
                    count,
                    target,
                    updated_at: at,
                    last_attempt_at: pool_last_attempt,
                    stale,
                    status: if count == 0 {
                        "deferred"
                    } else if count < target || stale || failed {
                        "partial"
                    } else {
                        "ready"
                    }
                    .into(),
                    error_category: if stale || failed {
                        Some(
                            sources
                                .iter()
                                .find_map(|s| s.error_category.clone())
                                .unwrap_or_else(|| "source_deferred".into()),
                        )
                    } else {
                        None
                    },
                });
            }
            tx.execute(
                "INSERT OR REPLACE INTO documents VALUES ('sample_pools',?1,?2)",
                params![serde_json::to_string(&statuses)?, now],
            )?;
        }
        let mut all_candidates = Vec::new();
        let mut standard_candidates = Vec::new();
        let mut best_candidates = Vec::new();
        for (endpoint_id, service_id, url, first_seen, _, next_v4, next_v6) in &rows {
            let (service_first_seen, service_baseline) = service_discovery[service_id.as_str()];
            let alias_of = aliases.get(endpoint_id).cloned();
            let parsed = Url::parse(url)?;
            let paused_until = paused_hosts
                .get(parsed.host_str().unwrap_or_default())
                .copied();
            let next_check = |family: &str| {
                let scheduled = match family {
                    "ipv4" => *next_v4,
                    "ipv6" => *next_v6,
                    _ => return None,
                };
                scheduled.max(paused_until).filter(|until| *until > now)
            };
            let colocated = own_hosts
                .iter()
                .any(|h| h.eq_ignore_ascii_case(parsed.host_str().unwrap_or_default()));
            let event_revision: i64 = self.private.query_row(
                "SELECT COALESCE(MAX(id),0) FROM observations WHERE endpoint_id=?1",
                [endpoint_id],
                |r| r.get(0),
            )?;
            let sample_revision: i64 = self.private.query_row(
                "SELECT COALESCE(MAX(rowid),0) FROM samples WHERE endpoint_id=?1",
                [endpoint_id],
                |r| r.get(0),
            )?;
            use rusqlite::OptionalExtension;
            let cached:Option<(String,i64)>=tx.query_row("SELECT e.payload,c.all_eligible FROM publication_cache c JOIN endpoints e ON e.id=c.endpoint_id WHERE c.endpoint_id=?1 AND c.event_revision=?2 AND c.sample_revision=?3 AND c.next_refresh>?4 AND c.service_id=?5 AND c.colocated=?6 AND c.interval_sec=?7",params![endpoint_id,event_revision,sample_revision,now,service_id,colocated as i64,interval as i64],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((payload, all_eligible)) = cached
                && let Ok(mut endpoint) = serde_json::from_str::<Endpoint>(&payload)
                && endpoint.first_seen == service_first_seen
                && endpoint.baseline == service_baseline
                && endpoint.alias_of == alias_of
            {
                // Host backoff and family schedules can change without a new observation.
                // Keep the cached evidence and update just the public schedule in that case.
                let mut schedule_changed = false;
                for family in &mut endpoint.families {
                    let scheduled = next_check(&family.family);
                    schedule_changed |= family.next_check_at != scheduled;
                    family.next_check_at = scheduled;
                }
                if schedule_changed {
                    tx.execute(
                        "UPDATE endpoints SET payload=?1 WHERE id=?2",
                        params![serde_json::to_string(&endpoint)?, endpoint_id],
                    )?;
                }
                // Aliases are published for id lookups but never listed as feed
                // candidates, and a cached payload whose alias status changed
                // is rebuilt below instead of reused.
                if endpoint.alias_of.is_none() {
                    let poisoned = url.contains("durukanbal.com");
                    let score = endpoint.evidence.success_rate_24h.unwrap_or(0.0);
                    let candidate = (
                        service_id.clone(),
                        url.clone(),
                        score,
                        endpoint.evidence.sample_count_7d as usize,
                        endpoint.evidence.median_latency_ms.unwrap_or(f64::INFINITY),
                    );
                    if !poisoned && all_eligible != 0 {
                        all_candidates.push(candidate.clone());
                    }
                    if !poisoned && endpoint.evidence.standard_eligible {
                        standard_candidates.push(candidate.clone());
                    }
                    if !poisoned && endpoint.evidence.best_eligible {
                        best_candidates.push(candidate);
                    }
                }
                continue;
            }
            let mut q = self.private.prepare("SELECT payload FROM observations WHERE endpoint_id=?1 AND observed_at>=?2 AND json_extract(payload,'$.stage')='announce' ORDER BY observed_at ASC")?;
            let observations = q
                .query_map(params![endpoint_id, now - 7 * 86400], |r| {
                    r.get::<_, String>(0)
                })?
                .filter_map(|r| {
                    r.ok()
                        .and_then(|s| serde_json::from_str::<Observation>(&s).ok())
                })
                .collect::<Vec<_>>();
            let mut family_status = Vec::new();
            for family in ["ipv4", "ipv6"] {
                if let Some(o) = observations.iter().rev().find(|o| o.family == family) {
                    let valid_until = o.observed_at + (2 * interval as i64 + 300).min(86400);
                    family_status.push(FamilyStatus {
                        family: family.into(),
                        state: if valid_until < now {
                            "unknown".into()
                        } else if o.error_category.as_deref() == Some("family_unavailable") {
                            "not_published".into()
                        } else {
                            o.result.clone()
                        },
                        checked_at: Some(o.observed_at),
                        valid_until: Some(valid_until),
                        next_check_at: next_check(family),
                        http_status: o.http_status,
                        protocol_result: Some(o.result.clone()),
                        error_category: o.error_category.clone(),
                        latency_ms: o.latency_ms,
                    });
                } else {
                    family_status.push(FamilyStatus {
                        family: family.into(),
                        state: "unknown".into(),
                        next_check_at: next_check(family),
                        ..Default::default()
                    });
                }
            }
            let valid: Vec<_> = observations
                .iter()
                .filter(|o| o.result == "accepted" || o.result == "failed")
                .collect();
            let recent: Vec<_> = valid
                .iter()
                .filter(|o| o.observed_at >= now - 86400)
                .collect();
            let successes = recent.iter().filter(|o| o.result == "accepted").count();
            let recent_probes: Vec<_> = observations
                .iter()
                .filter(|o| o.observed_at >= now - 86400 && attempted_protocol(o))
                .collect();
            let replies = recent_probes.iter().filter(|o| protocol_reply(o)).count();
            let successes7 = valid.iter().filter(|o| o.result == "accepted").count();
            let mut sample_q = self.private.prepare("SELECT sample_id,source,observed_at,handshakes,bitmap_observations,coverage FROM samples WHERE endpoint_id=?1 AND observed_at>=?2")?;
            let samples = sample_q
                .query_map(params![endpoint_id, now - 7 * 86400], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, Option<f64>>(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let successful: Vec<_> = samples.iter().filter(|s| s.3 > 0).collect();
            let sample_ids = successful
                .iter()
                .map(|s| &s.0)
                .collect::<std::collections::HashSet<_>>();
            let source_ids = successful
                .iter()
                .map(|s| &s.1)
                .collect::<std::collections::HashSet<_>>();
            let last_handshake = successful.iter().map(|s| s.2).max();
            let recent_samples = successful
                .iter()
                .filter(|s| s.2 >= now - 72 * 3600)
                .map(|s| &s.0)
                .collect::<std::collections::HashSet<_>>();
            let handshakes:u64=self.private.query_row("SELECT COUNT(DISTINCT peer_key) FROM observed_peers WHERE endpoint_id=?1 AND observed_at>=?2",params![endpoint_id,now-7*86400],|r|r.get(0))?;
            let bitmaps = samples.iter().map(|s| s.4.max(0) as u64).sum::<u64>();
            let span = valid
                .first()
                .zip(valid.last())
                .map(|(a, b)| (b.observed_at - a.observed_at).max(0) as u64)
                .unwrap_or(0);
            let coverage: Vec<f64> = samples
                .iter()
                .filter_map(|s| s.5.filter(|x| x.is_finite() && (0.0..=1.0).contains(x)))
                .collect();
            // Feed eligibility deliberately ignores the ghost-sample verdict
            // (`poisoned_peers`, recorded on `sample_announce` rows). Measured
            // against the public snapshot it flagged a fifth of all endpoints,
            // long-established trackers included, so it stays a diagnostic
            // until the check can tell fabricated peers from real ones. See
            // docs/releases/1.2.5.md.
            let poisoned = url.contains("durukanbal.com");
            let standard = !poisoned
                && family_status
                    .iter()
                    .all(|f| f.state == "accepted" || f.state == "not_published")
                && family_status.iter().any(|f| f.state == "accepted")
                && recent.len() >= 4
                && successes * 100 >= 95 * recent.len()
                && recent_samples.len() >= 2;
            let best = !poisoned
                && standard
                && valid.len() >= 48
                && span >= 72 * 3600
                && successes7 * 100 >= 99 * valid.len()
                && sample_ids.len() >= 4
                && source_ids.len() >= 2
                && handshakes >= 12
                && bitmaps >= 4
                && last_handshake.is_some_and(|t| t >= now - 86400);
            let mut latencies: Vec<f64> = recent
                .iter()
                .filter(|o| o.result == "accepted")
                .filter_map(|o| o.latency_ms)
                .filter(|x| x.is_finite())
                .collect();
            latencies.sort_by(f64::total_cmp);
            let median = if latencies.is_empty() {
                f64::INFINITY
            } else if latencies.len().is_multiple_of(2) {
                (latencies[latencies.len() / 2 - 1] + latencies[latencies.len() / 2]) / 2.0
            } else {
                latencies[latencies.len() / 2]
            };
            let evidence = Evidence {
                checks_24h: recent.len() as u64,
                successes_24h: successes as u64,
                replies_24h: replies as u64,
                reply_checks_24h: recent_probes.len() as u64,
                success_rate_24h: (!recent.is_empty())
                    .then(|| successes as f64 / recent.len() as f64),
                reply_rate_24h: (!recent_probes.is_empty())
                    .then(|| replies as f64 / recent_probes.len() as f64),
                median_latency_ms: median.is_finite().then_some(median),
                checks_7d: valid.len() as u64,
                successes_7d: successes7 as u64,
                success_rate_7d: (!valid.is_empty())
                    .then(|| successes7 as f64 / valid.len() as f64),
                observation_span_secs: span,
                handshake_nodes_7d: handshakes,
                sample_count_7d: sample_ids.len() as u64,
                source_count_7d: source_ids.len() as u64,
                bitmap_observations_7d: bitmaps,
                last_handshake_at: last_handshake,
                coverage_mean: (!coverage.is_empty())
                    .then(|| coverage.iter().sum::<f64>() / coverage.len() as f64),
                standard_eligible: standard,
                best_eligible: best,
            };
            let candidate = (
                service_id.clone(),
                url.clone(),
                successes as f64 / (recent.len().max(1) as f64),
                sample_ids.len(),
                median,
            );
            if alias_of.is_none() {
                if !poisoned
                    && observations
                        .iter()
                        .any(|o| o.observed_at >= now - 86400 && o.result == "accepted")
                {
                    all_candidates.push(candidate.clone());
                }
                if standard {
                    standard_candidates.push(candidate.clone());
                }
                if best {
                    best_candidates.push(candidate);
                }
            }
            let updated_at = observations
                .last()
                .map(|o| o.observed_at)
                .unwrap_or(*first_seen);
            let all_eligible = observations
                .iter()
                .any(|o| o.observed_at >= now - 86400 && o.result == "accepted");
            let mut next_refresh = now + 3600;
            for o in &observations {
                for expiry in [
                    o.observed_at + 86401,
                    o.observed_at + 7 * 86400 + 1,
                    o.observed_at + (2 * interval as i64 + 300).min(86400) + 1,
                ] {
                    if expiry > now {
                        next_refresh = next_refresh.min(expiry);
                    }
                }
            }
            for s in &samples {
                for expiry in [s.2 + 86401, s.2 + 72 * 3600 + 1, s.2 + 7 * 86400 + 1] {
                    if expiry > now {
                        next_refresh = next_refresh.min(expiry);
                    }
                }
            }
            let infra: Option<String> = self
                .private
                .query_row(
                    "SELECT payload FROM infrastructure WHERE host=?1 AND updated_at>=?2",
                    params![parsed.host_str().unwrap_or_default(), now - 7 * 86400],
                    |r| r.get(0),
                )
                .optional()?;
            let endpoint = Endpoint {
                id: endpoint_id.clone(),
                service_id: service_id.clone(),
                url: url.clone(),
                protocol: parsed.scheme().into(),
                host: parsed.host_str().unwrap_or_default().into(),
                first_seen: service_first_seen,
                baseline: service_baseline,
                updated_at,
                colocated,
                families: family_status,
                evidence,
                infrastructure: infra
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                alias_of: alias_of.clone(),
            };
            tx.execute("INSERT INTO endpoints VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(id) DO UPDATE SET service_id=excluded.service_id,url=excluded.url,first_seen=excluded.first_seen,baseline=excluded.baseline,updated_at=excluded.updated_at,payload=excluded.payload WHERE endpoints.payload!=excluded.payload OR endpoints.service_id!=excluded.service_id", params![endpoint_id, service_id, url, service_first_seen, service_baseline as i64, updated_at, serde_json::to_string(&endpoint)?])?;
            tx.execute(
                "INSERT OR REPLACE INTO publication_cache VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    endpoint_id,
                    event_revision,
                    sample_revision,
                    next_refresh,
                    service_id,
                    colocated as i64,
                    all_eligible as i64,
                    interval as i64
                ],
            )?;
        }
        let choose =
            |mut candidates: Vec<(String, String, f64, usize, f64)>, limit: usize| -> Vec<String> {
                candidates.sort_by(|a, b| {
                    b.2.total_cmp(&a.2)
                        .then(b.3.cmp(&a.3))
                        .then(a.4.total_cmp(&b.4))
                        .then(a.1.cmp(&b.1))
                });
                let mut seen = std::collections::HashSet::new();
                candidates
                    .into_iter()
                    .filter_map(|c| seen.insert(c.0).then_some(c.1))
                    .take(limit)
                    .collect()
            };
        let standard = choose(standard_candidates, 100);
        let best = choose(best_candidates, 20);
        let all = choose(all_candidates, usize::MAX);
        let feeds = [
            ("trackers.txt", standard.clone()),
            ("trackers_best.txt", best.clone()),
            (
                "trackers_udp.txt",
                standard
                    .iter()
                    .filter(|s| s.starts_with("udp://"))
                    .cloned()
                    .collect(),
            ),
            (
                "trackers_http.txt",
                standard
                    .iter()
                    .filter(|s| s.starts_with("http://"))
                    .cloned()
                    .collect(),
            ),
            (
                "trackers_https.txt",
                standard
                    .iter()
                    .filter(|s| s.starts_with("https://"))
                    .cloned()
                    .collect(),
            ),
        ];
        for (name, entries) in feeds {
            tx.execute(
                "INSERT OR REPLACE INTO documents VALUES (?1,?2,?3)",
                params![
                    format!("feed:{name}"),
                    serde_json::to_string(&entries.join("\n"))?,
                    now
                ],
            )?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO documents VALUES ('feed:trackers_all.txt',?1,?2)",
            params![serde_json::to_string(&all.join("\n"))?, now],
        )?;
        // Overview totals describe origins, not duplicate aliases.
        let mut visible_services = std::collections::HashSet::new();
        let mut visible_endpoints = 0usize;
        for (endpoint_id, service_id, ..) in &rows {
            if aliases.contains_key(endpoint_id) {
                continue;
            }
            visible_endpoints += 1;
            visible_services.insert(service_id);
        }
        let overview = Overview {
            version: VERSION.into(),
            generated_at: now,
            endpoints: visible_endpoints,
            services: visible_services.len(),
            all_count: all.len(),
            standard_count: standard.len(),
            best_count: best.len(),
            warming: standard.is_empty(),
        };
        tx.execute(
            "INSERT OR REPLACE INTO documents VALUES ('overview',?1,?2)",
            params![serde_json::to_string(&overview)?, now],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ghost_pool_status_uses_its_actual_target() -> Result<()> {
        use crate::pool::{PoolKind, PoolUpdate, SampleRecord, Source, ghost_samples};

        fn published_pool(store: &mut Store, pool: &str) -> Result<PoolStatus> {
            store.publish(1800, &[])?;
            let raw: String = store.public.query_row(
                "SELECT payload FROM documents WHERE key='sample_pools'",
                [],
                |r| r.get(0),
            )?;
            let statuses: Vec<PoolStatus> = serde_json::from_str(&raw)?;
            Ok(statuses.into_iter().find(|s| s.pool == pool).unwrap())
        }

        let dir =
            std::env::temp_dir().join(format!("tracker-ghost-pool-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let now = unix_now();
        let source = |source, status: &str| crate::pool::PoolStatus {
            source,
            status: status.into(),
            retry_at: 0,
            error_category: (status != "ok").then(|| "source_deferred".into()),
        };
        let mut samples = ghost_samples(now);
        samples.push(SampleRecord {
            info_hash: [42; 20],
            pool: PoolKind::Official,
            source: Source::Debian,
            name: "Debian".into(),
            discovered_at: now,
            geometry: Some((1, 1, 1)),
        });
        let sources = || vec![source(Source::Hjd, "ok"), source(Source::Debian, "ok")];
        store.replace_pool(PoolUpdate {
            samples: samples.clone(),
            sources: sources(),
            collected_at: now,
        })?;
        let ghost = published_pool(&mut store, "forum")?;
        assert_eq!(
            (ghost.count, ghost.target, ghost.status.as_str()),
            (16, 16, "ready")
        );
        let software = published_pool(&mut store, "software")?;
        assert_eq!(
            (software.count, software.target, software.status.as_str()),
            (1, 100, "partial")
        );

        let mut incomplete = samples.clone();
        incomplete.remove(0);
        store.replace_pool(PoolUpdate {
            samples: incomplete,
            sources: sources(),
            collected_at: now,
        })?;
        let ghost = published_pool(&mut store, "forum")?;
        assert_eq!(
            (ghost.count, ghost.target, ghost.status.as_str()),
            (15, 16, "partial")
        );

        store.replace_pool(PoolUpdate {
            samples: samples.clone(),
            sources: sources(),
            collected_at: now,
        })?;
        store.set_value("pool_forum_fresh_at", now - 86401)?;
        let ghost = published_pool(&mut store, "forum")?;
        assert!(ghost.stale);
        assert_eq!(ghost.status, "partial");

        store.replace_pool(PoolUpdate {
            samples,
            sources: vec![
                source(Source::Hjd, "ok"),
                source(Source::FbyLt1, "deferred"),
                source(Source::Debian, "ok"),
            ],
            collected_at: now,
        })?;
        let ghost = published_pool(&mut store, "forum")?;
        assert!(!ghost.stale);
        assert_eq!(ghost.status, "partial");
        assert_eq!(ghost.error_category.as_deref(), Some("source_deferred"));
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn pool_freshness_is_distinct_from_partial_source_failure() -> Result<()> {
        use crate::pool::{PoolKind, PoolUpdate, SampleRecord, Source};
        let dir =
            std::env::temp_dir().join(format!("tracker-pool-freshness-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let now = unix_now();
        let sample = SampleRecord {
            info_hash: [42; 20],
            pool: PoolKind::Official,
            source: Source::Debian,
            name: "Debian".into(),
            discovered_at: now,
            geometry: Some((1, 1, 1)),
        };
        let source = |source, status: &str| crate::pool::PoolStatus {
            source,
            status: status.into(),
            retry_at: 0,
            error_category: if status == "ok" {
                None
            } else {
                Some("source_deferred".into())
            },
        };
        store.set_value("pool_last_attempt", now)?;
        store.replace_pool(PoolUpdate {
            samples: vec![sample.clone()],
            sources: vec![
                source(Source::Debian, "ok"),
                source(Source::TpbRecent, "deferred"),
                source(Source::Hjd, "deferred"),
            ],
            collected_at: now,
        })?;
        // Upgrade from a snapshot written before per-pool freshness existed.
        store
            .private
            .execute("DELETE FROM meta WHERE k='pool_software_fresh_at'", [])?;
        store.publish(1800, &[])?;
        let read_status = |store: &Store| -> Result<Vec<PoolStatus>> {
            let raw: String = store.public.query_row(
                "SELECT payload FROM documents WHERE key='sample_pools'",
                [],
                |r| r.get(0),
            )?;
            Ok(serde_json::from_str(&raw)?)
        };
        let statuses = read_status(&store)?;
        let software = statuses.iter().find(|s| s.pool == "software").unwrap();
        assert_eq!(software.count, 1);
        assert_eq!(software.status, "partial");
        assert!(!software.stale);
        assert_eq!(software.updated_at, now);
        let forum = statuses.iter().find(|s| s.pool == "forum").unwrap();
        assert_eq!(forum.count, 0);
        assert_eq!(forum.status, "deferred");
        assert!(!forum.stale);
        assert_eq!(forum.updated_at, 0);
        assert_eq!(forum.last_attempt_at, now);
        store.set_value("pool_software_fresh_at", now - 86401)?;
        store.replace_pool(PoolUpdate {
            samples: vec![sample],
            sources: vec![source(Source::Debian, "deferred")],
            collected_at: now,
        })?;
        store.publish(1800, &[])?;
        let statuses = read_status(&store)?;
        let software = statuses.iter().find(|s| s.pool == "software").unwrap();
        assert!(software.stale);
        assert_eq!(software.updated_at, now - 86401);
        assert_eq!(software.status, "partial");
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn migration_removes_old_public_details_and_preserves_private_evidence() -> Result<()> {
        use crate::pool::{PoolKind, PoolUpdate, SampleRecord, Source};
        let dir =
            std::env::temp_dir().join(format!("tracker-private-migrate-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        store.ingest(
            &["https://example.org/announce".into()],
            &Default::default(),
        )?;
        let id = store.targets()?[0].0.clone();
        let now = unix_now();
        let sample = crate::samples::Sample {
            info_hash: [19; 20],
            piece_count: 1,
            piece_length: 1,
            total_length: 1,
        };
        store.replace_pool(PoolUpdate {
            samples: vec![SampleRecord {
                info_hash: sample.info_hash,
                pool: PoolKind::Official,
                source: Source::Debian,
                name: "Debian".into(),
                discovered_at: now,
                geometry: Some((1, 1, 1)),
            }],
            sources: vec![],
            collected_at: now,
        })?;
        let stages = vec![Observation {
            endpoint_id: id.clone(),
            observed_at: now,
            family: "ipv4".into(),
            stage: "sample_announce".into(),
            result: "accepted".into(),
            ..Default::default()
        }];
        let peer: std::net::SocketAddr = "8.8.8.8:6881".parse()?;
        store.sample_evidence(ActivityEvidence {
            endpoint: &id,
            source: "debian",
            sample: &sample,
            peers: &[peer],
            bitmaps: 1,
            coverage: Some(1.0),
            stages: &stages,
        })?;
        store.record_stats(&StatsPoint {
            at: now,
            instance: "test".into(),
            ..Default::default()
        })?;
        store.publish(1800, &[])?;
        store.private.execute_batch(
            "CREATE TABLE probe_traces(id TEXT); INSERT INTO probe_traces VALUES ('old');",
        )?;
        store.public.execute_batch("CREATE TABLE public_samples(id TEXT PRIMARY KEY,payload TEXT,updated_at INTEGER); INSERT INTO public_samples VALUES('x','magnet:?xt=urn:btih:0123456789012345678901234567890123456789',1); CREATE TABLE public_runs(id TEXT PRIMARY KEY,endpoint_id TEXT,sample_id TEXT,observed_at INTEGER,family TEXT,payload TEXT); CREATE INDEX public_runs_endpoint_time ON public_runs(endpoint_id,observed_at); INSERT INTO public_runs VALUES('r','e','x',1,'ipv4','{}');")?;
        drop(store);
        let mut store = Store::open(&dir)?;
        store.publish(1800, &[])?;
        let old:i64=store.public.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name IN ('public_samples','public_runs','public_runs_endpoint_time')",[],|r|r.get(0))?;
        assert_eq!(old, 0);
        let old_private: i64 = store.private.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='probe_traces'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(old_private, 0);
        let private: (i64, i64) = store.private.query_row(
            "SELECT handshakes,bitmap_observations FROM samples WHERE endpoint_id=?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(private, (1, 1));
        assert!(store.next_sample(&id)?.is_some());
        let endpoint: String =
            store
                .public
                .query_row("SELECT payload FROM endpoints WHERE id=?1", [&id], |r| {
                    r.get(0)
                })?;
        let endpoint: Endpoint = serde_json::from_str(&endpoint)?;
        assert_eq!(endpoint.evidence.handshake_nodes_7d, 1);
        assert_eq!(endpoint.evidence.bitmap_observations_7d, 1);
        let stats: i64 = store
            .public
            .query_row("SELECT COUNT(*) FROM stats_history", [], |r| r.get(0))?;
        assert_eq!(stats, 1);
        let pools: i64 = store.public.query_row(
            "SELECT COUNT(*) FROM documents WHERE key='sample_pools'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(pools, 1);
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn reject_secret_and_private_paths() {
        assert!(normalize("https://user:pass@example.org/announce").is_none());
        assert!(normalize("https://example.org/announce?passkey=secret").is_none());
        assert!(normalize("https://example.org/passkey/announce").is_none());
        assert!(normalize("https://example.org/tracker/custom").is_some());
        assert!(
            normalize("https://example.org/1234567890abcdef1234567890abcdef/announce").is_none()
        );
        assert!(normalize("http://127.0.0.1/announce").is_none());
        assert!(normalize("http://192.168.1.1/announce").is_none());
        assert!(normalize("http://example.internal/announce").is_none());
        assert!(normalize("https://example.org/announce").is_some());
    }
    #[test]
    fn protocol_reply_is_not_equivalent_to_http_response() {
        let mut o = Observation {
            result: "unknown".into(),
            http_status: Some(200),
            error_category: Some("tracker_rejection".into()),
            protocol_reply: true,
            ..Default::default()
        };
        assert!(protocol_reply(&o));
        assert!(attempted_protocol(&o));
        o.result = "failed".into();
        o.protocol_reply = false;
        o.error_category = Some("invalid_protocol".into());
        assert!(!protocol_reply(&o));
        assert!(attempted_protocol(&o));
        o.http_status = Some(502);
        o.error_category = Some("http_error".into());
        assert!(!protocol_reply(&o));
        o.http_status = None;
        o.error_category = Some("timeout".into());
        o.protocol_reply = true;
        assert!(protocol_reply(&o));
        o.protocol_reply = false;
        o.result = "unknown".into();
        o.http_status = Some(429);
        o.error_category = Some("rate_limited".into());
        assert!(!attempted_protocol(&o));
        o.http_status = None;
        o.error_category = Some("local_unavailable".into());
        assert!(!attempted_protocol(&o));
    }
    #[test]
    fn family_resource_admission_and_fair_activity_order() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-observatory-due-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        store.ingest(
            &[
                "https://first.example/announce".into(),
                "https://second.example/announce".into(),
            ],
            &Default::default(),
        )?;
        let first = store.activity_targets()?[0].0.clone();
        let now = unix_now();
        store.mark_activity(&first, now)?;
        let never_attempted = store.activity_targets()?[0].0.clone();
        assert_ne!(never_attempted, first);
        // Randomization is only within an hour bucket. An older bucket must
        // stay ahead of a recently attempted endpoint.
        store.mark_activity(&never_attempted, now - 7200)?;
        assert_eq!(store.activity_targets()?[0].0, never_attempted);
        store.defer_announce(&first, "ipv4", "health", 200)?;
        assert!(!store.announce_allowed(&first, "ipv4", "health", 150)?);
        assert!(store.announce_allowed(&first, "ipv6", "health", 150)?);
        assert!(store.announce_allowed(&first, "ipv4", "other-resource", 150)?);
        assert!(store.announce_allowed(&first, "ipv4", "health", 200)?);
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn migration_keeps_legacy_activity_unknown_and_new_events_out_of_health() -> Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "tracker-observatory-migrate-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir)?;
        let old = Connection::open(dir.join("private.sqlite3"))?;
        old.execute_batch("CREATE TABLE samples(id INTEGER PRIMARY KEY AUTOINCREMENT,endpoint_id TEXT NOT NULL,sample_id TEXT NOT NULL,source TEXT NOT NULL,observed_at INTEGER NOT NULL,handshakes INTEGER NOT NULL,bitmap_observations INTEGER NOT NULL,coverage REAL); INSERT INTO samples(endpoint_id,sample_id,source,observed_at,handshakes,bitmap_observations,coverage) VALUES ('old','old','legacy',1,0,0,NULL);")?;
        drop(old);
        let mut store = Store::open(&dir)?;
        let legacy: (Option<String>, Option<String>) = store.private.query_row(
            "SELECT announce_result,handshake_result FROM samples WHERE source='legacy'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(legacy, (None, None));
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
        let peer: std::net::SocketAddr = "1.1.1.1:1234".parse()?;
        let stages = vec![
            Observation {
                endpoint_id: id.clone(),
                family: "ipv4".into(),
                stage: "sample_announce".into(),
                observed_at: now,
                result: "accepted".into(),
                protocol_reply: true,
                ..Default::default()
            },
            Observation {
                endpoint_id: id.clone(),
                family: "ipv4".into(),
                stage: "handshake".into(),
                observed_at: now,
                result: "accepted".into(),
                ..Default::default()
            },
        ];
        store.sample_evidence(ActivityEvidence {
            endpoint: &id,
            source: "test",
            sample: &sample,
            peers: &[peer],
            bitmaps: 0,
            coverage: None,
            stages: &stages,
        })?;
        let empty = vec![Observation {
            endpoint_id: id.clone(),
            family: "ipv4".into(),
            stage: "sample_announce".into(),
            observed_at: now,
            result: "empty".into(),
            protocol_reply: true,
            ..Default::default()
        }];
        store.sample_evidence(ActivityEvidence {
            endpoint: &id,
            source: "empty",
            sample: &sample,
            peers: &[],
            bitmaps: 0,
            coverage: None,
            stages: &empty,
        })?;
        let timed_out = vec![Observation {
            endpoint_id: id.clone(),
            family: "ipv4".into(),
            stage: "sample_announce".into(),
            observed_at: now,
            result: "failed".into(),
            error_category: Some("timeout".into()),
            ..Default::default()
        }];
        store.sample_evidence(ActivityEvidence {
            endpoint: &id,
            source: "timeout",
            sample: &sample,
            peers: &[],
            bitmaps: 0,
            coverage: None,
            stages: &timed_out,
        })?;
        store.publish(1800, &[])?;
        let payload: String =
            store
                .public
                .query_row("SELECT payload FROM endpoints WHERE id=?1", [&id], |r| {
                    r.get(0)
                })?;
        let endpoint: Endpoint = serde_json::from_str(&payload)?;
        assert_eq!(endpoint.evidence.checks_24h, 1);
        assert_eq!(endpoint.evidence.successes_24h, 1);
        let mut q = store
            .public
            .prepare("SELECT payload FROM observations WHERE endpoint_id=?1")?;
        let public_events: Vec<String> = q
            .query_map([&id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        drop(q);
        assert_eq!(public_events.len(), 5);
        assert!(
            public_events
                .iter()
                .all(|s| !s.contains("1.1.1.1") && !s.contains(&hex::encode(sample.info_hash)))
        );
        let private_result: (String, Option<String>) = store.private.query_row(
            "SELECT announce_result,announce_error FROM samples WHERE source='empty'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(private_result, ("empty".into(), None));
        let failed: (String, Option<String>) = store.private.query_row(
            "SELECT announce_result,announce_error FROM samples WHERE source='timeout'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(failed, ("failed".into(), Some("timeout".into())));
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn public_new_uses_service_discovery_without_changing_endpoint_history() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-service-new-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let aliases = std::collections::BTreeMap::from([
            (
                "old.example.org".to_owned(),
                "canonical.example.org".to_owned(),
            ),
            (
                "new.example.org".to_owned(),
                "canonical.example.org".to_owned(),
            ),
        ]);
        let first_host = "http://same.example.org/announce";
        let alias_old = "https://old.example.org/announce";
        store.ingest(&[first_host.into(), alias_old.into()], &aliases)?;
        let old_time = unix_now() - 2 * 86400;
        store
            .private
            .execute("UPDATE endpoints SET first_seen=?1", [old_time])?;
        store.finish_baseline()?;
        store.publish(1800, &[])?;

        let same_host = "https://same.example.org/announce";
        let alias_new = "https://new.example.org/announce";
        let fresh = "https://fresh.example.org/announce";
        store.ingest(
            &[same_host.into(), alias_new.into(), fresh.into()],
            &aliases,
        )?;
        store.publish(1800, &[])?;
        fn public_endpoint(store: &Store, url: &str) -> Result<Endpoint> {
            let payload: String = store.public.query_row(
                "SELECT payload FROM endpoints WHERE url=?1",
                [url],
                |r| r.get(0),
            )?;
            Ok(serde_json::from_str(&payload)?)
        }
        let host_old = public_endpoint(&store, first_host)?;
        let host_new = public_endpoint(&store, same_host)?;
        let old_alias = public_endpoint(&store, alias_old)?;
        let new_alias = public_endpoint(&store, alias_new)?;
        let fresh_endpoint = public_endpoint(&store, fresh)?;
        assert_eq!(host_old.service_id, host_new.service_id);
        assert_eq!(old_alias.service_id, new_alias.service_id);
        for endpoint in [host_new, new_alias] {
            assert!(endpoint.baseline);
            assert_eq!(endpoint.first_seen, old_time);
        }
        assert!(!fresh_endpoint.baseline);
        assert!(fresh_endpoint.first_seen >= old_time + 86400);
        let private: (i64, i64) = store.private.query_row(
            "SELECT first_seen,baseline FROM endpoints WHERE url=?1",
            [same_host],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(private.1, 0);
        assert!(private.0 >= old_time + 86400);

        // A newly joined older endpoint must also invalidate a cached sibling.
        let sibling = "http://fresh.example.org/announce";
        store.ingest(&[sibling.into()], &aliases)?;
        store.private.execute(
            "UPDATE endpoints SET first_seen=?1 WHERE url=?2",
            params![old_time, sibling],
        )?;
        store.publish(1800, &[])?;
        assert_eq!(public_endpoint(&store, fresh)?.first_seen, old_time);
        let projected: (i64, i64) = store.public.query_row(
            "SELECT first_seen,baseline FROM endpoints WHERE url=?1",
            [same_host],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(projected, (old_time, 1));
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn baseline_freshness_and_public_projection() -> Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "tracker-observatory-test-{}",
            rand::random::<u64>()
        ));
        let mut store = Store::open(&dir)?;
        store.ingest(
            &["https://example.org/announce".into()],
            &Default::default(),
        )?;
        let id = store.targets()?[0].0.clone();
        let now = unix_now();
        store.record(&Observation {
            observed_at: now,
            endpoint_id: id.clone(),
            family: "ipv4".into(),
            stage: "announce".into(),
            result: "accepted".into(),
            http_status: Some(200),
            latency_ms: Some(10.0),
            ..Default::default()
        })?;
        store.publish(1800, &[])?;
        let endpoint: String =
            store
                .public
                .query_row("SELECT payload FROM endpoints", [], |r| r.get(0))?;
        let endpoint: Endpoint = serde_json::from_str(&endpoint)?;
        assert!(endpoint.baseline);
        assert!(!endpoint.evidence.standard_eligible);
        let list: String = store.public.query_row(
            "SELECT payload FROM documents WHERE key='feed:trackers_all.txt'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(
            serde_json::from_str::<String>(&list)?,
            "https://example.org/announce"
        );
        let standard: String = store.public.query_row(
            "SELECT payload FROM documents WHERE key='feed:trackers.txt'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(serde_json::from_str::<String>(&standard)?, "");
        let event_id: i64 = store
            .public
            .query_row("SELECT id FROM observations", [], |r| r.get(0))?;
        store.publish(1800, &[])?;
        let (events, same_id): (i64, i64) =
            store
                .public
                .query_row("SELECT COUNT(*),MAX(id) FROM observations", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
        assert_eq!((events, same_id), (1, event_id));
        store.record(&Observation {
            observed_at: now,
            endpoint_id: id.clone(),
            family: "ipv6".into(),
            stage: "announce".into(),
            result: "failed".into(),
            error_category: Some("timeout".into()),
            protocol_reply: true,
            ..Default::default()
        })?;
        store.publish(1800, &[])?;
        let updated: String =
            store
                .public
                .query_row("SELECT payload FROM endpoints WHERE id=?1", [&id], |r| {
                    r.get(0)
                })?;
        let updated: Endpoint = serde_json::from_str(&updated)?;
        assert_eq!(updated.evidence.checks_24h, 2);
        assert_eq!(updated.evidence.replies_24h, 1);
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    fn target_id(store: &Store, url: &str) -> Result<String> {
        store
            .targets()?
            .into_iter()
            .find(|(_, candidate, _)| candidate == url)
            .map(|(id, _, _)| id)
            .ok_or_else(|| anyhow::anyhow!("missing endpoint {url}"))
    }
    fn announce(
        store: &mut Store,
        endpoint: &str,
        result: &str,
        observed_at: i64,
        target_ip: &str,
    ) -> Result<()> {
        store.record(&Observation {
            observed_at,
            endpoint_id: endpoint.to_owned(),
            family: "ipv4".into(),
            stage: "announce".into(),
            result: result.into(),
            protocol_reply: result == "accepted",
            latency_ms: (result == "accepted").then_some(10.0),
            error_category: (result != "accepted").then(|| "timeout".into()),
            target_ip: Some(target_ip.into()),
            ..Default::default()
        })
    }
    fn published_endpoint(store: &Store, url: &str) -> Result<Endpoint> {
        let payload: String =
            store
                .public
                .query_row("SELECT payload FROM endpoints WHERE url=?1", [url], |r| {
                    r.get(0)
                })?;
        Ok(serde_json::from_str(&payload)?)
    }
    fn published_feed(store: &Store, name: &str) -> Result<String> {
        let payload: String = store.public.query_row(
            "SELECT payload FROM documents WHERE key=?1",
            [format!("feed:{name}")],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&payload)?)
    }
    fn published_overview(store: &Store) -> Result<Overview> {
        let payload: String = store.public.query_row(
            "SELECT payload FROM documents WHERE key='overview'",
            [],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&payload)?)
    }

    #[test]
    fn health_queue_prioritizes_only_families_that_are_due() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-health-queue-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let a = "https://a.example.org/announce";
        let b = "https://b.example.org/announce";
        store.ingest(&[a.into(), b.into()], &Default::default())?;
        let a = target_id(&store, a)?;
        let b = target_id(&store, b)?;
        let now = unix_now();
        announce(&mut store, &a, "accepted", now - 1, "203.0.113.10")?;
        announce(&mut store, &b, "accepted", now - 20, "203.0.113.11")?;
        store.defer_announce(&a, "ipv4", "health", now + 10)?;
        store.defer_announce(&a, "ipv6", "health", now - 120)?;
        store.defer_announce(&b, "ipv4", "health", now - 5)?;
        store.defer_announce(&b, "ipv6", "health", now - 3)?;
        store.defer_announce(&a, "ipv6", "unrelated-sample", now + 7200)?;

        let due = store.due_targets(now)?;
        assert_eq!(due.len(), 2);
        assert_eq!((&due[0].0, due[0].2), (&a, true));
        assert_eq!((&due[1].0, due[1].2), (&b, false));
        assert_eq!(store.health_wake_delay(now)?, 5);

        store.defer_announce(&a, "ipv6", "health", now + 40)?;
        store.pause("a.example.org", now + 15)?;
        store.pause("b.example.org", now + 45)?;
        assert!(
            store
                .due_targets(now)?
                .iter()
                .all(|(endpoint, _, _)| endpoint != &a)
        );
        assert_eq!(store.health_wake_delay(now)?, 15);
        let due = store.due_targets(now + 15)?;
        assert_eq!((&due[1].0, due[1].2), (&a, false));
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn health_schedule_updates_a_cached_snapshot_without_changing_evidence() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-health-public-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let https = "https://plan.example.org/announce";
        let udp = "udp://plan.example.org:6969/announce";
        store.ingest(&[https.into(), udp.into()], &Default::default())?;
        let now = unix_now();
        let endpoint = target_id(&store, https)?;
        let other = target_id(&store, udp)?;
        announce(
            &mut store,
            &endpoint,
            "accepted",
            now - 4500,
            "203.0.113.12",
        )?;
        store.publish(1800, &[])?;
        let previous = published_endpoint(&store, https)?;
        let feed = published_feed(&store, "trackers_all.txt")?;
        let fresh_deadline = previous.families[0].valid_until;
        assert_eq!(previous.families[0].next_check_at, None);
        assert_eq!(previous.families[0].state, "unknown");
        let cached_refresh = |store: &Store| -> Result<i64> {
            Ok(store.public.query_row(
                "SELECT next_refresh FROM publication_cache WHERE endpoint_id=?1",
                [&endpoint],
                |row| row.get(0),
            )?)
        };
        let refresh = cached_refresh(&store)?;
        store.defer_announce(&endpoint, "ipv4", "health", now + 600)?;
        store.defer_announce(&endpoint, "ipv6", "health", now + 1200)?;
        store.defer_announce(&other, "ipv4", "health", now + 2400)?;
        store.publish(1800, &[])?;
        let updated = published_endpoint(&store, https)?;
        assert_eq!(updated.families[0].next_check_at, Some(now + 600));
        assert_eq!(updated.families[1].next_check_at, Some(now + 1200));
        assert_eq!(updated.families[0].valid_until, fresh_deadline);
        assert_eq!(updated.families[0].state, previous.families[0].state);
        assert_eq!(updated.evidence.success_rate_24h, Some(1.0));
        assert_eq!(published_feed(&store, "trackers_all.txt")?, feed);
        assert_eq!(cached_refresh(&store)?, refresh);

        store.pause("plan.example.org", now + 1800)?;
        store.publish(1800, &[])?;
        let updated = published_endpoint(&store, https)?;
        let sibling = published_endpoint(&store, udp)?;
        assert_eq!(updated.families[0].next_check_at, Some(now + 1800));
        assert_eq!(updated.families[1].next_check_at, Some(now + 1800));
        assert_eq!(sibling.families[0].next_check_at, Some(now + 2400));
        assert_eq!(sibling.families[1].next_check_at, Some(now + 1800));
        assert_eq!(cached_refresh(&store)?, refresh);
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn ghost_sample_verdict_is_recorded_but_does_not_gate_feeds() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-ghost-verdict-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let url = "https://flagged.example.org/announce";
        store.ingest(&[url.into()], &Default::default())?;
        let now = unix_now();
        let id = target_id(&store, url)?;
        announce(&mut store, &id, "accepted", now, "203.0.113.20")?;

        // Production records the verdict as a `sample_announce` stage through
        // `sample_evidence`; it never becomes a health `announce` event.
        let ghost = crate::pool::ghost_samples(now).remove(0);
        assert!(crate::pool::is_ghost_hash(&ghost.info_hash));
        let sample = crate::samples::Sample {
            info_hash: ghost.info_hash,
            piece_count: 0,
            piece_length: 0,
            total_length: 0,
        };
        let stages = vec![Observation {
            endpoint_id: id.clone(),
            family: "ipv4".into(),
            stage: "sample_announce".into(),
            observed_at: now,
            result: "failed".into(),
            protocol_reply: true,
            error_category: Some("poisoned_peers".into()),
            ..Default::default()
        }];
        store.sample_evidence(ActivityEvidence {
            endpoint: &id,
            source: "hjd",
            sample: &sample,
            peers: &[],
            bitmaps: 0,
            coverage: None,
            stages: &stages,
        })?;
        let recorded: Option<String> = store.private.query_row(
            "SELECT announce_error FROM samples WHERE endpoint_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        assert_eq!(recorded.as_deref(), Some("poisoned_peers"));

        // The verdict is a diagnostic. It must not change feed membership or
        // the health figures, on a fresh publication or on the cached path.
        for _ in 0..2 {
            store.publish(1800, &[])?;
            assert_eq!(published_feed(&store, "trackers_all.txt")?, url);
            assert_eq!(published_overview(&store)?.all_count, 1);
            let endpoint = published_endpoint(&store, url)?;
            assert_eq!(endpoint.evidence.checks_24h, 1);
            assert_eq!(endpoint.evidence.successes_24h, 1);
        }
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn target_ip_roundtrips_private_and_public_observation_payloads() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("tracker-target-ip-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        store.ingest(
            &["https://example.org/announce".into()],
            &Default::default(),
        )?;
        let id = store.targets()?[0].0.clone();
        let now = unix_now();
        announce(&mut store, &id, "accepted", now, "8.8.8.8")?;
        let private: String = store.private.query_row(
            "SELECT payload FROM observations WHERE endpoint_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        assert!(private.contains("\"target_ip\":\"8.8.8.8\""));
        let recorded: Observation = serde_json::from_str(&private)?;
        assert_eq!(recorded.target_ip.as_deref(), Some("8.8.8.8"));
        store.publish(1800, &[])?;
        let public: String = store.public.query_row(
            "SELECT payload FROM observations WHERE endpoint_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        let projected: Observation = serde_json::from_str(&public)?;
        assert_eq!(projected.target_ip.as_deref(), Some("8.8.8.8"));
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn hostname_and_literal_ip_merge_into_one_probed_origin() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-alias-http-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let hostname = "https://tracker.example.org:6969/announce";
        let literal = "https://8.8.8.8:6969/announce";
        store.ingest(&[hostname.into(), literal.into()], &Default::default())?;
        assert_eq!(store.targets()?.len(), 2);
        let hostname_id = target_id(&store, hostname)?;
        let literal_id = target_id(&store, literal)?;
        let now = unix_now();
        announce(&mut store, &hostname_id, "accepted", now - 10, "8.8.8.8")?;
        announce(&mut store, &literal_id, "accepted", now - 10, "8.8.8.8")?;
        store.publish(1800, &[])?;
        let aliases = store.alias_map()?;
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases.get(&literal_id), Some(&hostname_id));
        // The alias is still published by id, but resolves to the hostname.
        assert_eq!(published_endpoint(&store, hostname)?.alias_of, None);
        assert_eq!(
            published_endpoint(&store, literal)?.alias_of.as_deref(),
            Some(hostname_id.as_str())
        );
        // Only the canonical origin is handed out as probe or activity work.
        let due = store.due_targets(now)?;
        assert!(due.iter().any(|(id, _, _)| id == &hostname_id));
        assert!(!due.iter().any(|(id, _, _)| id == &literal_id));
        assert!(
            !store
                .activity_targets()?
                .iter()
                .any(|(id, _)| id == &literal_id)
        );
        let targets = store.targets()?;
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].0, hostname_id);
        // Feeds and overview describe origins, never aliases.
        assert_eq!(published_feed(&store, "trackers_all.txt")?, hostname);
        let overview = published_overview(&store)?;
        assert_eq!(
            (overview.endpoints, overview.services, overview.all_count),
            (1, 1, 1)
        );
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn dual_stack_family_failure_keeps_the_hostname_canonical() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-alias-dual-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let hostname = "https://tracker.example.org:6969/announce";
        let literal = "https://8.8.8.8:6969/announce";
        store.ingest(&[hostname.into(), literal.into()], &Default::default())?;
        let hostname_id = target_id(&store, hostname)?;
        let literal_id = target_id(&store, literal)?;
        let now = unix_now();
        announce(&mut store, &hostname_id, "accepted", now - 20, "8.8.8.8")?;
        announce(&mut store, &literal_id, "accepted", now - 20, "8.8.8.8")?;
        // The most recent check of the hostname is the family it cannot serve.
        // That must not demote the working IPv4 hostname to a literal canonical.
        store.record(&Observation {
            observed_at: now - 5,
            endpoint_id: hostname_id.clone(),
            family: "ipv6".into(),
            stage: "announce".into(),
            result: "unsupported".into(),
            protocol_reply: false,
            http_status: None,
            latency_ms: None,
            error_category: Some("family_unavailable".into()),
            target_ip: None,
        })?;
        store.publish(1800, &[])?;
        let aliases = store.alias_map()?;
        assert_eq!(
            aliases.get(&literal_id),
            Some(&hostname_id),
            "aliases were {aliases:?}"
        );
        assert_eq!(published_endpoint(&store, hostname)?.alias_of, None);
        // Another cycle with unchanged evidence keeps the same direction.
        store.refresh_aliases(now + 600, &[])?;
        assert_eq!(store.alias_map()?.get(&literal_id), Some(&hostname_id));
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn canonical_hostname_choice_is_stable_across_refreshes() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-alias-stable-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let first = "udp://first.example.org:6969/announce";
        let second = "udp://second.example.org:6969/announce";
        store.ingest(&[first.into(), second.into()], &Default::default())?;
        let first_id = target_id(&store, first)?;
        let second_id = target_id(&store, second)?;
        let now = unix_now();
        announce(&mut store, &first_id, "accepted", now - 40, "8.8.8.8")?;
        announce(&mut store, &second_id, "accepted", now - 40, "8.8.8.8")?;
        store.publish(1800, &[])?;
        let aliases = store.alias_map()?;
        assert_eq!(
            aliases.get(&second_id),
            Some(&first_id),
            "aliases were {aliases:?}"
        );
        // More successful probes for `second` would win a fresh ranking, but
        // the existing canonical stays while it is still a healthy candidate.
        announce(&mut store, &second_id, "accepted", now - 20, "8.8.8.8")?;
        announce(&mut store, &second_id, "accepted", now - 15, "8.8.8.8")?;
        announce(&mut store, &second_id, "accepted", now - 10, "8.8.8.8")?;
        store.publish(1800, &[])?;
        store.refresh_aliases(now + 600, &[])?;
        assert_eq!(
            store.alias_map()?.get(&second_id),
            Some(&first_id),
            "canonical flipped although it was still healthy"
        );
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn own_service_hosts_are_never_aliased() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("tracker-alias-own-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let official = "udp://udp.augrains.com:6969/announce";
        let retired = "udp://announce.augrains.com:6969/announce";
        store.ingest(&[official.into(), retired.into()], &Default::default())?;
        let official_id = target_id(&store, official)?;
        let retired_id = target_id(&store, retired)?;
        let now = unix_now();
        announce(&mut store, &official_id, "accepted", now - 10, "8.8.8.8")?;
        announce(&mut store, &retired_id, "accepted", now - 10, "8.8.8.8")?;
        store.publish(1800, &["udp.augrains.com".to_owned()])?;
        // The configured public entry point must not disappear behind a
        // retired name that happens to resolve to the same address.
        let aliases = store.alias_map()?;
        assert_eq!(aliases.get(&retired_id), Some(&official_id));
        assert!(!aliases.contains_key(&official_id));
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn retired_own_endpoints_are_aliased_on_first_publication_after_restart() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-alias-retired-{}", rand::random::<u64>()));
        let pairs = [
            (
                "http://announce.augrains.com/announce",
                "http://tracker.augrains.com/announce",
            ),
            (
                "https://announce.augrains.com/announce",
                "https://tracker.augrains.com/announce",
            ),
            (
                "udp://announce.augrains.com:6969/announce",
                "udp://udp.augrains.com:6969/announce",
            ),
        ];
        let own = vec![
            "tracker.augrains.com".to_string(),
            "udp.augrains.com".to_string(),
        ];
        let mut store = Store::open(&dir)?;
        let mut urls: Vec<String> = pairs
            .iter()
            .flat_map(|(old, new)| [old.to_string(), new.to_string()])
            .collect();
        let other_host = "https://other.example.org:6969/announce";
        let other_ip = "https://8.8.8.8:6969/announce";
        urls.extend([other_host.into(), other_ip.into()]);
        store.ingest(&urls, &Default::default())?;
        let now = unix_now();
        for url in &urls {
            let endpoint_id = target_id(&store, url)?;
            announce(&mut store, &endpoint_id, "accepted", now - 10, "8.8.8.8")?;
        }
        // Model a previously running version with a recent cached alias refresh.
        store.publish(1800, &[])?;
        store.set_value("alias_checked_at", now)?;
        drop(store);

        let mut store = Store::open(&dir)?;
        store.publish(1800, &own)?;
        let aliases = store.alias_map()?;
        let due = store.due_targets(now)?;
        let activity = store.activity_targets()?;
        let all = published_feed(&store, "trackers_all.txt")?;
        assert_eq!(
            aliases.get(&published_endpoint(&store, other_ip)?.id),
            Some(&published_endpoint(&store, other_host)?.id)
        );
        for (old, new) in pairs {
            let old_id = published_endpoint(&store, old)?.id;
            let new_id = published_endpoint(&store, new)?.id;
            assert_eq!(aliases.get(&old_id), Some(&new_id));
            assert!(!aliases.contains_key(&new_id));
            assert_eq!(
                published_endpoint(&store, old)?.alias_of.as_deref(),
                Some(new_id.as_str())
            );
            assert_eq!(published_endpoint(&store, new)?.alias_of, None);
            assert!(!due.iter().any(|(id, _, _)| id == &old_id));
            assert!(!activity.iter().any(|(id, _)| id == &old_id));
            assert!(due.iter().any(|(id, _, _)| id == &new_id));
            assert!(!all.lines().any(|url| url == old));
            let history: i64 = store.private.query_row(
                "SELECT COUNT(*) FROM observations WHERE endpoint_id=?1",
                [&old_id],
                |r| r.get(0),
            )?;
            assert_eq!(history, 1);
        }
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn retired_alias_requires_configured_owner_and_existing_canonical() -> Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "tracker-alias-retired-seed-{}",
            rand::random::<u64>()
        ));
        let mut store = Store::open(&dir)?;
        let old = "http://announce.augrains.com/announce";
        let new = "http://tracker.augrains.com/announce";
        let own = vec!["tracker.augrains.com".to_string()];
        store.ingest(&[old.into()], &Default::default())?;
        store.publish(1800, &own)?;
        assert!(store.alias_map()?.is_empty());
        assert!(
            store
                .due_targets(unix_now())?
                .iter()
                .any(|(_, url, _)| url == old)
        );
        store.ingest(&[new.into()], &Default::default())?;
        // Ingest invalidates a recent refresh so the next publication applies it.
        store.publish(1800, &own)?;
        let old_id = published_endpoint(&store, old)?.id;
        let new_id = published_endpoint(&store, new)?.id;
        assert_eq!(store.alias_map()?.get(&old_id), Some(&new_id));
        store.refresh_aliases(unix_now(), &[])?;
        assert!(!store.alias_map()?.contains_key(&old_id));
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn failing_hostname_does_not_absorb_a_working_literal_ip() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-alias-health-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let hostname = "https://tracker.example.org:6969/announce";
        let literal = "https://8.8.8.8:6969/announce";
        store.ingest(&[hostname.into(), literal.into()], &Default::default())?;
        let hostname_id = target_id(&store, hostname)?;
        let literal_id = target_id(&store, literal)?;
        let now = unix_now();
        // The hostname announces successfully once, then its latest announce
        // times out; the literal IP keeps working.
        announce(&mut store, &hostname_id, "accepted", now - 600, "8.8.8.8")?;
        announce(&mut store, &hostname_id, "failed", now - 10, "8.8.8.8")?;
        announce(&mut store, &literal_id, "accepted", now - 10, "8.8.8.8")?;
        store.publish(1800, &[])?;
        assert!(store.alias_map()?.is_empty());
        assert_eq!(published_endpoint(&store, hostname)?.alias_of, None);
        assert_eq!(published_endpoint(&store, literal)?.alias_of, None);
        assert_eq!(store.due_targets(now)?.len(), 2);
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn different_hostname_vhosts_on_one_http_address_stay_distinct() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("tracker-alias-vhost-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let alpha = "https://alpha.example.org:6969/announce";
        let beta = "https://beta.example.org:6969/announce";
        store.ingest(&[alpha.into(), beta.into()], &Default::default())?;
        let alpha_id = target_id(&store, alpha)?;
        let beta_id = target_id(&store, beta)?;
        let now = unix_now();
        announce(&mut store, &alpha_id, "accepted", now - 10, "8.8.4.4")?;
        announce(&mut store, &beta_id, "accepted", now - 10, "8.8.4.4")?;
        store.publish(1800, &[])?;
        assert!(store.alias_map()?.is_empty());
        assert_eq!(store.targets()?.len(), 2);
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn duplicate_udp_hostnames_merge_into_the_healthy_canonical() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("tracker-alias-udp-{}", rand::random::<u64>()));
        let mut store = Store::open(&dir)?;
        let first = "udp://one-a.example.org:6969/announce";
        let first_duplicate = "udp://one-b.example.org:6969/announce";
        let second = "udp://two-a.example.org:6970/announce";
        let second_duplicate = "udp://two-b.example.org:6970/announce";
        store.ingest(
            &[
                first.into(),
                first_duplicate.into(),
                second.into(),
                second_duplicate.into(),
            ],
            &Default::default(),
        )?;
        let first_id = target_id(&store, first)?;
        let first_duplicate_id = target_id(&store, first_duplicate)?;
        let second_id = target_id(&store, second)?;
        let second_duplicate_id = target_id(&store, second_duplicate)?;
        let now = unix_now();
        // Both members healthy: the first URL wins the canonical tie-break.
        announce(&mut store, &first_id, "accepted", now - 10, "9.9.9.9")?;
        announce(
            &mut store,
            &first_duplicate_id,
            "accepted",
            now - 10,
            "9.9.9.9",
        )?;
        // Only the second member is healthy, so it becomes canonical.
        announce(&mut store, &second_id, "accepted", now - 600, "8.8.4.4")?;
        announce(&mut store, &second_id, "failed", now - 10, "8.8.4.4")?;
        announce(
            &mut store,
            &second_duplicate_id,
            "accepted",
            now - 10,
            "8.8.4.4",
        )?;
        store.publish(1800, &[])?;
        let aliases = store.alias_map()?;
        assert_eq!(aliases.len(), 2);
        assert_eq!(aliases.get(&first_duplicate_id), Some(&first_id));
        assert_eq!(aliases.get(&second_id), Some(&second_duplicate_id));
        let due = store.due_targets(now)?;
        assert!(due.iter().any(|(id, _, _)| id == &first_id));
        assert!(due.iter().any(|(id, _, _)| id == &second_duplicate_id));
        assert!(!due.iter().any(|(id, _, _)| id == &first_duplicate_id));
        assert!(!due.iter().any(|(id, _, _)| id == &second_id));
        let all = published_feed(&store, "trackers_all.txt")?;
        assert!(all.contains(first) && all.contains(second_duplicate));
        assert!(!all.contains(first_duplicate) && !all.contains(second));
        let overview = published_overview(&store)?;
        assert_eq!((overview.endpoints, overview.services), (2, 2));
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}
