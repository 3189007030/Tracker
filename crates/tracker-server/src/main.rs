//! Stateless protocol adapters around one shared, sharded, volatile registry.
use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, OriginalUri, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::Response,
    routing::get,
};
use bt_protocol::*;
use clap::Parser;
use serde::Deserialize;
use serde_json::json;
use std::{
    borrow::Cow,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use swarm_engine::{EngineConfig, EngineError, SwarmEngine};
use tokio::{
    net::{TcpListener, UdpSocket},
    sync::Semaphore,
};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/etc/tracker-v1/config.toml")]
    config: PathBuf,
    #[arg(long)]
    http: Option<SocketAddr>,
    #[arg(long)]
    udp_v4: Option<SocketAddr>,
    #[arg(long)]
    udp_v6: Option<SocketAddr>,
    #[arg(long)]
    allow_private_for_tests: bool,
}

#[derive(Clone, Deserialize)]
#[serde(default)]
struct Settings {
    http: SocketAddr,
    udp_v4: SocketAddr,
    udp_v6: SocketAddr,
    max_peers: usize,
    max_swarms: usize,
    max_peers_per_swarm: usize,
    memory_budget_mib: usize,
    per_ip_rate: u64,
    http_concurrency: usize,
    maintenance_budget: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            http: "127.0.0.1:8081".parse().unwrap(),
            udp_v4: "0.0.0.0:6969".parse().unwrap(),
            udp_v6: "[::]:6969".parse().unwrap(),
            max_peers: 2_000_000,
            max_swarms: 2_000_000,
            max_peers_per_swarm: 10_000,
            memory_budget_mib: 1024,
            per_ip_rate: 20_000,
            http_concurrency: 4096,
            maintenance_budget: 256,
        }
    }
}

