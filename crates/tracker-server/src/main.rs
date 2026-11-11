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

#[derive(Default)]
struct Counters {
    http_announces: AtomicU64,
    udp_connects: AtomicU64,
    udp_announces: AtomicU64,
    scrapes: AtomicU64,
    accepted: AtomicU64,
    accepted_ipv4: AtomicU64,
    accepted_ipv6: AtomicU64,
    failures: AtomicU64,
    invalid_packets: AtomicU64,
    rate_limited: AtomicU64,
    http_busy: AtomicU64,
    udp_rx: AtomicU64,
    udp_v4_packets: AtomicU64,
    udp_v6_packets: AtomicU64,
    udp_tx: AtomicU64,
    udp_send_errors: AtomicU64,
    service_micros_sum: AtomicU64,
    service_count: AtomicU64,
    service_buckets: [AtomicU64; 10],
    maintenance_count: AtomicU64,
    maintenance_micros_sum: AtomicU64,
    maintenance_micros_max: AtomicU64,
}
struct ServiceTimer<'a> {
    counters: &'a Counters,
    started: Instant,
}
impl<'a> ServiceTimer<'a> {
    fn new(counters: &'a Counters) -> Self {
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
impl Counters {
    fn observe(&self, started: Instant) {
        let micros = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        self.service_micros_sum.fetch_add(micros, Ordering::Relaxed);
        self.service_count.fetch_add(1, Ordering::Relaxed);
        let index = BUCKET_US
            .iter()
            .position(|bound| micros <= *bound)
            .unwrap_or(9);
        self.service_buckets[index].fetch_add(1, Ordering::Relaxed);
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
    secret: [u8; 32],
    started: Instant,
    http_slots: Arc<Semaphore>,
    maintenance_at: AtomicU64,
    pressure_until: AtomicU64,
}
impl App {
    fn accepted_announce(&self, request: &AnnounceRequest) {
        self.counters.accepted.fetch_add(1, Ordering::Relaxed);
        let counter = if request.ip.is_ipv4() {
            &self.counters.accepted_ipv4
        } else {
            &self.counters.accepted_ipv6
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
fn failure(message: &str) -> Response {
    no_store(encode_http_failure(message), StatusCode::OK)
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
    app.counters.rate_limited.fetch_add(1, Ordering::Relaxed);
    let mut response = no_store(
        encode_http_failure("request budget exceeded"),
        StatusCode::TOO_MANY_REQUESTS,
    );
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
    let _timer = ServiceTimer::new(&app.counters);
    app.counters.http_announces.fetch_add(1, Ordering::Relaxed);
    let Ok(_slot) = app.http_slots.clone().try_acquire_owned() else {
        app.counters.http_busy.fetch_add(1, Ordering::Relaxed);
        return no_store(
            encode_http_failure("service busy"),
            StatusCode::SERVICE_UNAVAILABLE,
        );
    };
    let ip = source_ip(source, &headers);
    if !app.limiter.allow(ip) {
        return rate_response(&app);
    }
    let raw = uri.query().unwrap_or("").as_bytes();
    if raw.len() > 8192 {
        app.counters.invalid_packets.fetch_add(1, Ordering::Relaxed);
        return failure("query too large");
    }
    match parse_http_announce(raw, ip) {
        Ok(request) => match app.engine.announce(&request, now()) {
            Ok(reply) => {
                app.accepted_announce(&request);
                no_store(encode_http_announce(&reply), StatusCode::OK)
            }
            Err(error) => {
                app.counters.failures.fetch_add(1, Ordering::Relaxed);
                app.capacity_refusal(&error);
                failure(&error.to_string())
            }
        },
        Err(_) => {
            app.counters.invalid_packets.fetch_add(1, Ordering::Relaxed);
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
    let _timer = ServiceTimer::new(&app.counters);
    let Ok(_slot) = app.http_slots.clone().try_acquire_owned() else {
        app.counters.http_busy.fetch_add(1, Ordering::Relaxed);
        return no_store(
            encode_http_failure("service busy"),
            StatusCode::SERVICE_UNAVAILABLE,
        );
    };
    if !app.limiter.allow(source_ip(source, &headers)) {
        return rate_response(&app);
    }
    let raw = uri.query().unwrap_or("").as_bytes();
    if raw.len() > 8192 {
        app.counters.invalid_packets.fetch_add(1, Ordering::Relaxed);
        return failure("query too large");
    }
    match parse_http_scrape(raw) {
        Ok(hashes) => {
            app.counters.scrapes.fetch_add(1, Ordering::Relaxed);
            match app.engine.scrape(&hashes, now()) {
                Ok(entries) => no_store(encode_http_scrape(&entries), StatusCode::OK),
                Err(error) => {
                    app.counters.failures.fetch_add(1, Ordering::Relaxed);
                    app.capacity_refusal(&error);
                    failure(&error.to_string())
                }
            }
        }
        Err(_) => {
            app.counters.invalid_packets.fetch_add(1, Ordering::Relaxed);
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
    json_no_store(
        json!({"version":tracker_model::VERSION,"instance_id":app.instance_id,"uptime_seconds":app.started.elapsed().as_secs(),"volatile":true,"engine":engine,"rss_bytes":rss_bytes(),"pressure":{"active":reason!="none","reason":reason},"requests":{"http_announces":get(&c.http_announces),"udp_connects":get(&c.udp_connects),"udp_announces":get(&c.udp_announces),"scrapes":get(&c.scrapes),"accepted_announces":get(&c.accepted),"accepted_ipv4":get(&c.accepted_ipv4),"accepted_ipv6":get(&c.accepted_ipv6),"business_failures":get(&c.failures),"invalid_packets":get(&c.invalid_packets),"rate_limited":get(&c.rate_limited),"http_busy":get(&c.http_busy),"udp_send_errors":get(&c.udp_send_errors),"udp_v4_packets":get(&c.udp_v4_packets),"udp_v6_packets":get(&c.udp_v6_packets),"udp_bytes_received":get(&c.udp_rx),"udp_bytes_sent":get(&c.udp_tx)},"service_time":{"scope":"HTTP announce/scrape and received UDP packets; excludes socket send and transport","count":get(&c.service_count),"sum_micros":get(&c.service_micros_sum),"bucket_upper_micros":BUCKET_US,"bucket_counts":c.service_buckets.iter().map(get).collect::<Vec<_>>()},"maintenance":{"last_at":get(&app.maintenance_at),"count":get(&c.maintenance_count),"sum_micros":get(&c.maintenance_micros_sum),"max_micros":get(&c.maintenance_micros_max)}}),
    )
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
        app.counters.accepted.load(Ordering::Relaxed),
        app.counters.failures.load(Ordering::Relaxed),
        app.counters.udp_connects.load(Ordering::Relaxed),
        app.counters.rate_limited.load(Ordering::Relaxed)
    );
    if let Some(rss) = rss_bytes() {
        s.push_str(&format!("tracker_rss_bytes {rss}\n"));
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
    loop {
        let (length, source) = socket.recv_from(&mut buffer).await?;
        let timer = ServiceTimer::new(&app.counters);
        let packets = if source.is_ipv6() {
            &app.counters.udp_v6_packets
        } else {
            &app.counters.udp_v4_packets
        };
        packets.fetch_add(1, Ordering::Relaxed);
        app.counters
            .udp_rx
            .fetch_add(length as u64, Ordering::Relaxed);
        if !app.limiter.allow(source.ip()) {
            app.counters.rate_limited.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let time = now();
        let reply = match parse_udp(&buffer[..length], source) {
            Ok(UdpRequest::Connect { transaction_id }) => {
                app.counters.udp_connects.fetch_add(1, Ordering::Relaxed);
                encode_udp_connect(transaction_id, udp_cookie(&app.secret, source.ip(), time))
            }
            Ok(UdpRequest::Announce {
                connection_id,
                transaction_id,
                request,
            }) => {
                if !validate_udp_cookie(&app.secret, source.ip(), time, connection_id) {
                    app.counters.invalid_packets.fetch_add(1, Ordering::Relaxed);
                    continue;
                } else {
                    app.counters.udp_announces.fetch_add(1, Ordering::Relaxed);
                    match app.engine.announce(&request, time) {
                        Ok(response) => {
                            app.accepted_announce(&request);
                            encode_udp_announce(transaction_id, &response, source.is_ipv6())
                        }
                        Err(error) => {
                            app.counters.failures.fetch_add(1, Ordering::Relaxed);
                            app.capacity_refusal(&error);
                            encode_udp_error(transaction_id, &error.to_string())
                        }
                    }
                }
            }
            Ok(UdpRequest::Scrape {
                connection_id,
                transaction_id,
                hashes,
            }) => {
                if !validate_udp_cookie(&app.secret, source.ip(), time, connection_id) {
                    app.counters.invalid_packets.fetch_add(1, Ordering::Relaxed);
                    continue;
                } else {
                    app.counters.scrapes.fetch_add(1, Ordering::Relaxed);
                    match app.engine.scrape(&hashes, time) {
                        Ok(entries) => encode_udp_scrape(transaction_id, &entries),
                        Err(error) => {
                            app.counters.failures.fetch_add(1, Ordering::Relaxed);
                            app.capacity_refusal(&error);
                            encode_udp_error(transaction_id, &error.to_string())
                        }
                    }
                }
            }
            Err(_) => {
                app.counters.invalid_packets.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        drop(timer);
        if reply.len() <= 1200 {
            match socket.send_to(&reply, source).await {
                Ok(sent) => {
                    app.counters
                        .udp_tx
                        .fetch_add(sent as u64, Ordering::Relaxed);
                }
                Err(_) => {
                    app.counters.udp_send_errors.fetch_add(1, Ordering::Relaxed);
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
        secret: rand::random(),
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
                    .maintenance_count
                    .fetch_add(1, Ordering::Relaxed);
                app.counters
                    .maintenance_micros_sum
                    .fetch_add(micros, Ordering::Relaxed);
                app.counters
                    .maintenance_micros_max
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
