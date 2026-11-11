//! Shared contracts. Sample hashes, peer addresses and credentials stay private.
use serde::{Deserialize, Serialize};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PUBLIC_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS endpoints(id TEXT PRIMARY KEY, service_id TEXT NOT NULL, url TEXT NOT NULL, first_seen INTEGER NOT NULL, baseline INTEGER NOT NULL, updated_at INTEGER NOT NULL, payload TEXT NOT NULL); CREATE INDEX IF NOT EXISTS endpoints_service ON endpoints(service_id); CREATE TABLE IF NOT EXISTS observations(id INTEGER PRIMARY KEY, endpoint_id TEXT NOT NULL, observed_at INTEGER NOT NULL, family TEXT NOT NULL, payload TEXT NOT NULL); CREATE INDEX IF NOT EXISTS observations_endpoint_time ON observations(endpoint_id,observed_at); CREATE TABLE IF NOT EXISTS documents(key TEXT PRIMARY KEY,payload TEXT NOT NULL,updated_at INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS stats_history(bucket INTEGER PRIMARY KEY,instance TEXT NOT NULL,payload TEXT NOT NULL);";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FamilyStatus {
    pub family: String,
    pub state: String,
    pub checked_at: Option<i64>,
    pub valid_until: Option<i64>,
    pub http_status: Option<u16>,
    pub protocol_result: Option<String>,
    pub error_category: Option<String>,
    pub latency_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Evidence {
    pub checks_24h: u64,
    pub successes_24h: u64,
    pub replies_24h: u64,
    #[serde(default)]
    pub reply_checks_24h: u64,
    pub success_rate_24h: Option<f64>,
    pub reply_rate_24h: Option<f64>,
    pub median_latency_ms: Option<f64>,
    pub checks_7d: u64,
    pub successes_7d: u64,
    pub success_rate_7d: Option<f64>,
    pub observation_span_secs: u64,
    pub handshake_nodes_7d: u64,
    pub sample_count_7d: u64,
    pub source_count_7d: u64,
    pub bitmap_observations_7d: u64,
    pub coverage_mean: Option<f64>,
    pub last_handshake_at: Option<i64>,
    pub standard_eligible: bool,
    pub best_eligible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Infrastructure {
    pub asn: Option<String>,
    pub organization: Option<String>,
    pub country: Option<String>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Endpoint {
    pub id: String,
    pub service_id: String,
    pub url: String,
    pub protocol: String,
    pub host: String,
    pub first_seen: i64,
    pub baseline: bool,
    pub updated_at: i64,
    pub colocated: bool,
    pub families: Vec<FamilyStatus>,
    pub evidence: Evidence,
    pub infrastructure: Infrastructure,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Observation {
    pub observed_at: i64,
    pub endpoint_id: String,
    pub family: String,
    pub stage: String,
    pub result: String,
    #[serde(default)]
    pub protocol_reply: bool,
    pub http_status: Option<u16>,
    pub latency_ms: Option<f64>,
    pub error_category: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Overview {
    pub version: String,
    pub generated_at: i64,
    pub endpoints: usize,
    pub services: usize,
    pub standard_count: usize,
    pub best_count: usize,
    pub all_count: usize,
    pub warming: bool,
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PoolStatus {
    pub pool: String,
    pub count: usize,
    pub target: usize,
    pub updated_at: i64,
    pub last_attempt_at: i64,
    pub stale: bool,
    pub status: String,
    pub error_category: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatsPoint {
    pub at: i64,
    pub instance: String,
    pub counters: std::collections::BTreeMap<String, u64>,
    pub gauges: std::collections::BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatsHistory {
    pub window: String,
    pub step_seconds: u64,
    pub items: Vec<StatsPoint>,
}