// Service timing and protocol counters are grouped by the task that writes
// them. Adjacent atomics shared a cache line, so the two UDP loops, the HTTP
// handlers and the maintenance task invalidated each other's line on every
// update. Each group is cache-line aligned; only names move, not values.
#[repr(align(64))]
#[derive(Default)]
struct ServiceCounters {
    micros_sum: AtomicU64,
    count: AtomicU64,
    buckets: [AtomicU64; 10], // one entry per BUCKET_US bound
}
#[repr(align(64))]
#[derive(Default)]
struct AcceptCounters {
    accepted: AtomicU64,
    ipv4: AtomicU64,
    ipv6: AtomicU64,
}
#[repr(align(64))]
#[derive(Default)]
struct FailureCounters {
    business: AtomicU64,
    engine: [AtomicU64; 7], // one entry per ENGINE_FAILURE_KEYS reason
    invalid_packets: AtomicU64,
    http_invalid_queries: AtomicU64,
    udp_malformed_packets: AtomicU64,
    udp_cookie_window: AtomicU64,
    udp_cookie_auth: AtomicU64,
}
#[repr(align(64))]
#[derive(Default)]
struct UdpCounters {
    connects: AtomicU64,
    announces: AtomicU64,
    v4_packets: AtomicU64,
    v6_packets: AtomicU64,
    rx_bytes: AtomicU64,
    tx_bytes: AtomicU64,
    send_errors: AtomicU64,
}
#[repr(align(64))]
#[derive(Default)]
struct RequestCounters {
    http_announces: AtomicU64,
    scrapes: AtomicU64,
    rate_limited: AtomicU64,
    http_busy: AtomicU64,
}
#[repr(align(64))]
#[derive(Default)]
struct MaintenanceCounters {
    count: AtomicU64,
    micros_sum: AtomicU64,
    micros_max: AtomicU64,
}
#[derive(Default)]
struct Counters {
    service: ServiceCounters,
    accepts: AcceptCounters,
    failures: FailureCounters,
    udp: UdpCounters,
    requests: RequestCounters,
    maintenance: MaintenanceCounters,
}
struct ServiceTimer<'a> {
    counters: &'a ServiceCounters,
    started: Instant,
}
impl<'a> ServiceTimer<'a> {
    fn new(counters: &'a ServiceCounters) -> Self {
        Self {
            counters,
            started: Instant::now(),
        }
    }
}
impl Drop for ServiceTimer<'_> {
    fn drop(&mut self) {
        self.counters.observe(self.started);
    }
}
const BUCKET_US: [u64; 10] = [
    100,
    500,
    1000,
    2000,
    5000,
    10_000,
    20_000,
    50_000,
    100_000,
    u64::MAX,
];
/// Index of the first bucket whose upper bound covers `micros`.
///
/// `BUCKET_US` is ascending, so partitioning on `bound < micros` lands on the
/// same bucket as the linear scan it replaces: the first bound that is not
/// below the sample. The clamp preserves the old `unwrap_or` tail bucket.
fn bucket(micros: u64) -> usize {
    BUCKET_US
        .partition_point(|bound| *bound < micros)
        .min(BUCKET_US.len() - 1)
}
const ENGINE_FAILURE_KEYS: [&str; 7] = [
    "invalid_address",
    "invalid_port",
    "peer_capacity",
    "swarm_capacity",
    "swarm_peer_capacity",
    "memory_budget",
    "busy",
];
impl ServiceCounters {
    fn observe(&self, started: Instant) {
        let micros = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        self.micros_sum.fetch_add(micros, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.buckets[bucket(micros)].fetch_add(1, Ordering::Relaxed);
    }
}
impl Counters {
    fn diagnostics(&self) -> serde_json::Value {
        let get = |v: &AtomicU64| v.load(Ordering::Relaxed);
        let failures = &self.failures;
        let errors: serde_json::Map<String, serde_json::Value> = ENGINE_FAILURE_KEYS
            .iter()
            .zip(&failures.engine)
            .map(|(key, value)| ((*key).to_owned(), json!(get(value))))
            .collect();
        json!({
            "http_invalid_queries": get(&failures.http_invalid_queries),
            "udp_malformed_packets": get(&failures.udp_malformed_packets),
            "udp_cookie_window_rejections": get(&failures.udp_cookie_window),
            "udp_cookie_auth_rejections": get(&failures.udp_cookie_auth),
            "engine_errors": errors,
        })
    }
}

// A fixed-memory, four-row approximate counter avoids retaining source IPs or
// refusing all new addresses when an exact-key map fills. Secret row hashes
// make intentional collisions difficult; taking the minimum limits collision
// overestimation. This is a short burst guard, not a client identity database.
struct IpLimiter {
    slots: Vec<AtomicU64>,
    seeds: [u64; 4],
    rate: u64,
    started: Instant,
}
impl IpLimiter {
    fn new(rate: u64) -> Self {
        Self {
            slots: (0..4 * 65536).map(|_| AtomicU64::new(0)).collect(),
            seeds: rand::random(),
            rate: rate.clamp(1, u32::MAX as u64 / 2),
            started: Instant::now(),
        }
    }
    fn allow(&self, ip: IpAddr) -> bool {
        let key = match ip {
            IpAddr::V4(v) => u32::from(v) as u64,
            IpAddr::V6(v) => {
                let o = v.octets();
                u64::from_be_bytes(o[..8].try_into().unwrap()).rotate_left(29)
                    ^ u64::from_be_bytes(o[8..].try_into().unwrap())
            }
        };
        let epoch = (self.started.elapsed().as_secs() as u32).wrapping_add(1) as u64;
        let mut minimum = u32::MAX;
        for row in 0..4 {
            let mut hash = key ^ self.seeds[row];
            hash = hash.wrapping_mul(0xbf58476d1ce4e5b9);
            hash ^= hash >> 31;
            hash = hash.wrapping_mul(0x94d049bb133111eb);
            hash ^= hash >> 27;
            let slot = &self.slots[row * 65536 + (hash as usize & 65535)];
            let old = slot
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                    let count = if old >> 32 == epoch { old as u32 } else { 0 };
                    Some((epoch << 32) | u64::from(count.saturating_add(1)))
                })
                .unwrap();
            let count = if old >> 32 == epoch { old as u32 } else { 0 };
            minimum = minimum.min(count);
        }
        u64::from(minimum) < self.rate * 2
    }
}

