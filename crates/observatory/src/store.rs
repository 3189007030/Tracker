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
        tx.commit()?;
        Ok(added)
    }
    pub fn finish_baseline(&self) -> Result<()> {
        self.private.execute("INSERT OR IGNORE INTO meta SELECT 'baseline','complete' WHERE EXISTS(SELECT 1 FROM endpoints)",[])?;
        Ok(())
    }
    pub fn targets(&self) -> Result<Vec<(String, String, Option<i64>)>> {
        let mut q = self.private.prepare("SELECT e.id,e.url,MAX(o.observed_at) FROM endpoints e LEFT JOIN observations o ON o.endpoint_id=e.id AND json_extract(o.payload,'$.stage')='announce' GROUP BY e.id ORDER BY MAX(o.observed_at) ASC LIMIT 256")?;
        let rows = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    pub fn due_targets(&self, now: i64) -> Result<Vec<(String, String, Option<i64>)>> {
        let mut q=self.private.prepare("SELECT e.id,e.url,MAX(o.observed_at) FROM endpoints e LEFT JOIN observations o ON o.endpoint_id=e.id AND json_extract(o.payload,'$.stage')='announce' WHERE NOT EXISTS (SELECT 1 FROM announce_due d WHERE d.endpoint_id=e.id AND d.family='ipv4' AND d.resource='health' AND d.until>?1) OR NOT EXISTS (SELECT 1 FROM announce_due d WHERE d.endpoint_id=e.id AND d.family='ipv6' AND d.resource='health' AND d.until>?1) GROUP BY e.id ORDER BY MAX(o.observed_at) ASC LIMIT 20000")?;
        let rows = q
            .query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    pub fn activity_targets(&self) -> Result<Vec<(String, String)>> {
        let mut q=self.private.prepare("SELECT e.id,e.url FROM endpoints e LEFT JOIN activity_cursor c ON c.endpoint_id=e.id ORDER BY COALESCE(c.last_attempt,0)/3600, RANDOM() LIMIT 20000")?;
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
    pub fn publish(&mut self, interval: u64, own_hosts: &[String]) -> Result<()> {
        let now = unix_now();
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
        let mut query = self
            .private
            .prepare("SELECT id,service_id,url,first_seen,baseline FROM endpoints ORDER BY url")?;
        let rows = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(query);
        // Public NEW describes the stable service, not a newly added endpoint.
        // Keep the endpoint's own first_seen and baseline in the private table.
        let mut service_discovery = std::collections::HashMap::<&str, (i64, bool)>::new();
        for (_, service_id, _, first_seen, baseline) in &rows {
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
                    target: 100,
                    updated_at: at,
                    last_attempt_at: pool_last_attempt,
                    stale,
                    status: if count == 0 {
                        "deferred"
                    } else if count < 100 || stale || failed {
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
        for (endpoint_id, service_id, url, first_seen, _baseline) in &rows {
            let (service_first_seen, service_baseline) = service_discovery[service_id.as_str()];
            let parsed = Url::parse(url)?;
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
                && let Ok(endpoint) = serde_json::from_str::<Endpoint>(&payload)
                && endpoint.first_seen == service_first_seen
                && endpoint.baseline == service_baseline
            {
                let score = endpoint.evidence.success_rate_24h.unwrap_or(0.0);
                let candidate = (
                    service_id.clone(),
                    url.clone(),
                    score,
                    endpoint.evidence.sample_count_7d as usize,
                    endpoint.evidence.median_latency_ms.unwrap_or(f64::INFINITY),
                );
                if all_eligible != 0 {
                    all_candidates.push(candidate.clone());
                }
                if endpoint.evidence.standard_eligible {
                    standard_candidates.push(candidate.clone());
                }
                if endpoint.evidence.best_eligible {
                    best_candidates.push(candidate);
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
                        http_status: o.http_status,
                        protocol_result: Some(o.result.clone()),
                        error_category: o.error_category.clone(),
                        latency_ms: o.latency_ms,
                    });
                } else {
                    family_status.push(FamilyStatus {
                        family: family.into(),
                        state: "unknown".into(),
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
            let standard = family_status
                .iter()
                .all(|f| f.state == "accepted" || f.state == "not_published")
                && family_status.iter().any(|f| f.state == "accepted")
                && recent.len() >= 4
                && successes * 100 >= 95 * recent.len()
                && recent_samples.len() >= 2;
            let best = standard
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
            if observations
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
        let overview = Overview {
            version: VERSION.into(),
            generated_at: now,
            endpoints: rows.len(),
            services: rows
                .iter()
                .map(|r| &r.1)
                .collect::<std::collections::HashSet<_>>()
                .len(),
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
}