struct App {
    engine: SwarmEngine,
    instance_id: String,
    counters: Counters,
    limiter: IpLimiter,
    settings: Settings,
    cookies: CookieValidator,
    started: Instant,
    http_slots: Arc<Semaphore>,
    maintenance_at: AtomicU64,
    pressure_until: AtomicU64,
}
impl App {
    fn engine_failure(&self, error: &EngineError) {
        self.counters
            .failures
            .business
            .fetch_add(1, Ordering::Relaxed);
        let index = match error {
            EngineError::InvalidAddress => 0,
            EngineError::InvalidPort => 1,
            EngineError::PeerCapacity => 2,
            EngineError::SwarmCapacity => 3,
            EngineError::SwarmPeerCapacity => 4,
            EngineError::MemoryBudget => 5,
            EngineError::Busy => 6,
        };
        self.counters.failures.engine[index].fetch_add(1, Ordering::Relaxed);
        self.capacity_refusal(error);
    }
    fn invalid_http_query(&self) {
        self.counters
            .failures
            .invalid_packets
            .fetch_add(1, Ordering::Relaxed);
        self.counters
            .failures
            .http_invalid_queries
            .fetch_add(1, Ordering::Relaxed);
    }
    fn rejected_cookie(&self, time: u64, connection_id: u64) {
        self.counters
            .failures
            .invalid_packets
            .fetch_add(1, Ordering::Relaxed);
        // The public hint only identifies which validation stage rejected it.
        // An old hint does not prove the cookie was ever authentic or issued here.
        let age = ((time / 60) as u16).wrapping_sub((connection_id >> 48) as u16);
        let counter = if age > 2 {
            &self.counters.failures.udp_cookie_window
        } else {
            &self.counters.failures.udp_cookie_auth
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
    fn accepted_announce(&self, request: &AnnounceRequest) {
        self.counters
            .accepts
            .accepted
            .fetch_add(1, Ordering::Relaxed);
        let counter = if request.ip.is_ipv4() {
            &self.counters.accepts.ipv4
        } else {
            &self.counters.accepts.ipv6
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
    fn capacity_refusal(&self, error: &EngineError) {
        if matches!(
            error,
            EngineError::MemoryBudget
                | EngineError::PeerCapacity
                | EngineError::SwarmCapacity
                | EngineError::Busy
        ) {
            self.pressure_until
                .fetch_max(now().saturating_add(60), Ordering::Relaxed);
        }
    }
}
fn pressure_reason(
    estimated: usize,
    budget: usize,
    available: usize,
    total: usize,
    recent_refusal: bool,
) -> &'static str {
    if recent_refusal || (estimated as u128) * 10 >= (budget as u128) * 9 {
        "memory_admission"
    } else if (available as u128) * 100 < total as u128 {
        "http_concurrency"
    } else {
        "none"
    }
}
fn now() -> u64 {
    tracker_model::unix_now().max(0) as u64
}
fn no_store(body: Vec<u8>, status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .header(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate")
        .body(Body::from(body))
        .unwrap()
}
fn failure_with(message: &str, status: StatusCode) -> Response {
    let mut body = Vec::with_capacity(message.len() + 40);
    encode_http_failure_into(&mut body, message);
    no_store(body, status)
}
fn failure(message: &str) -> Response {
    failure_with(message, StatusCode::OK)
}
/// Static reply text for every engine rejection.
///
/// Each string is byte-identical to `EngineError`'s `Display` output, which
/// `engine_failure_text_is_static_and_matches_display` asserts for every
/// variant. `None` is unreachable today and exists so that a variant added
/// later degrades to the formatting path instead of breaking the build.
fn failure_static(error: &EngineError) -> Option<&'static str> {
    Some(match error {
        EngineError::InvalidAddress => "invalid or non-public address",
        EngineError::InvalidPort => "invalid port",
        EngineError::PeerCapacity => "peer capacity reached",
        EngineError::SwarmCapacity => "swarm capacity reached",
        EngineError::SwarmPeerCapacity => "swarm peer capacity reached",
        EngineError::MemoryBudget => "memory budget reached",
        EngineError::Busy => "expiry backlog; please retry",
    })
}
/// Reply text for an engine rejection without allocating on the hot path.
fn engine_failure_reason(error: &EngineError) -> Cow<'static, str> {
    match failure_static(error) {
        Some(reason) => Cow::Borrowed(reason),
        None => Cow::Owned(error.to_string()),
    }
}
fn source_ip(source: SocketAddr, headers: &HeaderMap) -> IpAddr {
    // The HTTP listener is private; Nginx overwrites X-Real-IP. Arbitrary
    // forwarded headers from non-loopback clients are never trusted.
    if source.ip().is_loopback()
        && let Some(ip) = headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<IpAddr>().ok())
    {
        return match ip {
            IpAddr::V6(ip) => ip
                .to_ipv4_mapped()
                .map(IpAddr::V4)
                .unwrap_or(IpAddr::V6(ip)),
            ip => ip,
        };
    }
    match source.ip() {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        ip => ip,
    }
}
fn rate_response(app: &App) -> Response {
    app.counters
        .requests
        .rate_limited
        .fetch_add(1, Ordering::Relaxed);
    let mut response = failure_with("request budget exceeded", StatusCode::TOO_MANY_REQUESTS);
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, "60".parse().unwrap());
    response
}
async fn announce(
    State(app): State<Arc<App>>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    method: Method,
    OriginalUri(uri): OriginalUri,
) -> Response {
    if method == Method::HEAD {
        return no_store(Vec::new(), StatusCode::OK);
    }
    let _timer = ServiceTimer::new(&app.counters.service);
    app.counters
        .requests
        .http_announces
        .fetch_add(1, Ordering::Relaxed);
    let Ok(_slot) = app.http_slots.clone().try_acquire_owned() else {
        app.counters
            .requests
            .http_busy
            .fetch_add(1, Ordering::Relaxed);
        return failure_with("service busy", StatusCode::SERVICE_UNAVAILABLE);
    };
    let ip = source_ip(source, &headers);
    if !app.limiter.allow(ip) {
        return rate_response(&app);
    }
    let raw = uri.query().unwrap_or("").as_bytes();
    if raw.len() > 8192 {
        app.invalid_http_query();
        return failure("query too large");
    }
    match parse_http_announce(raw, ip) {
        Ok(request) => match app.engine.announce(&request, now()) {
            Ok(reply) => {
                app.accepted_announce(&request);
                let mut body = Vec::with_capacity(256);
                encode_http_announce_into(&mut body, &reply);
                no_store(body, StatusCode::OK)
            }
            Err(error) => {
                app.engine_failure(&error);
                failure(engine_failure_reason(&error).as_ref())
            }
        },
        Err(_) => {
            app.invalid_http_query();
            failure("invalid announce request")
        }
    }
}
async fn scrape(
    State(app): State<Arc<App>>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    method: Method,
    OriginalUri(uri): OriginalUri,
) -> Response {
    if method == Method::HEAD {
        return no_store(Vec::new(), StatusCode::OK);
    }
    let _timer = ServiceTimer::new(&app.counters.service);
    let Ok(_slot) = app.http_slots.clone().try_acquire_owned() else {
        app.counters
            .requests
            .http_busy
            .fetch_add(1, Ordering::Relaxed);
        return failure_with("service busy", StatusCode::SERVICE_UNAVAILABLE);
    };
    if !app.limiter.allow(source_ip(source, &headers)) {
        return rate_response(&app);
    }
    let raw = uri.query().unwrap_or("").as_bytes();
    if raw.len() > 8192 {
        app.invalid_http_query();
        return failure("query too large");
    }
    match parse_http_scrape(raw) {
        Ok(hashes) => {
            app.counters
                .requests
                .scrapes
                .fetch_add(1, Ordering::Relaxed);
            match app.engine.scrape(&hashes, now()) {
                Ok(entries) => {
                    let mut body = Vec::with_capacity(32 + entries.len() * 96);
                    encode_http_scrape_into(&mut body, &entries);
                    no_store(body, StatusCode::OK)
                }
                Err(error) => {
                    app.engine_failure(&error);
                    failure(engine_failure_reason(&error).as_ref())
                }
            }
        }
        Err(_) => {
            app.invalid_http_query();
            failure("invalid scrape request")
        }
    }
}
fn rss_bytes() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find(|l| l.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()
        .map(|kb| kb * 1024)
}
fn json_no_store(value: serde_json::Value) -> Response {
    let mut response = no_store(serde_json::to_vec(&value).unwrap(), StatusCode::OK);
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    response
}
async fn stats(State(app): State<Arc<App>>) -> Response {
    let c = &app.counters;
    let get = |v: &AtomicU64| v.load(Ordering::Relaxed);
    let engine = app.engine.stats();
    let reason = pressure_reason(
        engine.estimated_bytes,
        app.settings.memory_budget_mib * 1024 * 1024,
        app.http_slots.available_permits(),
        app.settings.http_concurrency,
        now() < get(&app.pressure_until),
    );
    json_no_store(json!({
        "version": tracker_model::VERSION,
        "instance_id": app.instance_id,
        "uptime_seconds": app.started.elapsed().as_secs(),
        "volatile": true,
        "engine": engine,
        "rss_bytes": rss_bytes(),
        "pressure": {"active": reason != "none", "reason": reason},
        "requests": {
            "http_announces": get(&c.requests.http_announces),
            "udp_connects": get(&c.udp.connects),
            "udp_announces": get(&c.udp.announces),
            "scrapes": get(&c.requests.scrapes),
            "accepted_announces": get(&c.accepts.accepted),
            "accepted_ipv4": get(&c.accepts.ipv4),
            "accepted_ipv6": get(&c.accepts.ipv6),
            "business_failures": get(&c.failures.business),
            "invalid_packets": get(&c.failures.invalid_packets),
            "rate_limited": get(&c.requests.rate_limited),
            "http_busy": get(&c.requests.http_busy),
            "udp_send_errors": get(&c.udp.send_errors),
            "udp_v4_packets": get(&c.udp.v4_packets),
            "udp_v6_packets": get(&c.udp.v6_packets),
            "udp_bytes_received": get(&c.udp.rx_bytes),
            "udp_bytes_sent": get(&c.udp.tx_bytes)
        },
        "diagnostics": c.diagnostics(),
        "service_time": {
            "scope": "HTTP announce/scrape and received UDP packets; excludes socket send and transport",
            "count": get(&c.service.count),
            "sum_micros": get(&c.service.micros_sum),
            "bucket_upper_micros": BUCKET_US,
            "bucket_counts": c.service.buckets.iter().map(get).collect::<Vec<_>>()
        },
        "maintenance": {
            "last_at": get(&app.maintenance_at),
            "count": get(&c.maintenance.count),
            "sum_micros": get(&c.maintenance.micros_sum),
            "max_micros": get(&c.maintenance.micros_max)
        }
    }))
}
async fn health() -> Response {
    json_no_store(json!({"status":"ok","version":tracker_model::VERSION}))
}
async fn ready(State(app): State<Arc<App>>) -> Response {
    let healthy = now().saturating_sub(app.maintenance_at.load(Ordering::Relaxed)) < 15
        && app.http_slots.available_permits() > 0;
    let status = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    no_store(
        serde_json::to_vec(&json!({"ready":healthy,"version":tracker_model::VERSION})).unwrap(),
        status,
    )
}
async fn metrics(State(app): State<Arc<App>>) -> String {
    let mut s = format!(
        "tracker_uptime_seconds {}\ntracker_accepted_announces_total {}\ntracker_business_failures_total {}\ntracker_udp_connects_total {}\ntracker_rate_limited_total {}\n",
        app.started.elapsed().as_secs(),
        app.counters.accepts.accepted.load(Ordering::Relaxed),
        app.counters.failures.business.load(Ordering::Relaxed),
        app.counters.udp.connects.load(Ordering::Relaxed),
        app.counters.requests.rate_limited.load(Ordering::Relaxed)
    );
    if let Some(rss) = rss_bytes() {
        s.push_str(&format!("tracker_rss_bytes {rss}\n"));
    }
    let diagnostics = app.counters.diagnostics();
    if let Some(values) = diagnostics.as_object() {
        for (key, value) in values {
            if value.is_u64() {
                s.push_str(&format!("tracker_{key}_total {value}\n"));
            }
        }
    }
    for (key, value) in ENGINE_FAILURE_KEYS
        .iter()
        .zip(&app.counters.failures.engine)
    {
        s.push_str(&format!(
            "tracker_engine_failures_total{{reason=\"{key}\"}} {}\n",
            value.load(Ordering::Relaxed)
        ));
    }
    if let Ok(serde_json::Value::Object(values)) = serde_json::to_value(app.engine.stats()) {
        for (key, value) in values {
            if value.is_number() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                s.push_str(&format!("tracker_engine_{key} {value}\n"));
            }
        }
    }
    s
}
fn bind_udp(addr: SocketAddr) -> Result<UdpSocket> {
    let socket = socket2::Socket::new(
        if addr.is_ipv4() {
            socket2::Domain::IPV4
        } else {
            socket2::Domain::IPV6
        },
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.set_recv_buffer_size(4 * 1024 * 1024)?;
    socket.set_send_buffer_size(4 * 1024 * 1024)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    let socket: std::net::UdpSocket = socket.into();
    Ok(UdpSocket::from_std(socket)?)
}
async fn udp_loop(socket: UdpSocket, app: Arc<App>) -> Result<()> {
    let mut buffer = [0u8; 2048];
    let mut reply = Vec::with_capacity(1200);
    loop {
        let (length, source) = socket.recv_from(&mut buffer).await?;
        let timer = ServiceTimer::new(&app.counters.service);
        let packets = if source.is_ipv6() {
            &app.counters.udp.v6_packets
        } else {
            &app.counters.udp.v4_packets
        };
        packets.fetch_add(1, Ordering::Relaxed);
        app.counters
            .udp
            .rx_bytes
            .fetch_add(length as u64, Ordering::Relaxed);
        if !app.limiter.allow(source.ip()) {
            app.counters
                .requests
                .rate_limited
                .fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let time = now();
        match parse_udp(&buffer[..length], source) {
            Ok(UdpRequest::Connect { transaction_id }) => {
                app.counters.udp.connects.fetch_add(1, Ordering::Relaxed);
                serialize_udp_connect_into(
                    &mut reply,
                    transaction_id,
                    app.cookies.cookie(source.ip(), time),
                );
            }
            Ok(UdpRequest::Announce {
                connection_id,
                transaction_id,
                request,
            }) => {
                if !app.cookies.validate(source.ip(), time, connection_id) {
                    app.rejected_cookie(time, connection_id);
                    continue;
                } else {
                    app.counters.udp.announces.fetch_add(1, Ordering::Relaxed);
                    match app.engine.announce(&request, time) {
                        Ok(response) => {
                            app.accepted_announce(&request);
                            serialize_udp_announce_into(
                                &mut reply,
                                transaction_id,
                                &response,
                                source.is_ipv6(),
                            );
                        }
                        Err(error) => {
                            app.engine_failure(&error);
                            serialize_udp_error_into(
                                &mut reply,
                                transaction_id,
                                engine_failure_reason(&error).as_ref(),
                            );
                        }
                    }
                }
            }
            Ok(UdpRequest::Scrape {
                connection_id,
                transaction_id,
                hashes,
            }) => {
                if !app.cookies.validate(source.ip(), time, connection_id) {
                    app.rejected_cookie(time, connection_id);
                    continue;
                } else {
                    app.counters
                        .requests
                        .scrapes
                        .fetch_add(1, Ordering::Relaxed);
                    match app.engine.scrape(&hashes, time) {
                        Ok(entries) => {
                            serialize_udp_scrape_into(&mut reply, transaction_id, &entries)
                        }
                        Err(error) => {
                            app.engine_failure(&error);
                            serialize_udp_error_into(
                                &mut reply,
                                transaction_id,
                                engine_failure_reason(&error).as_ref(),
                            );
                        }
                    }
                }
            }
            Err(_) => {
                app.counters
                    .failures
                    .invalid_packets
                    .fetch_add(1, Ordering::Relaxed);
                app.counters
                    .failures
                    .udp_malformed_packets
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
        }
        drop(timer);
        if reply.len() <= 1200 {
            match socket.send_to(&reply, source).await {
                Ok(sent) => {
                    app.counters
                        .udp
                        .tx_bytes
                        .fetch_add(sent as u64, Ordering::Relaxed);
                }
                Err(_) => {
                    app.counters.udp.send_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let mut settings = Settings::default();
    if args.config.exists() {
        let root: toml::Value = toml::from_str(&std::fs::read_to_string(&args.config)?)?;
        if let Some(value) = root.get("tracker") {
            settings = value.clone().try_into()?;
        }
    }
    if let Some(value) = args.http {
        settings.http = value;
    }
    if let Some(value) = args.udp_v4 {
        settings.udp_v4 = value;
    }
    if let Some(value) = args.udp_v6 {
        settings.udp_v6 = value;
    }
    anyhow::ensure!(
        settings.memory_budget_mib > 0
            && settings.max_peers > 0
            && settings.max_swarms > 0
            && settings.http_concurrency > 0,
        "invalid capacity configuration"
    );
    let config = EngineConfig {
        max_peers: settings.max_peers,
        max_swarms: settings.max_swarms,
        max_peers_per_swarm: settings.max_peers_per_swarm,
        memory_budget_bytes: settings.memory_budget_mib * 1024 * 1024,
        allow_private_ips: args.allow_private_for_tests,
        ..EngineConfig::default()
    };
    let app = Arc::new(App {
        engine: SwarmEngine::new(config),
        instance_id: format!("{:016x}", rand::random::<u64>()),
        counters: Counters::default(),
        limiter: IpLimiter::new(settings.per_ip_rate),
        cookies: CookieValidator::new(&rand::random::<[u8; 32]>()),
        started: Instant::now(),
        http_slots: Arc::new(Semaphore::new(settings.http_concurrency)),
        settings: settings.clone(),
        maintenance_at: AtomicU64::new(now()),
        pressure_until: AtomicU64::new(0),
    });
    let v4 = bind_udp(settings.udp_v4).context("bind UDP IPv4")?;
    let v6 = bind_udp(settings.udp_v6).context("bind UDP IPv6")?;
    let mut udp4 = tokio::spawn(udp_loop(v4, app.clone()));
    let mut udp6 = tokio::spawn(udp_loop(v6, app.clone()));
    let mut maintenance = {
        let app = app.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            loop {
                tick.tick().await;
                let started = Instant::now();
                app.engine
                    .maintenance(now(), app.settings.maintenance_budget);
                let micros = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
                app.counters
                    .maintenance
                    .count
                    .fetch_add(1, Ordering::Relaxed);
                app.counters
                    .maintenance
                    .micros_sum
                    .fetch_add(micros, Ordering::Relaxed);
                app.counters
                    .maintenance
                    .micros_max
                    .fetch_max(micros, Ordering::Relaxed);
                app.maintenance_at.store(now(), Ordering::Relaxed);
            }
        })
    };
    let router = Router::new()
        .route("/announce", get(announce))
        .route("/scrape", get(scrape))
        .route("/stats.json", get(stats))
        .route("/stats", get(stats))
        .route("/metrics", get(metrics))
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .with_state(app);
    let listener = TcpListener::bind(settings.http)
        .await
        .context("bind HTTP")?;
    tracing::info!(version = tracker_model::VERSION, "tracker listeners ready");
    let serving = async {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown())
        .await
    };
    tokio::pin!(serving);
    let result: Result<()> = tokio::select! {result=&mut serving=>result.map_err(Into::into),_=&mut udp4=>Err(anyhow::anyhow!("IPv4 UDP task stopped")),_=&mut udp6=>Err(anyhow::anyhow!("IPv6 UDP task stopped")),_=&mut maintenance=>Err(anyhow::anyhow!("maintenance task stopped"))};
    udp4.abort();
    udp6.abort();
    maintenance.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_app(allow_private_ips: bool) -> Arc<App> {
        Arc::new(App {
            engine: SwarmEngine::new(EngineConfig {
                allow_private_ips,
                ..EngineConfig::default()
            }),
            instance_id: "test".to_owned(),
            counters: Counters::default(),
            limiter: IpLimiter::new(1000),
            settings: Settings::default(),
            cookies: CookieValidator::new(&[7; 32]),
            started: Instant::now(),
            http_slots: Arc::new(Semaphore::new(16)),
            maintenance_at: AtomicU64::new(now()),
            pressure_until: AtomicU64::new(0),
        })
    }
    #[tokio::test]
    async fn invalid_udp_ids_stay_silent_and_have_separate_counters() {
        let app = test_app(true);
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let task = tokio::spawn(udp_loop(server, app.clone()));
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(address).await.unwrap();
        let mut response = [0u8; 1200];
        client.send(&encode_udp_connect_request(123)).await.unwrap();
        let n = tokio::time::timeout(Duration::from_secs(1), client.recv(&mut response))
            .await
            .unwrap()
            .unwrap();
        let cookie = decode_udp_connect_response(&response[..n], 123).unwrap();
        let request = AnnounceRequest {
            info_hash: [3; 20],
            peer_id: [5; 20],
            ip: "127.0.0.1".parse().unwrap(),
            port: 6881,
            uploaded: 0,
            downloaded: 0,
            left: 1,
            event: AnnounceEvent::Started,
            numwant: 0,
        };
        client
            .send(&encode_udp_announce_request(cookie, 127, &request))
            .await
            .unwrap();
        let n = tokio::time::timeout(Duration::from_secs(1), client.recv(&mut response))
            .await
            .unwrap()
            .unwrap();
        let announced = decode_udp_announce_response(&response[..n], 127, false).unwrap();
        assert_eq!(announced.incomplete, 1);
        let mut scrape = Vec::with_capacity(36);
        scrape.extend_from_slice(&cookie.to_be_bytes());
        scrape.extend_from_slice(&2u32.to_be_bytes());
        scrape.extend_from_slice(&128u32.to_be_bytes());
        scrape.extend_from_slice(&request.info_hash);
        client.send(&scrape).await.unwrap();
        let n = tokio::time::timeout(Duration::from_secs(1), client.recv(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n, 20);
        assert_eq!(&response[..8], &[0, 0, 0, 2, 0, 0, 0, 128]);
        assert_eq!(u32::from_be_bytes(response[16..20].try_into().unwrap()), 1);
        let initial = app.engine.stats();
        let bad_tag = encode_udp_announce_request(cookie ^ 1, 124, &request);
        let old = app.cookies.cookie(request.ip, now().saturating_sub(240));
        let old_hint = encode_udp_announce_request(old, 125, &request);
        for packet in [bad_tag.as_slice(), old_hint.as_slice(), &[0u8; 7]] {
            client.send(packet).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(50), client.recv(&mut response))
                    .await
                    .is_err()
            );
        }
        let d = app.counters.diagnostics();
        assert_eq!(d["udp_cookie_auth_rejections"], 1);
        assert_eq!(d["udp_cookie_window_rejections"], 1);
        assert_eq!(d["udp_malformed_packets"], 1);
        assert_eq!(
            app.counters
                .failures
                .invalid_packets
                .load(Ordering::Relaxed),
            3
        );
        let unchanged = app.engine.stats();
        assert_eq!(unchanged.peers, initial.peers);
        assert_eq!(unchanged.announces, initial.announces);
        assert_eq!(unchanged.scrapes, initial.scrapes);
        client.send(&encode_udp_connect_request(126)).await.unwrap();
        let n = tokio::time::timeout(Duration::from_secs(1), client.recv(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(decode_udp_connect_response(&response[..n], 126).is_ok());
        let mut stopped = request.clone();
        stopped.event = AnnounceEvent::Stopped;
        client
            .send(&encode_udp_announce_request(cookie, 129, &stopped))
            .await
            .unwrap();
        let n = tokio::time::timeout(Duration::from_secs(1), client.recv(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            decode_udp_announce_response(&response[..n], 129, false)
                .unwrap()
                .incomplete,
            0
        );
        assert_eq!(app.engine.stats().peers, 0);
        task.abort();
        let _ = task.await;
    }
    #[tokio::test]
    async fn diagnostics_distinguish_protocol_replies_from_business_failures() {
        let app = test_app(false);
        let request = AnnounceRequest {
            info_hash: [3; 20],
            peer_id: [5; 20],
            ip: "127.0.0.1".parse().unwrap(),
            port: 6881,
            uploaded: 0,
            downloaded: 0,
            left: 1,
            event: AnnounceEvent::Stopped,
            numwant: 0,
        };
        let uri = format!("/announce?{}", encode_http_announce_query(&request))
            .parse()
            .unwrap();
        let reply = announce(
            State(app.clone()),
            ConnectInfo("127.0.0.1:6881".parse().unwrap()),
            HeaderMap::new(),
            Method::GET,
            OriginalUri(uri),
        )
        .await;
        assert_eq!(reply.status(), StatusCode::OK);
        let body = axum::body::to_bytes(reply.into_body(), 1024).await.unwrap();
        assert!(
            body.windows(b"failure reason".len())
                .any(|s| s == b"failure reason")
        );
        // The reply text is the static mapping, byte for byte.
        assert!(
            body.windows(b"invalid or non-public address".len())
                .any(|s| s == b"invalid or non-public address")
        );
        assert_eq!(app.counters.failures.business.load(Ordering::Relaxed), 1);
        assert_eq!(
            app.counters.diagnostics()["engine_errors"]["invalid_address"],
            1
        );
        let reply = announce(
            State(app.clone()),
            ConnectInfo("127.0.0.1:6881".parse().unwrap()),
            HeaderMap::new(),
            Method::GET,
            OriginalUri("/announce?bad=query".parse().unwrap()),
        )
        .await;
        assert_eq!(reply.status(), StatusCode::OK);
        assert_eq!(app.counters.diagnostics()["http_invalid_queries"], 1);
        assert_eq!(app.counters.failures.business.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn engine_failure_text_is_static_and_matches_display() {
        let variants = [
            EngineError::InvalidAddress,
            EngineError::InvalidPort,
            EngineError::PeerCapacity,
            EngineError::SwarmCapacity,
            EngineError::SwarmPeerCapacity,
            EngineError::MemoryBudget,
            EngineError::Busy,
        ];
        for error in variants {
            let reason = engine_failure_reason(&error);
            let displayed = error.to_string();
            assert!(matches!(reason, Cow::Borrowed(_)), "{error:?} allocated");
            assert_eq!(reason.as_ref(), failure_static(&error).unwrap());
            assert_eq!(reason.as_ref(), displayed.as_str());
            // Both reply paths stay byte-identical to the old allocation path.
            assert_eq!(
                encode_http_failure(reason.as_ref()),
                encode_http_failure(&displayed)
            );
            assert_eq!(
                encode_udp_error(9, reason.as_ref()),
                encode_udp_error(9, &displayed)
            );
        }
    }
    #[test]
    fn service_time_bucket_matches_the_linear_scan_it_replaced() {
        // The exact expression used before the lookup became a partition point.
        let reference = |micros: u64| {
            BUCKET_US
                .iter()
                .position(|bound| micros <= *bound)
                .unwrap_or(BUCKET_US.len() - 1)
        };
        let mut samples = vec![
            0u64, 1, 99, 100, 101, 499, 500, 501, 999, 1000, 1001, 4999, 5000, 5001, 9999, 10_000,
            10_001, 20_000, 20_001, 50_000, 50_001, 99_999, 100_000, 100_001,
        ];
        samples.extend([u64::MAX - 1, u64::MAX]);
        for bound in BUCKET_US {
            if bound != u64::MAX {
                samples.extend([bound - 1, bound, bound + 1]);
            }
        }
        for micros in samples {
            assert_eq!(bucket(micros), reference(micros), "micros {micros}");
        }
        // Below the first bound, exactly on a bound, and past the last bound.
        assert_eq!(bucket(0), 0);
        assert_eq!(bucket(1), 0);
        assert_eq!(bucket(100), 0);
        assert_eq!(bucket(101), 1);
        assert_eq!(bucket(100_000), 8);
        assert_eq!(bucket(100_001), 9);
        assert_eq!(bucket(u64::MAX), 9);
    }
    #[test]
    fn resource_pressure_has_explicit_memory_and_concurrency_boundaries() {
        assert_eq!(pressure_reason(89, 100, 100, 100, false), "none");
        assert_eq!(
            pressure_reason(90, 100, 100, 100, false),
            "memory_admission"
        );
        assert_eq!(pressure_reason(1, 100, 0, 100, false), "http_concurrency");
        assert_eq!(pressure_reason(1, 100, 100, 100, true), "memory_admission");
    }
    #[test]
    fn forwarded_headers_require_loopback() {
        let mut h = HeaderMap::new();
        h.insert("x-real-ip", "1.1.1.1".parse().unwrap());
        assert_eq!(
            source_ip("8.8.8.8:1".parse().unwrap(), &h),
            "8.8.8.8".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            source_ip("127.0.0.1:1".parse().unwrap(), &h),
            "1.1.1.1".parse::<IpAddr>().unwrap()
        );
    }
    #[test]
    fn source_bucket_is_bounded_and_recovers() {
        let limiter = IpLimiter::new(1);
        let ip = "1.1.1.1".parse().unwrap();
        assert!(limiter.allow(ip));
        assert!(limiter.allow(ip));
        assert!(!limiter.allow(ip));
    }
}
