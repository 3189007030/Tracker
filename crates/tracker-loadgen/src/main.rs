use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, ValueEnum};
use futures_util::TryStreamExt;
use reqwest::{Client, Url, header};
use serde::Serialize;
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::{UdpSocket, lookup_host};
use tokio::time::{Instant, sleep_until, timeout};
use tokio_util::io::StreamReader;
use tracker_loadgen::{
    Announce, ByteBudget, HARD_MAX_BYTES, Histogram, MAX_HTTP_BODY, MAX_TARGET_BYTES,
    MAX_UDP_PACKET, ReplyError, TrackerReply, parse_http_announce, parse_http_scrape,
    parse_udp_announce, parse_udp_connect, parse_udp_scrape, percent, udp_announce_request,
    udp_connect_request, udp_cookie_rejected, udp_scrape_request,
};

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum Mode {
    Pressure,
    Verify,
    StateBench,
}
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum Protocol {
    Http,
    Https,
    Udp,
    Mixed,
}
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum Family {
    Ipv4,
    Ipv6,
}
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum Workload {
    Announce,
    Scrape,
    Connect,
    Mixed,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum StateWorkload {
    Baseline,
    Scatter,
    Churn,
}

#[derive(Debug, Parser, Clone)]
#[command(
    version,
    about = "Bounded BitTorrent tracker test client for endpoints you own"
)]
struct Args {
    /// Acknowledge that every target is owned or explicitly operated by you.
    #[arg(long)]
    i_own_the_target: bool,
    #[arg(long, value_enum, default_value_t = Mode::Pressure)]
    mode: Mode,
    /// Run an isolated in-process logical-time state benchmark; no network traffic.
    #[arg(long)]
    state_bench: bool,
    /// Baseline, scattered-deadline admission, or sequential churn; no network.
    #[arg(long, value_enum, default_value_t = StateWorkload::Baseline)]
    state_workload: StateWorkload,
    #[arg(long, default_value_t = 10_000)]
    state_count: usize,
    #[arg(long, default_value_t = 768)]
    state_memory_mib: usize,
    #[arg(long, value_enum, default_value_t = Protocol::Udp)]
    protocol: Protocol,
    /// Mixed workload is 80% announce / 20% scrape; connect-only is UDP.
    #[arg(long, value_enum, default_value_t = Workload::Announce)]
    operation: Workload,
    /// HTTP or HTTPS announce URL (no credentials or query string).
    #[arg(long, alias = "url")]
    http_url: Option<String>,
    /// UDP host:port, using [IPv6]:port for literal IPv6 addresses.
    #[arg(long)]
    udp_target: Option<String>,
    #[arg(long, value_enum, default_value_t = Family::Ipv4)]
    family: Family,
    /// Per-client address families for verify (cross-family requires HTTP).
    #[arg(long, value_enum)]
    family_a: Option<Family>,
    #[arg(long, value_enum)]
    family_b: Option<Family>,
    /// Connect HTTP(S) to this IP while preserving URL host and TLS SNI.
    #[arg(long)]
    resolve_ip: Option<IpAddr>,
    /// Override the HTTP(S) destination for verify client A.
    #[arg(long)]
    resolve_a_ip: Option<IpAddr>,
    /// Override the HTTP(S) destination for verify client B.
    #[arg(long)]
    resolve_b_ip: Option<IpAddr>,
    /// Accept private/loopback peers only for isolated test servers.
    #[arg(long)]
    allow_private_test: bool,
    /// Optional locally assigned address to bind the load generator's sockets.
    #[arg(long)]
    source_ip: Option<IpAddr>,
    /// Optional locally assigned address for verify client A.
    #[arg(long)]
    source_a_ip: Option<IpAddr>,
    /// Optional locally assigned address for verify client B.
    #[arg(long)]
    source_b_ip: Option<IpAddr>,
    #[arg(long, default_value_t = 10)]
    rate: u32,
    #[arg(long, default_value_t = 30)]
    duration: u64,
    #[arg(long, default_value_t = 8)]
    concurrency: usize,
    #[arg(long, default_value_t = 10_000)]
    unique_hashes: u64,
    #[arg(long, default_value_t = 2)]
    peers_per_hash: u16,
    #[arg(long, default_value_t = 20_000)]
    base_port: u16,
    /// Expected externally visible source IP shared by both verify clients.
    #[arg(long)]
    expected_client_ip: Option<IpAddr>,
    /// Override the expected public source IP for verify client A.
    #[arg(long)]
    expected_peer_a_ip: Option<IpAddr>,
    /// Override the expected public source IP for verify client B.
    #[arg(long)]
    expected_peer_b_ip: Option<IpAddr>,
    #[arg(long, default_value_t = 50)]
    numwant: u16,
    /// Disable idle connection pooling and request a fresh HTTP connection.
    #[arg(long)]
    new_connections: bool,
    #[arg(long, default_value_t = 5_000)]
    timeout_ms: u64,
    /// In verify mode, wait without client refresh for 0..1800 real seconds.
    #[arg(long, default_value_t = 0)]
    verify_delay_secs: u64,
    /// Maximum measured application bytes, capped at 18 GiB. The host NIC must
    /// be monitored separately for headers, encryption, retries and buffering.
    #[arg(long, default_value_t = 67_108_864)]
    max_bytes: u64,
    /// Emit one bounded JSON progress record per second.
    #[arg(long)]
    progress: bool,
}

impl Args {
    fn validate(&self) -> Result<()> {
        if self.state_bench || self.mode == Mode::StateBench {
            ensure!(
                (1..=2_000_000).contains(&self.state_count),
                "state-count must be 1..2000000"
            );
            ensure!(
                (1..=2_000_000).contains(&self.unique_hashes),
                "unique-hashes must be 1..2000000"
            );
            ensure!(
                (64..=1400).contains(&self.state_memory_mib),
                "state-memory-mib must be 64..1400"
            );
            if self.state_workload != StateWorkload::Churn {
                ensure!(
                    self.state_count.div_ceil(self.unique_hashes as usize) <= 10_000,
                    "state benchmark exceeds 10000 peers per swarm"
                );
            }
            return Ok(());
        }
        ensure!(
            self.i_own_the_target,
            "use --i-own-the-target only for your own authorized tracker"
        );
        ensure!(
            (1..=200_000).contains(&self.rate),
            "rate must be 1..200000 requests/s"
        );
        ensure!(
            (1..=86_400).contains(&self.duration),
            "duration must be 1..86400 seconds"
        );
        let max_concurrency = if self.protocol == Protocol::Udp && self.mode == Mode::Pressure {
            4096
        } else {
            1024
        };
        ensure!(
            (1..=max_concurrency).contains(&self.concurrency),
            "concurrency exceeds the bounded protocol window (maximum {max_concurrency})"
        );
        ensure!(
            (1..=2_000_000).contains(&self.unique_hashes),
            "unique-hashes must be 1..2000000"
        );
        ensure!(
            (1..=1000).contains(&self.peers_per_hash),
            "peers-per-hash must be 1..1000"
        );
        ensure!(
            u32::from(self.base_port) + u32::from(self.peers_per_hash.max(2))
                <= u32::from(u16::MAX),
            "announced ports exceed 65535"
        );
        ensure!(self.numwant <= 100, "numwant must be 0..100");
        ensure!(
            self.verify_delay_secs <= 1800,
            "verify-delay-secs must be 0..1800"
        );
        ensure!(
            self.mode == Mode::Verify || self.verify_delay_secs == 0,
            "verify-delay-secs is available only in verify mode"
        );
        let family_a = self.family_a.unwrap_or(self.family);
        let family_b = self.family_b.unwrap_or(self.family);
        let matches_family = |family, ip| {
            matches!(
                (family, ip),
                (Family::Ipv4, IpAddr::V4(_)) | (Family::Ipv6, IpAddr::V6(_))
            )
        };
        let overrides = [self.resolve_ip, self.resolve_a_ip, self.resolve_b_ip];
        if overrides.iter().any(Option::is_some) {
            ensure!(
                self.protocol != Protocol::Udp,
                "HTTP resolution overrides require an HTTP(S) target"
            );
            for ip in overrides.into_iter().flatten() {
                ensure!(
                    !ip.is_unspecified()
                        && !ip.is_multicast()
                        && !matches!(ip, IpAddr::V4(v) if v.is_broadcast()),
                    "resolution override must be a concrete unicast IP"
                );
            }
        }
        if self.mode != Mode::Verify {
            ensure!(
                self.family_a.is_none()
                    && self.family_b.is_none()
                    && self.source_a_ip.is_none()
                    && self.source_b_ip.is_none(),
                "per-client families and sources are only available in verify mode"
            );
            ensure!(
                self.resolve_a_ip.is_none() && self.resolve_b_ip.is_none(),
                "per-client resolution overrides are only available in verify mode"
            );
            if let Some(ip) = self.resolve_ip {
                ensure!(
                    matches_family(self.family, ip),
                    "resolution override must match --family"
                );
            }
            if let Some(ip) = self.source_ip {
                ensure!(
                    matches_family(self.family, ip),
                    "source IP must match --family"
                );
            }
        }
        if self.mode == Mode::Verify {
            ensure!(
                self.protocol != Protocol::Mixed || self.resolve_b_ip.is_none(),
                "client B uses UDP in mixed verify mode; --resolve-b-ip applies only to HTTP"
            );
            let resolve_a = if self.protocol != Protocol::Udp {
                self.resolve_a_ip.or(self.resolve_ip)
            } else {
                None
            };
            let resolve_b = if matches!(self.protocol, Protocol::Http | Protocol::Https) {
                self.resolve_b_ip.or(self.resolve_ip)
            } else {
                None
            };
            if let Some(ip) = resolve_a {
                ensure!(
                    matches_family(family_a, ip),
                    "resolution override A must match family A"
                );
            }
            if let Some(ip) = resolve_b {
                ensure!(
                    matches_family(family_b, ip),
                    "resolution override B must match family B"
                );
            }
            if family_a != family_b {
                ensure!(
                    matches!(self.protocol, Protocol::Http | Protocol::Https),
                    "cross-family verification requires HTTP for both clients; UDP replies contain only the request family"
                );
                ensure!(
                    self.source_ip.is_none(),
                    "cross-family verification requires separate --source-a-ip / --source-b-ip when binding"
                );
            }
            if let Some(ip) = self.source_a_ip.or(self.source_ip) {
                ensure!(
                    matches_family(family_a, ip),
                    "source A IP does not match family A"
                );
            }
            if let Some(ip) = self.source_b_ip.or(self.source_ip) {
                ensure!(
                    matches_family(family_b, ip),
                    "source B IP does not match family B"
                );
            }
            let a = self
                .expected_peer_a_ip
                .or(self.expected_client_ip)
                .context("verify requires --expected-client-ip or --expected-peer-a-ip")?;
            let b = self
                .expected_peer_b_ip
                .or(self.expected_client_ip)
                .context("verify requires --expected-client-ip or --expected-peer-b-ip")?;
            ensure!(
                matches_family(family_a, a) && matches_family(family_b, b),
                "expected peer IPs must match their requested address families"
            );
        }
        ensure!(
            self.operation != Workload::Connect || self.protocol == Protocol::Udp,
            "connect-only is available for UDP"
        );
        ensure!(
            (100..=60_000).contains(&self.timeout_ms),
            "timeout-ms must be 100..60000"
        );
        ensure!(
            (32_768..=HARD_MAX_BYTES).contains(&self.max_bytes),
            "max-bytes must be 32768..19327352832"
        );
        if matches!(
            self.protocol,
            Protocol::Http | Protocol::Https | Protocol::Mixed
        ) {
            ensure!(
                self.http_url.is_some(),
                "--http-url is required for this protocol"
            );
        }
        if matches!(self.protocol, Protocol::Udp | Protocol::Mixed) {
            ensure!(
                self.udp_target.is_some(),
                "--udp-target is required for this protocol"
            );
        }
        if let Some(raw) = &self.http_url {
            ensure!(raw.len() <= MAX_TARGET_BYTES, "HTTP target too long");
            let u = Url::parse(raw).context("invalid HTTP target")?;
            ensure!(
                matches!(u.scheme(), "http" | "https") && u.host_str().is_some(),
                "target must be HTTP(S)"
            );
            ensure!(
                u.username().is_empty()
                    && u.password().is_none()
                    && u.query().is_none()
                    && u.fragment().is_none(),
                "target credentials, query and fragment are not accepted"
            );
            if overrides.iter().any(Option::is_some) {
                ensure!(
                    matches!(u.host(), Some(url::Host::Domain(_))),
                    "resolution override requires a URL hostname, not a literal IP"
                );
            }
            if self.protocol == Protocol::Http {
                ensure!(u.scheme() == "http", "--protocol http requires an http URL");
            }
            if self.protocol == Protocol::Https {
                ensure!(
                    u.scheme() == "https",
                    "--protocol https requires an https URL"
                );
            }
        }
        if let Some(raw) = &self.udp_target {
            ensure!(
                raw.len() <= 256 && !raw.contains('/') && !raw.contains('@'),
                "invalid UDP target"
            );
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Request {
    hash: [u8; 20],
    peer_id: [u8; 20],
    port: u16,
    event: u32,
    numwant: u16,
    key: u32,
}

#[derive(Default)]
struct IoCounts {
    tx: u64,
    rx: u64,
    udp_connects: u64,
    udp_connections_ok: u64,
    udp_announces: u64,
    udp_scrapes: u64,
    udp_stale_packets: u64,
}

#[derive(Debug)]
enum RunError {
    Timeout,
    Transport,
    HttpStatus(u16),
    Protocol(ReplyError),
    TooLarge,
    UnmatchedDatagrams,
}

#[derive(Debug, Clone, Copy)]
enum Operation {
    Announce,
    Scrape,
    Connect,
}

#[derive(Clone)]
struct HttpTransport {
    client: Client,
    base: Url,
    close: bool,
    allow_private: bool,
}
struct UdpTransport {
    socket: UdpSocket,
    target: SocketAddr,
    connection: Option<(u64, Instant)>,
    v6: bool,
    allow_private: bool,
    next_connect_at: Option<Instant>,
    connect_failures: u32,
    last_timeout_refresh: Option<Instant>,
}
enum Transport {
    Http(HttpTransport),
    Udp(UdpTransport),
}

async fn resolve_target(host: &str, port: u16, family: Family) -> Result<SocketAddr> {
    if let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        ensure!(
            matches!(
                (family, ip),
                (Family::Ipv4, IpAddr::V4(_)) | (Family::Ipv6, IpAddr::V6(_))
            ),
            "literal target does not match requested family"
        );
        return Ok(SocketAddr::new(ip, port));
    }
    lookup_host((host, port))
        .await
        .context("target DNS resolution failed")?
        .find(|addr| {
            matches!(
                (family, addr),
                (Family::Ipv4, SocketAddr::V4(_)) | (Family::Ipv6, SocketAddr::V6(_))
            )
        })
        .context("target has no address in requested family")
}

async fn make_http(
    raw: &str,
    family: Family,
    close: bool,
    timeout_ms: u64,
    source: Option<IpAddr>,
    allow_private: bool,
    resolve_ip: Option<IpAddr>,
) -> Result<Transport> {
    let base = Url::parse(raw)?;
    ensure!(
        matches!(base.scheme(), "http" | "https")
            && base.username().is_empty()
            && base.password().is_none()
            && base.query().is_none()
            && base.fragment().is_none(),
        "HTTP target must have a valid scheme and no credentials, query or fragment"
    );
    let host = base.host_str().context("missing host")?;
    let port = base.port_or_known_default().context("missing port")?;
    let selected = if let Some(ip) = resolve_ip {
        ensure!(
            matches!(base.host(), Some(url::Host::Domain(_))),
            "resolution override requires a URL hostname"
        );
        ensure!(
            matches!(
                (family, ip),
                (Family::Ipv4, IpAddr::V4(_)) | (Family::Ipv6, IpAddr::V6(_))
            ),
            "resolution override does not match address family"
        );
        ensure!(
            !ip.is_unspecified()
                && !ip.is_multicast()
                && !matches!(ip, IpAddr::V4(v) if v.is_broadcast()),
            "resolution override must be a concrete unicast IP"
        );
        SocketAddr::new(ip, port)
    } else {
        timeout(Duration::from_secs(5), resolve_target(host, port, family))
            .await
            .context("target DNS timed out")??
    };
    let mut builder = Client::builder()
        .http1_only()
        .no_proxy()
        .timeout(Duration::from_millis(timeout_ms))
        .redirect(reqwest::redirect::Policy::none());
    if matches!(base.host(), Some(url::Host::Domain(_))) {
        builder = builder.resolve(host, selected);
    }
    if let Some(ip) = source {
        builder = builder.local_address(ip);
    }
    if close {
        builder = builder.pool_max_idle_per_host(0);
    }
    Ok(Transport::Http(HttpTransport {
        client: builder.build()?,
        base,
        close,
        allow_private,
    }))
}

async fn resolve_udp_target(raw: &str, family: Family) -> Result<SocketAddr> {
    let placeholder = format!("udp://{raw}");
    let parsed = Url::parse(&placeholder).context("invalid UDP target")?;
    ensure!(
        parsed.path().is_empty() && parsed.query().is_none() && parsed.fragment().is_none(),
        "UDP target must only contain host:port"
    );
    let host = parsed.host_str().context("missing UDP host")?;
    let port = parsed.port().context("missing UDP port")?;
    timeout(Duration::from_secs(5), resolve_target(host, port, family))
        .await
        .context("target DNS timed out")?
}

async fn make_udp_address(
    target: SocketAddr,
    family: Family,
    source: Option<IpAddr>,
    allow_private: bool,
) -> Result<Transport> {
    let default_ip = match family {
        Family::Ipv4 => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        Family::Ipv6 => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    let bind = SocketAddr::new(source.unwrap_or(default_ip), 0);
    let socket = UdpSocket::bind(bind)
        .await
        .context("cannot bind UDP client")?;
    socket
        .connect(target)
        .await
        .context("cannot connect UDP client")?;
    Ok(Transport::Udp(UdpTransport {
        socket,
        target,
        connection: None,
        v6: family == Family::Ipv6,
        allow_private,
        next_connect_at: None,
        connect_failures: 0,
        last_timeout_refresh: None,
    }))
}

async fn make_udp(
    raw: &str,
    family: Family,
    source: Option<IpAddr>,
    allow_private: bool,
) -> Result<Transport> {
    make_udp_address(
        resolve_udp_target(raw, family).await?,
        family,
        source,
        allow_private,
    )
    .await
}

fn request_url(base: &Url, req: &Request, operation: Operation) -> Result<Url, RunError> {
    let mut url = base.clone();
    let query = match operation {
        Operation::Connect => return Err(RunError::Protocol(ReplyError::Malformed)),
        Operation::Scrape => {
            let Some(prefix) = base.path().strip_suffix("announce") else {
                return Err(RunError::Protocol(ReplyError::Malformed));
            };
            url.set_path(&format!("{prefix}scrape"));
            format!("info_hash={}", percent(&req.hash))
        }
        Operation::Announce => {
            let event = match req.event {
                1 => "&event=completed",
                2 => "&event=started",
                3 => "&event=stopped",
                _ => "",
            };
            format!(
                "info_hash={}&peer_id={}&port={}&uploaded=0&downloaded=0&left=1&compact=1&numwant={}&key={}{}",
                percent(&req.hash),
                percent(&req.peer_id),
                req.port,
                req.numwant,
                req.key,
                event
            )
        }
    };
    url.set_query(Some(&query));
    if url.as_str().len() > MAX_TARGET_BYTES + 512 {
        return Err(RunError::TooLarge);
    }
    Ok(url)
}

impl Transport {
    fn retry_at(&self) -> Option<Instant> {
        match self {
            Self::Udp(udp) => udp.next_connect_at,
            Self::Http(_) => None,
        }
    }

    fn note_result(&mut self, result: &Result<TrackerReply, RunError>) {
        let Self::Udp(udp) = self else { return };
        if result.is_ok() {
            udp.connect_failures = 0;
            udp.next_connect_at = None;
            return;
        }
        if matches!(result, Err(RunError::Timeout)) && udp.connection.is_some() {
            // A restarted tracker can silently discard an old connection ID.
            // Permit one forced refresh per minute per socket, avoiding an
            // extra connect for every timeout during prolonged overload.
            if udp
                .last_timeout_refresh
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(60))
            {
                udp.connection = None;
                udp.last_timeout_refresh = Some(Instant::now());
                udp.next_connect_at = None;
            }
        } else if udp.connection.is_none() || matches!(result, Err(RunError::Transport)) {
            udp.connect_failures = udp.connect_failures.saturating_add(1).min(6);
            let seconds = (1_u64 << udp.connect_failures).min(60);
            udp.next_connect_at = Some(Instant::now() + Duration::from_secs(seconds));
        }
    }

    fn reservation(&self) -> u64 {
        match self {
            Self::Http(_) => MAX_HTTP_BODY + 1 + (MAX_TARGET_BYTES as u64) + 512,
            // Up to four stale datagrams may be consumed for each of the
            // connect and operation transactions, all within the reservation.
            Self::Udp(_) => 16 + 98 + (MAX_UDP_PACKET + 1) * 8,
        }
    }

    async fn execute(
        &mut self,
        req: &Request,
        op: Operation,
        io: &mut IoCounts,
        http_status: &mut Option<u16>,
    ) -> Result<TrackerReply, RunError> {
        match self {
            Self::Http(http) => {
                let url = request_url(&http.base, req, op)?;
                // Deliberately excludes request headers, TLS records, TCP and retransmits.
                io.tx += url.path().len() as u64 + url.query().map_or(0, |q| 1 + q.len() as u64);
                let mut request = http.client.get(url);
                if http.close {
                    request = request.header(header::CONNECTION, "close");
                }
                let response = request.send().await.map_err(|e| {
                    if e.is_timeout() {
                        RunError::Timeout
                    } else {
                        RunError::Transport
                    }
                })?;
                let status = response.status().as_u16();
                *http_status = Some(status);
                if !(200..300).contains(&status) {
                    return Err(RunError::HttpStatus(status));
                }
                let stream = response.bytes_stream().map_err(std::io::Error::other);
                let reader = StreamReader::new(stream);
                tokio::pin!(reader);
                let mut body = Vec::new();
                let mut buf = [0_u8; 4096];
                loop {
                    let allowance = (MAX_HTTP_BODY + 1).saturating_sub(body.len() as u64) as usize;
                    if allowance == 0 {
                        return Err(RunError::TooLarge);
                    }
                    let n = reader
                        .as_mut()
                        .read(&mut buf[..allowance.min(4096)])
                        .await
                        .map_err(|_| RunError::Transport)?;
                    if n == 0 {
                        break;
                    }
                    io.rx += n as u64;
                    body.extend_from_slice(&buf[..n]);
                    if body.len() as u64 > MAX_HTTP_BODY {
                        return Err(RunError::TooLarge);
                    }
                }
                match op {
                    Operation::Announce => {
                        parse_http_announce(&body, req.numwant as usize, http.allow_private)
                            .map_err(RunError::Protocol)
                    }
                    Operation::Scrape => parse_http_scrape(&body, &req.hash)
                        .map(|(complete, incomplete)| TrackerReply {
                            complete,
                            incomplete,
                            ..Default::default()
                        })
                        .map_err(RunError::Protocol),
                    Operation::Connect => Err(RunError::Protocol(ReplyError::Malformed)),
                }
            }
            Self::Udp(udp) => {
                let _ = udp.target; // connected socket restricts replies to the selected owned target
                if matches!(op, Operation::Connect) {
                    udp.connection = None;
                }
                let conn = match udp.connection {
                    Some((id, created)) if created.elapsed() < Duration::from_secs(55) => id,
                    _ => {
                        let tx = rand::random::<u32>();
                        let packet = udp_connect_request(tx);
                        io.tx += udp
                            .socket
                            .send(&packet)
                            .await
                            .map_err(|_| RunError::Transport)?
                            as u64;
                        io.udp_connects += 1;
                        let data = receive_udp(&udp.socket, tx, io).await?;
                        let id = parse_udp_connect(&data, tx).map_err(RunError::Protocol)?;
                        io.udp_connections_ok += 1;
                        udp.connection = Some((id, Instant::now()));
                        id
                    }
                };
                if matches!(op, Operation::Connect) {
                    return Ok(TrackerReply::default());
                }
                let tx = rand::random::<u32>();
                let packet = match op {
                    Operation::Announce => udp_announce_request(
                        conn,
                        tx,
                        &Announce {
                            hash: &req.hash,
                            peer_id: &req.peer_id,
                            port: req.port,
                            event: req.event,
                            numwant: i32::from(req.numwant),
                            key: req.key,
                        },
                    ),
                    Operation::Scrape => udp_scrape_request(conn, tx, &req.hash),
                    Operation::Connect => unreachable!(),
                };
                io.tx += udp
                    .socket
                    .send(&packet)
                    .await
                    .map_err(|_| RunError::Transport)? as u64;
                if matches!(op, Operation::Announce) {
                    io.udp_announces += 1;
                }
                if matches!(op, Operation::Scrape) {
                    io.udp_scrapes += 1;
                }
                let data = receive_udp(&udp.socket, tx, io).await?;
                if udp_cookie_rejected(&data) {
                    udp.connection = None;
                }
                match op {
                    Operation::Announce => parse_udp_announce(
                        &data,
                        tx,
                        udp.v6,
                        req.numwant as usize,
                        udp.allow_private,
                    )
                    .map_err(RunError::Protocol),
                    Operation::Scrape => parse_udp_scrape(&data, tx)
                        .map(|(complete, incomplete)| TrackerReply {
                            complete,
                            incomplete,
                            ..Default::default()
                        })
                        .map_err(RunError::Protocol),
                    Operation::Connect => unreachable!(),
                }
            }
        }
    }
}

async fn receive_udp(socket: &UdpSocket, tx: u32, io: &mut IoCounts) -> Result<Vec<u8>, RunError> {
    let mut buf = [0_u8; MAX_UDP_PACKET as usize + 1];
    for _ in 0..4 {
        let n = socket
            .recv(&mut buf)
            .await
            .map_err(|_| RunError::Transport)?;
        io.rx += n as u64;
        if n >= 8 && u32::from_be_bytes(buf[4..8].try_into().unwrap()) != tx {
            io.udp_stale_packets += 1;
            continue;
        }
        return Ok(buf[..n].to_vec());
    }
    Err(RunError::UnmatchedDatagrams)
}

fn hash_from(seed: &[u8; 16], index: u64, kind: u8, peer: u16) -> [u8; 20] {
    let mut h = Sha1::new();
    h.update(seed);
    h.update([kind]);
    h.update(index.to_be_bytes());
    h.update(peer.to_be_bytes());
    h.finalize().into()
}

struct Shared {
    budget: ByteBudget,
    sent: AtomicU64,
    ok: AtomicU64,
    timeout: AtomicU64,
    transport_error: AtomicU64,
    business_error: AtomicU64,
    invalid_reply: AtomicU64,
    non_public_peer_responses: AtomicU64,
    http_error: AtomicU64,
    http_announces: AtomicU64,
    http_scrapes: AtomicU64,
    udp_announces: AtomicU64,
    udp_scrapes: AtomicU64,
    udp_connects: AtomicU64,
    udp_connections_ok: AtomicU64,
    udp_stale_packets: AtomicU64,
    udp_unmatched_limit: AtomicU64,
    http_statuses: Mutex<BTreeMap<u16, u64>>,
    latency: Histogram,
    success_latency: Histogram,
    next_ns: AtomicU64,
    budget_exhausted: AtomicBool,
    cpu_start: Option<cpu_time::ProcessTime>,
    start: Instant,
    period_ns: u64,
}
impl Shared {
    fn new(max_bytes: u64, rate: u32) -> Self {
        Self {
            budget: ByteBudget::new(max_bytes),
            sent: AtomicU64::new(0),
            ok: AtomicU64::new(0),
            timeout: AtomicU64::new(0),
            transport_error: AtomicU64::new(0),
            business_error: AtomicU64::new(0),
            invalid_reply: AtomicU64::new(0),
            non_public_peer_responses: AtomicU64::new(0),
            http_error: AtomicU64::new(0),
            http_announces: AtomicU64::new(0),
            http_scrapes: AtomicU64::new(0),
            udp_announces: AtomicU64::new(0),
            udp_scrapes: AtomicU64::new(0),
            udp_connects: AtomicU64::new(0),
            udp_connections_ok: AtomicU64::new(0),
            udp_stale_packets: AtomicU64::new(0),
            udp_unmatched_limit: AtomicU64::new(0),
            http_statuses: Mutex::new(BTreeMap::new()),
            latency: Histogram::default(),
            success_latency: Histogram::default(),
            next_ns: AtomicU64::new(0),
            budget_exhausted: AtomicBool::new(false),
            cpu_start: cpu_time::ProcessTime::try_now().ok(),
            start: Instant::now(),
            period_ns: 1_000_000_000 / u64::from(rate),
        }
    }
    fn schedule(&self) -> Instant {
        loop {
            let old = self.next_ns.load(Ordering::Relaxed);
            let now = self.start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            let slot = old.max(now);
            if self
                .next_ns
                .compare_exchange_weak(
                    old,
                    slot.saturating_add(self.period_ns),
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                return self.start + Duration::from_nanos(slot);
            }
        }
    }
    fn record(&self, result: &Result<TrackerReply, RunError>, status: Option<u16>, millis: u64) {
        self.latency.record(millis);
        if let Some(code) = status {
            *self.http_statuses.lock().unwrap().entry(code).or_default() += 1;
        }
        match result {
            Ok(_) => {
                self.success_latency.record(millis);
                self.ok.fetch_add(1, Ordering::Relaxed);
            }
            Err(RunError::Timeout) => {
                self.timeout.fetch_add(1, Ordering::Relaxed);
            }
            Err(RunError::Transport) => {
                self.transport_error.fetch_add(1, Ordering::Relaxed);
            }
            Err(RunError::Protocol(ReplyError::TrackerFailure)) => {
                self.business_error.fetch_add(1, Ordering::Relaxed);
            }
            Err(RunError::Protocol(ReplyError::NonPublicPeer)) => {
                self.non_public_peer_responses
                    .fetch_add(1, Ordering::Relaxed);
                self.invalid_reply.fetch_add(1, Ordering::Relaxed);
            }
            Err(RunError::Protocol(_) | RunError::TooLarge) => {
                self.invalid_reply.fetch_add(1, Ordering::Relaxed);
            }
            Err(RunError::UnmatchedDatagrams) => {
                self.udp_unmatched_limit.fetch_add(1, Ordering::Relaxed);
            }
            Err(RunError::HttpStatus(code)) => {
                let _ = code;
                self.http_error.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Serialize)]
struct Report {
    kind: &'static str,
    protocol: String,
    operation: String,
    elapsed_seconds: f64,
    configured_rps: u32,
    attempted: u64,
    completed: u64,
    protocol_success: u64,
    protocol_success_rate: Option<f64>,
    http_announce_attempts: u64,
    http_scrape_attempts: u64,
    udp_announce_packets_sent: u64,
    udp_scrape_packets_sent: u64,
    udp_connect_packets_sent: u64,
    udp_connect_responses_ok: u64,
    udp_stale_datagrams: u64,
    udp_unmatched_limit: u64,
    tracker_business_error: u64,
    http_error: u64,
    timeout: u64,
    transport_error: u64,
    invalid_protocol_reply: u64,
    non_public_peer_responses: u64,
    allow_private_test: bool,
    http_resolve_override: bool,
    http_statuses: BTreeMap<u16, u64>,
    actual_attempted_rps: f64,
    actual_success_rps: f64,
    generator_cpu_cores: Option<f64>,
    latency_p50_ms_upper_bound: Option<u64>,
    latency_p95_ms_upper_bound: Option<u64>,
    latency_p99_ms_upper_bound: Option<u64>,
    success_latency_p50_ms_upper_bound: Option<u64>,
    success_latency_p95_ms_upper_bound: Option<u64>,
    success_latency_p99_ms_upper_bound: Option<u64>,
    histogram_note: &'static str,
    tx_application_bytes: u64,
    rx_application_bytes: u64,
    tx_application_mbps: f64,
    rx_application_mbps: f64,
    remaining_application_budget_bytes: u64,
    accounting_note: &'static str,
    budget_exhausted: bool,
    possible_generator_or_target_saturation: bool,
}

fn report(shared: &Shared, args: &Args, kind: &'static str) -> Report {
    let elapsed = shared.start.elapsed().as_secs_f64().max(0.001);
    let sent = shared.sent.load(Ordering::Relaxed);
    let ok = shared.ok.load(Ordering::Relaxed);
    let business_error = shared.business_error.load(Ordering::Relaxed);
    let http_error = shared.http_error.load(Ordering::Relaxed);
    let timed_out = shared.timeout.load(Ordering::Relaxed);
    let transport_error = shared.transport_error.load(Ordering::Relaxed);
    let invalid_reply = shared.invalid_reply.load(Ordering::Relaxed);
    let unmatched = shared.udp_unmatched_limit.load(Ordering::Relaxed);
    let completed =
        ok + business_error + http_error + timed_out + transport_error + invalid_reply + unmatched;
    let (tx, rx, remaining) = shared.budget.totals();
    let exhausted = shared.budget_exhausted.load(Ordering::Relaxed);
    Report {
        kind,
        protocol: format!("{:?}", args.protocol).to_lowercase(),
        operation: format!("{:?}", args.operation).to_lowercase(),
        elapsed_seconds: elapsed,
        configured_rps: args.rate,
        attempted: sent,
        completed,
        protocol_success: ok,
        protocol_success_rate: (completed > 0).then(|| ok as f64 / completed as f64),
        http_announce_attempts: shared.http_announces.load(Ordering::Relaxed),
        http_scrape_attempts: shared.http_scrapes.load(Ordering::Relaxed),
        udp_announce_packets_sent: shared.udp_announces.load(Ordering::Relaxed),
        udp_scrape_packets_sent: shared.udp_scrapes.load(Ordering::Relaxed),
        udp_connect_packets_sent: shared.udp_connects.load(Ordering::Relaxed),
        udp_connect_responses_ok: shared.udp_connections_ok.load(Ordering::Relaxed),
        udp_stale_datagrams: shared.udp_stale_packets.load(Ordering::Relaxed),
        udp_unmatched_limit: unmatched,
        tracker_business_error: business_error,
        http_error,
        timeout: timed_out,
        transport_error,
        invalid_protocol_reply: invalid_reply,
        non_public_peer_responses: shared.non_public_peer_responses.load(Ordering::Relaxed),
        allow_private_test: args.allow_private_test,
        http_resolve_override: args.resolve_ip.is_some(),
        http_statuses: shared.http_statuses.lock().unwrap().clone(),
        actual_attempted_rps: sent as f64 / elapsed,
        actual_success_rps: ok as f64 / elapsed,
        generator_cpu_cores: shared
            .cpu_start
            .and_then(|started| started.try_elapsed().ok())
            .map(|cpu| cpu.as_secs_f64() / elapsed),
        latency_p50_ms_upper_bound: shared.latency.percentile_ms(50, 100),
        latency_p95_ms_upper_bound: shared.latency.percentile_ms(95, 100),
        latency_p99_ms_upper_bound: shared.latency.percentile_ms(99, 100),
        success_latency_p50_ms_upper_bound: shared.success_latency.percentile_ms(50, 100),
        success_latency_p95_ms_upper_bound: shared.success_latency.percentile_ms(95, 100),
        success_latency_p99_ms_upper_bound: shared.success_latency.percentile_ms(99, 100),
        histogram_note: "1 ms buckets; 60000 denotes >= 60 s, not a finite upper bound",
        tx_application_bytes: tx,
        rx_application_bytes: rx,
        tx_application_mbps: tx as f64 * 8.0 / elapsed / 1_000_000.0,
        rx_application_mbps: rx as f64 * 8.0 / elapsed / 1_000_000.0,
        remaining_application_budget_bytes: remaining,
        accounting_note: "HTTP request target and response body plus UDP payloads only; excludes HTTP headers, TLS, IP/TCP, retransmissions and buffered unread bytes; monitor host NIC separately",
        budget_exhausted: exhausted,
        possible_generator_or_target_saturation: elapsed >= 5.0
            && !exhausted
            && (sent as f64 / elapsed) < f64::from(args.rate) * 0.9,
    }
}

async fn pressure_worker(
    mut transport: Transport,
    args: Args,
    shared: Arc<Shared>,
    seed: [u8; 16],
    sequence: Arc<AtomicU64>,
    deadline: Instant,
    protocol_parity: Option<u64>,
) {
    loop {
        if shared.budget_exhausted.load(Ordering::Relaxed) {
            break;
        }
        if let Some(retry_at) = transport.retry_at() {
            if retry_at >= deadline {
                break;
            }
            if retry_at > Instant::now() {
                sleep_until(retry_at).await;
            }
        }
        let slot = shared.schedule();
        if slot >= deadline {
            break;
        }
        sleep_until(slot).await;
        if Instant::now() >= deadline {
            break;
        }
        let seq = sequence.fetch_add(1, Ordering::Relaxed);
        let logical = match protocol_parity {
            Some(p) => seq * 2 + p,
            None => seq,
        };
        let hash_index = (logical / u64::from(args.peers_per_hash)) % args.unique_hashes;
        let peer_index = (logical % u64::from(args.peers_per_hash)) as u16;
        let operation = match args.operation {
            Workload::Announce => Operation::Announce,
            Workload::Scrape => Operation::Scrape,
            Workload::Connect => Operation::Connect,
            Workload::Mixed if logical % 5 == 4 => Operation::Scrape,
            Workload::Mixed => Operation::Announce,
        };
        let event = if logical
            < args
                .unique_hashes
                .saturating_mul(u64::from(args.peers_per_hash))
        {
            2
        } else {
            0
        };
        let req = Request {
            hash: hash_from(&seed, hash_index, 0, 0),
            peer_id: hash_from(&seed, hash_index, 1, peer_index),
            port: args.base_port + peer_index,
            event,
            numwant: args.numwant,
            key: u32::from(peer_index),
        };
        let reservation = transport.reservation();
        if !shared.budget.reserve(reservation) {
            shared.budget_exhausted.store(true, Ordering::Release);
            break;
        }
        shared.sent.fetch_add(1, Ordering::Relaxed);
        if matches!(&transport, Transport::Http(_)) {
            let counter = match operation {
                Operation::Announce => &shared.http_announces,
                Operation::Scrape => &shared.http_scrapes,
                Operation::Connect => unreachable!(),
            };
            counter.fetch_add(1, Ordering::Relaxed);
        }
        let mut io = IoCounts::default();
        let mut status = None;
        let started = Instant::now();
        let result = match timeout(
            Duration::from_millis(args.timeout_ms),
            transport.execute(&req, operation, &mut io, &mut status),
        )
        .await
        {
            Ok(value) => value,
            Err(_) => Err(RunError::Timeout),
        };
        transport.note_result(&result);
        shared
            .udp_connects
            .fetch_add(io.udp_connects, Ordering::Relaxed);
        shared
            .udp_connections_ok
            .fetch_add(io.udp_connections_ok, Ordering::Relaxed);
        shared
            .udp_announces
            .fetch_add(io.udp_announces, Ordering::Relaxed);
        shared
            .udp_scrapes
            .fetch_add(io.udp_scrapes, Ordering::Relaxed);
        shared
            .udp_stale_packets
            .fetch_add(io.udp_stale_packets, Ordering::Relaxed);
        shared.budget.settle(reservation, io.tx, io.rx);
        shared.record(
            &result,
            status,
            started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        );
    }
}

async fn pressure(args: Args) -> Result<()> {
    // Resolve each target once and allocate all client transports before the
    // measurement clock starts. This prevents startup DNS/setup from consuming
    // the requested test window or producing a false saturation signal.
    let http_template = if args.protocol != Protocol::Udp {
        let transport = make_http(
            args.http_url.as_deref().unwrap(),
            args.family,
            args.new_connections,
            args.timeout_ms,
            args.source_ip,
            args.allow_private_test,
            args.resolve_ip,
        )
        .await?;
        let Transport::Http(http) = transport else {
            unreachable!()
        };
        Some(http)
    } else {
        None
    };
    let udp_target = if matches!(args.protocol, Protocol::Udp | Protocol::Mixed) {
        Some(resolve_udp_target(args.udp_target.as_deref().unwrap(), args.family).await?)
    } else {
        None
    };
    let mut prepared = Vec::with_capacity(args.concurrency);
    for worker in 0..args.concurrency {
        let use_udp =
            args.protocol == Protocol::Udp || (args.protocol == Protocol::Mixed && worker % 2 == 1);
        let transport = if use_udp {
            make_udp_address(
                udp_target.expect("UDP target resolved"),
                args.family,
                args.source_ip,
                args.allow_private_test,
            )
            .await?
        } else {
            Transport::Http(
                http_template
                    .as_ref()
                    .expect("HTTP target resolved")
                    .clone(),
            )
        };
        prepared.push((transport, use_udp));
    }
    let shared = Arc::new(Shared::new(args.max_bytes, args.rate));
    let seed = rand::random::<[u8; 16]>();
    let seq_http = Arc::new(AtomicU64::new(0));
    let seq_udp = Arc::new(AtomicU64::new(0));
    let deadline = shared.start + Duration::from_secs(args.duration);
    let mut tasks = tokio::task::JoinSet::new();
    for (transport, use_udp) in prepared {
        let seq = if use_udp {
            seq_udp.clone()
        } else {
            seq_http.clone()
        };
        let parity = if args.protocol == Protocol::Mixed {
            Some(if use_udp { 1 } else { 0 })
        } else {
            None
        };
        tasks.spawn(pressure_worker(
            transport,
            args.clone(),
            shared.clone(),
            seed,
            seq,
            deadline,
            parity,
        ));
    }
    let progress = if args.progress {
        let s = shared.clone();
        let a = args.clone();
        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if Instant::now() >= deadline || s.budget_exhausted.load(Ordering::Relaxed) {
                    break;
                }
                println!(
                    "{}",
                    serde_json::to_string(&report(&s, &a, "progress")).unwrap()
                );
            }
        }))
    } else {
        None
    };
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result {
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
            if let Some(progress) = &progress {
                progress.abort();
            }
            return Err(anyhow::anyhow!("pressure worker failed: {error}"));
        }
    }
    if let Some(progress) = progress {
        progress.abort();
        let _ = progress.await;
    }
    println!(
        "{}",
        serde_json::to_string(&report(&shared, &args, "final"))?
    );
    Ok(())
}

async fn verify_operation(
    transport: &mut Transport,
    req: &Request,
    op: Operation,
    shared: &Shared,
    args: &Args,
) -> Result<TrackerReply> {
    if transport.retry_at().is_some_and(|at| at > Instant::now()) {
        bail!("UDP connection is in bounded retry backoff");
    }
    let reservation = transport.reservation();
    ensure!(
        shared.budget.reserve(reservation),
        "application byte budget exhausted during verify"
    );
    let mut io = IoCounts::default();
    let mut status = None;
    let result = match timeout(
        Duration::from_millis(args.timeout_ms),
        transport.execute(req, op, &mut io, &mut status),
    )
    .await
    {
        Ok(value) => value,
        Err(_) => Err(RunError::Timeout),
    };
    transport.note_result(&result);
    shared.budget.settle(reservation, io.tx, io.rx);
    result.map_err(|err| {
        anyhow::anyhow!("tracker protocol operation failed: {err:?}; HTTP status {status:?}")
    })
}

async fn wait_for_verify_delay(
    delay: Duration,
    cancel: impl std::future::Future<Output = std::io::Result<()>>,
) -> Result<Duration> {
    let started = Instant::now();
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(started.elapsed()),
        result = cancel => {
            result.context("could not receive Ctrl-C notification")?;
            bail!("verification interrupted; stop cleanup attempted")
        }
    }
}

fn now_utc_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn verify(args: Args) -> Result<()> {
    let shared = Shared::new(args.max_bytes, args.rate);
    let source_a = args.source_a_ip.or(args.source_ip);
    let source_b = args.source_b_ip.or(args.source_ip);
    let family_a = args.family_a.unwrap_or(args.family);
    let family_b = args.family_b.unwrap_or(args.family);
    let (mut a, mut b) = match args.protocol {
        Protocol::Http | Protocol::Https => (
            make_http(
                args.http_url.as_deref().unwrap(),
                family_a,
                false,
                args.timeout_ms,
                source_a,
                args.allow_private_test,
                args.resolve_a_ip.or(args.resolve_ip),
            )
            .await?,
            make_http(
                args.http_url.as_deref().unwrap(),
                family_b,
                false,
                args.timeout_ms,
                source_b,
                args.allow_private_test,
                args.resolve_b_ip.or(args.resolve_ip),
            )
            .await?,
        ),
        Protocol::Udp => (
            make_udp(
                args.udp_target.as_deref().unwrap(),
                family_a,
                source_a,
                args.allow_private_test,
            )
            .await?,
            make_udp(
                args.udp_target.as_deref().unwrap(),
                family_b,
                source_b,
                args.allow_private_test,
            )
            .await?,
        ),
        Protocol::Mixed => (
            make_http(
                args.http_url.as_deref().unwrap(),
                family_a,
                false,
                args.timeout_ms,
                source_a,
                args.allow_private_test,
                args.resolve_a_ip.or(args.resolve_ip),
            )
            .await?,
            make_udp(
                args.udp_target.as_deref().unwrap(),
                family_b,
                source_b,
                args.allow_private_test,
            )
            .await?,
        ),
    };
    let seed = rand::random::<[u8; 16]>();
    let hash = hash_from(&seed, 0, 0, 0);
    let req_a = Request {
        hash,
        peer_id: hash_from(&seed, 0, 1, 1),
        port: args.base_port,
        event: 2,
        numwant: 0,
        key: 1,
    };
    let req_b = Request {
        hash,
        peer_id: hash_from(&seed, 0, 1, 2),
        port: args.base_port + 1,
        event: 2,
        numwant: 50,
        key: 2,
    };
    let expected_a = SocketAddr::new(
        args.expected_peer_a_ip
            .or(args.expected_client_ip)
            .context("missing expected client A IP")?,
        req_a.port,
    );
    let expected_b = SocketAddr::new(
        args.expected_peer_b_ip
            .or(args.expected_client_ip)
            .context("missing expected client B IP")?,
        req_b.port,
    );
    let req_a_want = Request {
        numwant: 50,
        ..req_a.clone()
    };
    let stop_b = Request {
        event: 3,
        numwant: 0,
        ..req_b.clone()
    };
    let stop_a = Request {
        event: 3,
        numwant: 0,
        ..req_a.clone()
    };
    let observer = Request {
        hash,
        peer_id: hash_from(&seed, 0, 1, 3),
        port: args.base_port + 2,
        event: 2,
        numwant: 50,
        key: 3,
    };
    let expected_observer = SocketAddr::new(expected_a.ip(), observer.port);
    let stop_observer = Request {
        event: 3,
        numwant: 0,
        ..observer.clone()
    };
    let fresh = Request {
        hash: hash_from(&seed, 1, 0, 0),
        numwant: 50,
        ..req_a.clone()
    };
    let stop_fresh = Request {
        event: 3,
        numwant: 0,
        ..fresh.clone()
    };
    // Keep sufficient budget reserved for four best-effort stop notices even
    // when a verification step times out or the regular budget is exhausted.
    let cleanup_reservation = a.reservation().max(b.reservation()) * 4;
    ensure!(
        shared.budget.reserve(cleanup_reservation),
        "max-bytes too small for verification and cleanup"
    );
    let mut real_wait_seconds = None;
    let validation: Result<()> = async {
        let first = verify_operation(&mut a, &req_a, Operation::Announce, &shared, &args).await?;
        ensure!(first.peers.is_empty(), "numwant=0 returned peers");
        let b_view = verify_operation(&mut b, &req_b, Operation::Announce, &shared, &args).await?;
        ensure!(
            b_view.peers.contains(&expected_a),
            "second client did not receive the first client's exact IP and port"
        );
        ensure!(
            !b_view.peers.contains(&expected_b),
            "tracker returned the requesting client to itself"
        );
        let a_view =
            verify_operation(&mut a, &req_a_want, Operation::Announce, &shared, &args).await?;
        ensure!(
            a_view.peers.contains(&expected_b),
            "first client did not receive the second client's exact IP and port"
        );
        ensure!(
            !a_view.peers.contains(&expected_a),
            "tracker returned the requesting client to itself"
        );
        let scrape_two =
            verify_operation(&mut a, &req_a_want, Operation::Scrape, &shared, &args).await?;
        ensure!(
            scrape_two.complete + scrape_two.incomplete == 2,
            "scrape did not report two registered clients"
        );
        if args.verify_delay_secs > 0 {
            println!(
                "{}",
                serde_json::json!({"kind":"verify_phase","stage":"retention_wait_started",
                "planned_seconds":args.verify_delay_secs,"timestamp_utc":now_utc_seconds()})
            );
            let elapsed = wait_for_verify_delay(
                Duration::from_secs(args.verify_delay_secs),
                tokio::signal::ctrl_c(),
            )
            .await?;
            real_wait_seconds = Some(elapsed.as_secs_f64());
            println!(
                "{}",
                serde_json::json!({"kind":"verify_phase","stage":"retention_wait_completed",
                "actual_seconds":elapsed.as_secs_f64(),"timestamp_utc":now_utc_seconds()})
            );
            // A third, distinct registration observes the original two before
            // either one is refreshed. Otherwise a failed retention could be
            // hidden when the first original client re-announces.
            let observed =
                verify_operation(&mut a, &observer, Operation::Announce, &shared, &args).await?;
            ensure!(
                observed.peers.contains(&expected_a) && observed.peers.contains(&expected_b),
                "after the real wait the observer did not receive both original endpoints"
            );
            ensure!(
                !observed.peers.contains(&expected_observer),
                "tracker returned the observer to itself"
            );
            verify_operation(&mut a, &stop_observer, Operation::Announce, &shared, &args).await?;
            let delayed_a = Request {
                event: 0,
                ..req_a_want.clone()
            };
            let delayed_b = Request {
                event: 0,
                ..req_b.clone()
            };
            let a_after_wait =
                verify_operation(&mut a, &delayed_a, Operation::Announce, &shared, &args).await?;
            ensure!(
                a_after_wait.peers.contains(&expected_b)
                    && !a_after_wait.peers.contains(&expected_a),
                "client A did not receive the exact client B endpoint after the real wait"
            );
            let b_after_wait =
                verify_operation(&mut b, &delayed_b, Operation::Announce, &shared, &args).await?;
            ensure!(
                b_after_wait.peers.contains(&expected_a)
                    && !b_after_wait.peers.contains(&expected_b),
                "client B did not receive the exact client A endpoint after the real wait"
            );
        }
        verify_operation(&mut b, &stop_b, Operation::Announce, &shared, &args).await?;
        let after_stop =
            verify_operation(&mut a, &req_a_want, Operation::Announce, &shared, &args).await?;
        ensure!(
            !after_stop.peers.contains(&expected_b),
            "stopped peer was still returned"
        );
        let scrape_one =
            verify_operation(&mut a, &req_a_want, Operation::Scrape, &shared, &args).await?;
        ensure!(
            scrape_one.complete + scrape_one.incomplete == 1,
            "scrape did not report one registered client after stopped"
        );
        verify_operation(&mut a, &stop_a, Operation::Announce, &shared, &args).await?;
        let scrape_zero =
            verify_operation(&mut a, &req_a_want, Operation::Scrape, &shared, &args).await?;
        ensure!(
            scrape_zero.complete + scrape_zero.incomplete == 0,
            "scrape did not report zero after both clients stopped"
        );
        let random_view =
            verify_operation(&mut a, &fresh, Operation::Announce, &shared, &args).await?;
        ensure!(
            random_view.peers.is_empty(),
            "new random resource returned unrelated peers"
        );
        Ok(())
    }
    .await;
    shared.budget.settle(cleanup_reservation, 0, 0);
    let mut cleanup_failures = 0_u8;
    if verify_operation(&mut a, &stop_a, Operation::Announce, &shared, &args)
        .await
        .is_err()
    {
        cleanup_failures += 1;
    }
    if verify_operation(&mut b, &stop_b, Operation::Announce, &shared, &args)
        .await
        .is_err()
    {
        cleanup_failures += 1;
    }
    if verify_operation(&mut a, &stop_observer, Operation::Announce, &shared, &args)
        .await
        .is_err()
    {
        cleanup_failures += 1;
    }
    if verify_operation(&mut a, &stop_fresh, Operation::Announce, &shared, &args)
        .await
        .is_err()
    {
        cleanup_failures += 1;
    }
    validation?;
    ensure!(
        cleanup_failures == 0,
        "verification passed, but one or more best-effort stop requests failed"
    );
    let (tx, rx, remaining) = shared.budget.totals();
    println!(
        "{}",
        serde_json::json!({"kind":"verify","ok":true,"mixed_transports":args.protocol == Protocol::Mixed,
        "family_a":format!("{family_a:?}").to_lowercase(),"family_b":format!("{family_b:?}").to_lowercase(),
        "allow_private_test":args.allow_private_test,"delayed_retention_checked":args.verify_delay_secs > 0,
        "http_resolve_override_a":args.protocol != Protocol::Udp && args.resolve_a_ip.or(args.resolve_ip).is_some(),
        "http_resolve_override_b":matches!(args.protocol, Protocol::Http | Protocol::Https) && args.resolve_b_ip.or(args.resolve_ip).is_some(),
        "delay_model":"real_wall_time","real_wait_seconds":real_wait_seconds,
        "checks":["zero_want","two_way_discovery","scrape_2_1_0","stopped_removal","unrelated_hash_empty"],"tx_application_bytes":tx,"rx_application_bytes":rx,"remaining_application_budget_bytes":remaining,"accounting_note":"HTTP request target/body and UDP payload only; monitor host NIC for actual traffic"})
    );
    Ok(())
}

/// Synthetic private addresses never leave this process. This is a state-engine
/// microbenchmark, not an HTTP/UDP throughput or S1-capacity measurement.
#[derive(Serialize)]
struct StateBenchReport {
    kind: &'static str,
    state_workload: &'static str,
    network_used: bool,
    qualification: &'static str,
    logical_admission_start_seconds: u64,
    logical_admission_end_seconds: u64,
    logical_last_attempt_seconds: u64,
    logical_delayed_observation_origin_seconds: u64,
    logical_cleanup_seconds: u64,
    requested_peers: usize,
    requested_swarms: usize,
    memory_budget_mib: usize,
    accepted: usize,
    stop_reason: Option<String>,
    elapsed_seconds: f64,
    in_process_admissions_per_second: f64,
    sample_every_n_admissions: u64,
    sampled_p95_ms_upper_bound: Option<u64>,
    sampled_p99_ms_upper_bound: Option<u64>,
    histogram_note: &'static str,
    engine_stats_before_cleanup: swarm_engine::EngineStats,
    rss_before_cleanup_kib: Option<u64>,
    peak_rss_kib: Option<u64>,
    rss_sample_every_operations: u64,
    sampled_max_rss_kib: Option<u64>,
    admission_phase_vmhwm_kib: Option<u64>,
    full_run_vmhwm_kib: Option<u64>,
    rss_after_cleanup_kib: Option<u64>,
    logical_delayed_peer_visible_600s: Option<bool>,
    logical_delayed_peer_visible_1800s: Option<bool>,
    maintenance_calls: usize,
    max_maintenance_us: u128,
    engine_stats_after_cleanup: swarm_engine::EngineStats,
}

fn state_bench_data(args: &Args) -> Result<StateBenchReport> {
    use bt_protocol::{AnnounceEvent, AnnounceRequest};
    use swarm_engine::{EngineConfig, SwarmEngine};
    let requested_swarms = args.unique_hashes.min(args.state_count as u64) as usize;
    let config = EngineConfig {
        max_peers: args.state_count.saturating_add(2),
        max_swarms: requested_swarms.saturating_add(1),
        max_peers_per_swarm: args
            .state_count
            .div_ceil(requested_swarms)
            .saturating_add(2),
        memory_budget_bytes: args.state_memory_mib * 1024 * 1024,
        allow_private_ips: true,
        ..EngineConfig::default()
    };
    let engine = SwarmEngine::new(config);
    let seed = rand::random::<[u8; 16]>();
    let h = Histogram::default();
    let started = std::time::Instant::now();
    let mut accepted = 0_usize;
    let mut stop_reason = None;
    let mut sampled_max_rss = process_rss_kib("VmRSS:");
    let scatter = args.state_workload == StateWorkload::Scatter;
    let mut admission_end = 1_u64;
    let mut last_attempt = 1_u64;
    for index in 0..args.state_count {
        let address = 0x0a00_0001_u32 + index as u32;
        let peer_in_swarm = index / requested_swarms;
        let ip = if scatter && !peer_in_swarm.is_multiple_of(2) {
            IpAddr::V6(Ipv6Addr::from(
                u128::from(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 0)) + index as u128 + 1,
            ))
        } else {
            IpAddr::V4(Ipv4Addr::from(address))
        };
        let request = AnnounceRequest {
            info_hash: hash_from(&seed, (index % requested_swarms) as u64, 0, 0),
            peer_id: hash_from(&seed, index as u64, 1, 0),
            ip,
            port: 20_000,
            uploaded: 0,
            downloaded: 0,
            left: if scatter && (peer_in_swarm / 2).is_multiple_of(2) {
                0
            } else {
                1
            },
            event: AnnounceEvent::Started,
            numwant: 0,
        };
        let sample_start = if index % 100 == 0 {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let now = if scatter && args.state_count > 1 {
            1 + index as u64 * 1799 / (args.state_count as u64 - 1)
        } else {
            1
        };
        last_attempt = now;
        match engine.announce(&request, now) {
            Ok(_) => {
                accepted += 1;
                admission_end = now;
            }
            Err(error) => {
                stop_reason = Some(error.to_string());
                break;
            }
        }
        if let Some(sample) = sample_start {
            h.record(sample.elapsed().as_millis() as u64);
        }
        if (index + 1).is_multiple_of(1000)
            && let Some(rss) = process_rss_kib("VmRSS:")
        {
            sampled_max_rss = Some(sampled_max_rss.unwrap_or(0).max(rss));
        }
    }
    let elapsed = started.elapsed().as_secs_f64().max(0.000_001);
    let resident_before_cleanup = process_rss_kib("VmRSS:");
    let peak_resident = process_rss_kib("VmHWM:");
    let at_capacity = engine.stats();
    let original_hash = hash_from(&seed, 0, 0, 0);
    let test = AnnounceRequest {
        info_hash: original_hash,
        peer_id: hash_from(&seed, 0, 7, 0),
        ip: IpAddr::V4(Ipv4Addr::new(10, 254, 0, 1)),
        port: 65_000,
        uploaded: 0,
        downloaded: 0,
        left: 1,
        event: AnnounceEvent::Started,
        numwant: 50,
    };
    let mut delayed = Vec::new();
    for now in [last_attempt + 600, last_attempt + 1800] {
        let observed = engine
            .announce(&test, now)
            .ok()
            .map(|reply| !reply.peers.is_empty());
        let stop = AnnounceRequest {
            event: AnnounceEvent::Stopped,
            numwant: 0,
            ..test.clone()
        };
        let _ = engine.announce(&stop, now);
        delayed.push(observed);
    }
    let mut max_maintenance_us = 0_u128;
    let mut maintenance_calls = 0_usize;
    let mut unchanged = 0_usize;
    let cleanup_time = if scatter { last_attempt + 5401 } else { 3602 };
    while engine.stats().peers > 0
        && unchanged < 64
        && maintenance_calls < accepted.div_ceil(256) + 4096
    {
        let before = engine.stats().peers;
        let t = std::time::Instant::now();
        engine.maintenance(cleanup_time, 256);
        max_maintenance_us = max_maintenance_us.max(t.elapsed().as_micros());
        let after = engine.stats().peers;
        unchanged = if before == after { unchanged + 1 } else { 0 };
        maintenance_calls += 1;
        if maintenance_calls.is_multiple_of(1000)
            && let Some(rss) = process_rss_kib("VmRSS:")
        {
            sampled_max_rss = Some(sampled_max_rss.unwrap_or(0).max(rss));
        }
    }
    if let Some(rss) = process_rss_kib("VmRSS:") {
        sampled_max_rss = Some(sampled_max_rss.unwrap_or(0).max(rss));
    }
    Ok(StateBenchReport {
        kind: "state_benchmark",
        state_workload: if scatter { "scatter" } else { "baseline" },
        network_used: false,
        qualification: "in-process logical-time admission; not end-to-end tracker throughput or an S1 capacity guarantee",
        logical_admission_start_seconds: 1,
        logical_admission_end_seconds: admission_end,
        logical_last_attempt_seconds: last_attempt,
        logical_delayed_observation_origin_seconds: last_attempt,
        logical_cleanup_seconds: cleanup_time,
        requested_peers: args.state_count,
        requested_swarms,
        memory_budget_mib: args.state_memory_mib,
        accepted,
        stop_reason,
        elapsed_seconds: elapsed,
        in_process_admissions_per_second: accepted as f64 / elapsed,
        sample_every_n_admissions: 100,
        sampled_p95_ms_upper_bound: h.percentile_ms(95, 100),
        sampled_p99_ms_upper_bound: h.percentile_ms(99, 100),
        histogram_note: "1 ms buckets; 60000 means >= 60 s",
        engine_stats_before_cleanup: at_capacity,
        rss_before_cleanup_kib: resident_before_cleanup,
        peak_rss_kib: peak_resident,
        rss_sample_every_operations: 1000,
        sampled_max_rss_kib: sampled_max_rss,
        admission_phase_vmhwm_kib: peak_resident,
        full_run_vmhwm_kib: process_rss_kib("VmHWM:"),
        rss_after_cleanup_kib: process_rss_kib("VmRSS:"),
        logical_delayed_peer_visible_600s: delayed[0],
        logical_delayed_peer_visible_1800s: delayed[1],
        maintenance_calls,
        max_maintenance_us,
        engine_stats_after_cleanup: engine.stats(),
    })
}

// The count is cumulative admissions, not simultaneously resident peers. Each
// synthetic endpoint is local to this process and is recreated deterministically
// for update/stop, never sent over the network.
fn state_churn_data(args: &Args) -> Result<serde_json::Value> {
    use bt_protocol::{AnnounceEvent, AnnounceRequest};
    use swarm_engine::{EngineConfig, SwarmEngine};

    const SIZES: [usize; 7] = [2, 3, 4, 10, 100, 1000, 4000];
    fn request(seed: &[u8; 16], swarm: usize, peer: usize) -> AnnounceRequest {
        let ip = if peer.is_multiple_of(2) {
            IpAddr::V4(Ipv4Addr::new(10, (peer / 256) as u8, peer as u8, 1))
        } else {
            IpAddr::V6(Ipv6Addr::new(
                0xfd00,
                0,
                0,
                0,
                0,
                0,
                (peer / 65536) as u16,
                peer as u16,
            ))
        };
        AnnounceRequest {
            info_hash: hash_from(seed, swarm as u64, 0, 0),
            peer_id: hash_from(seed, peer as u64, 1, 0),
            ip,
            port: 20_000,
            uploaded: 0,
            downloaded: 0,
            left: if peer.is_multiple_of(3) { 0 } else { 1 },
            event: AnnounceEvent::Started,
            numwant: 0,
        }
    }
    fn sample(operations: u64, max: &mut Option<u64>) {
        if operations.is_multiple_of(1000)
            && let Some(rss) = process_rss_kib("VmRSS:")
        {
            *max = Some(max.unwrap_or(0).max(rss));
        }
    }
    fn logical_time(operations: u64, count: usize) -> u64 {
        1 + operations * 1800 / (count as u64 * 3)
    }

    let engine = SwarmEngine::new(EngineConfig {
        max_peers: args.state_count.saturating_add(2),
        max_swarms: (args.unique_hashes as usize).saturating_add(1),
        max_peers_per_swarm: 4000,
        memory_budget_bytes: args.state_memory_mib * 1024 * 1024,
        allow_private_ips: true,
        ..EngineConfig::default()
    });
    let seed = rand::random::<[u8; 16]>();
    let started = std::time::Instant::now();
    let mut accepted = 0_usize;
    let mut updates = 0_usize;
    let mut stopped = 0_usize;
    let mut finished_swarms = 0_usize;
    let mut operations = 0_u64;
    let mut sampled_max_rss = process_rss_kib("VmRSS:");
    let mut admission_hwm = process_rss_kib("VmHWM:");
    let mut stop_reason = None;
    let mut sizes_completed = [0_usize; 7];
    'swarms: for swarm in 0..args.unique_hashes as usize {
        if accepted == args.state_count {
            break;
        }
        let size_index = swarm % SIZES.len();
        let wanted = SIZES[size_index].min(args.state_count - accepted);
        for peer in 0..wanted {
            let result = engine.announce(
                &request(&seed, swarm, peer),
                logical_time(operations, args.state_count),
            );
            operations += 1;
            sample(operations, &mut sampled_max_rss);
            match result {
                Ok(_) => accepted += 1,
                Err(error) => {
                    stop_reason = Some(format!("admission: {error}"));
                    admission_hwm = process_rss_kib("VmHWM:");
                    break 'swarms;
                }
            }
        }
        admission_hwm = process_rss_kib("VmHWM:");
        if wanted == SIZES[size_index] {
            sizes_completed[size_index] += 1;
        }
        for peer in 0..wanted {
            if peer.is_multiple_of(3) {
                let mut req = request(&seed, swarm, peer);
                req.event = AnnounceEvent::None;
                req.left = u64::from(req.left == 0);
                let result = engine.announce(&req, logical_time(operations, args.state_count));
                operations += 1;
                sample(operations, &mut sampled_max_rss);
                match result {
                    Ok(_) => updates += 1,
                    Err(error) => {
                        stop_reason = Some(format!("update: {error}"));
                        break 'swarms;
                    }
                }
            }
        }
        for peer in 1..wanted {
            let mut req = request(&seed, swarm, peer);
            req.event = AnnounceEvent::Stopped;
            let result = engine.announce(&req, logical_time(operations, args.state_count));
            operations += 1;
            sample(operations, &mut sampled_max_rss);
            match result {
                Ok(_) => stopped += 1,
                Err(error) => {
                    stop_reason = Some(format!("stop: {error}"));
                    break 'swarms;
                }
            }
        }
        finished_swarms += 1;
    }
    let logical_now = logical_time(operations, args.state_count);
    let before = engine.stats();
    let rss_before_cleanup = process_rss_kib("VmRSS:");
    let mut maintenance_calls = 0_usize;
    let mut max_maintenance_us = 0_u128;
    let mut unchanged = 0_usize;
    while engine.stats().peers > 0
        && unchanged < 64
        && maintenance_calls < accepted.div_ceil(256) + 4096
    {
        let previous = engine.stats().peers;
        let t = std::time::Instant::now();
        engine.maintenance(logical_now + 5401, 256);
        max_maintenance_us = max_maintenance_us.max(t.elapsed().as_micros());
        unchanged = if engine.stats().peers == previous {
            unchanged + 1
        } else {
            0
        };
        maintenance_calls += 1;
        sample(maintenance_calls as u64 * 1000, &mut sampled_max_rss);
    }
    Ok(serde_json::json!({
        "kind": "state_benchmark", "state_workload": "churn", "network_used": false,
        "qualification": "synthetic in-process logical time; no network traffic; not real retention or tracker throughput",
        "memory_note": "engine estimated_bytes is accounting, not RSS; allocator retention and VmHWM persist after cleanup",
        "requested_admissions": args.state_count, "requested_swarms_limit": args.unique_hashes,
        "memory_budget_mib": args.state_memory_mib, "accepted": accepted, "updates": updates,
        "stopped": stopped, "finished_swarms": finished_swarms, "stop_reason": stop_reason,
        "full_size_swarms": SIZES.into_iter().zip(sizes_completed).map(|(size, count)| (size.to_string(), count)).collect::<BTreeMap<_, _>>(),
        "logical_end_seconds": logical_now, "logical_clock_formula": "1 + floor(operations * 1800 / (requested_admissions * 3))",
        "operations": operations, "elapsed_seconds": started.elapsed().as_secs_f64(),
        "rss_sample_every_operations": 1000, "sampled_max_rss_kib": sampled_max_rss,
        "admission_phase_vmhwm_kib": admission_hwm, "rss_before_cleanup_kib": rss_before_cleanup,
        "engine_stats_before_cleanup": before, "maintenance_calls": maintenance_calls,
        "max_maintenance_us": max_maintenance_us, "engine_stats_after_cleanup": engine.stats(),
        "rss_after_cleanup_kib": process_rss_kib("VmRSS:"), "full_run_vmhwm_kib": process_rss_kib("VmHWM:")
    }))
}

fn state_bench(args: &Args) -> Result<()> {
    let report = match args.state_workload {
        StateWorkload::Baseline | StateWorkload::Scatter => {
            serde_json::to_value(state_bench_data(args)?)?
        }
        StateWorkload::Churn => state_churn_data(args)?,
    };
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

fn process_rss_kib(field: &str) -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix(field)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    let args = Args::parse();
    args.validate()?;
    if args.state_bench || args.mode == Mode::StateBench {
        return state_bench(&args);
    }
    if args.protocol == Protocol::Mixed && args.concurrency < 2 && args.mode == Mode::Pressure {
        bail!("mixed mode needs at least two workers");
    }
    match args.mode {
        Mode::Pressure => pressure(args).await,
        Mode::Verify => verify(args).await,
        Mode::StateBench => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn larger_udp_pressure_window_remains_bounded_and_explicit() {
        for (protocol, concurrency, accepted) in [
            ("udp", "1024", true),
            ("udp", "4096", true),
            ("udp", "4097", false),
            ("http", "1024", true),
            ("http", "1025", false),
        ] {
            let args = Args::try_parse_from([
                "tracker-loadgen",
                "--mode",
                "pressure",
                "--protocol",
                protocol,
                "--udp-target",
                "example.invalid:6969",
                "--http-url",
                "http://example.invalid/announce",
                "--concurrency",
                concurrency,
                "--i-own-the-target",
            ])
            .unwrap();
            assert_eq!(args.validate().is_ok(), accepted);
        }
    }

    #[test]
    fn refuse_unowned_or_unbounded_requests() {
        let a =
            Args::try_parse_from(["tracker-loadgen", "--udp-target", "127.0.0.1:6969"]).unwrap();
        assert!(a.validate().is_err());
        let a = Args::try_parse_from([
            "tracker-loadgen",
            "--udp-target",
            "127.0.0.1:6969",
            "--i-own-the-target",
            "--max-bytes",
            "19327352833",
        ])
        .unwrap();
        assert!(a.validate().is_err());
        let a = Args::try_parse_from([
            "tracker-loadgen",
            "--state-bench",
            "--state-count",
            "100",
            "--unique-hashes",
            "100",
        ])
        .unwrap();
        assert!(a.validate().is_ok());
        let a = Args::try_parse_from([
            "tracker-loadgen",
            "--mode",
            "verify",
            "--udp-target",
            "127.0.0.1:6969",
            "--i-own-the-target",
        ])
        .unwrap();
        assert!(a.validate().is_err());
        let a = Args::try_parse_from([
            "tracker-loadgen",
            "--mode",
            "verify",
            "--protocol",
            "udp",
            "--udp-target",
            "example.invalid:6969",
            "--family-a",
            "ipv4",
            "--family-b",
            "ipv6",
            "--expected-peer-a-ip",
            "8.8.8.8",
            "--expected-peer-b-ip",
            "2606:4700:4700::1111",
            "--i-own-the-target",
        ])
        .unwrap();
        assert!(a.validate().is_err());
        let a = Args::try_parse_from([
            "tracker-loadgen",
            "--mode",
            "verify",
            "--protocol",
            "https",
            "--http-url",
            "https://example.invalid/announce",
            "--family-a",
            "ipv4",
            "--family-b",
            "ipv6",
            "--expected-peer-a-ip",
            "8.8.8.8",
            "--expected-peer-b-ip",
            "2606:4700:4700::1111",
            "--i-own-the-target",
        ])
        .unwrap();
        assert!(a.validate().is_ok());
        let a = Args::try_parse_from([
            "tracker-loadgen",
            "--mode",
            "verify",
            "--udp-target",
            "127.0.0.1:6969",
            "--expected-client-ip",
            "127.0.0.1",
            "--verify-delay-secs",
            "1801",
            "--i-own-the-target",
        ])
        .unwrap();
        assert!(a.validate().is_err());
    }

    #[test]
    fn binary_query_bytes_are_preserved_and_credentials_rejected() {
        let mut hash = [0_u8; 20];
        hash[0] = b'&';
        hash[1] = b'%';
        let request = Request {
            hash,
            peer_id: [0xff; 20],
            port: 20000,
            event: 2,
            numwant: 0,
            key: 0,
        };
        let url = request_url(
            &Url::parse("http://example.invalid/announce").unwrap(),
            &request,
            Operation::Announce,
        )
        .unwrap();
        assert!(url.query().unwrap().contains("info_hash=%26%25%00"));
        assert!(url.query().unwrap().contains("peer_id=%FF%FF"));
        let args = Args::try_parse_from([
            "tracker-loadgen",
            "--protocol",
            "http",
            "--http-url",
            "http://user:secret@example.invalid/announce",
            "--i-own-the-target",
        ])
        .unwrap();
        assert!(args.validate().is_err());
    }

    #[test]
    fn report_separates_success_latency_failures_and_application_bandwidth() {
        let args = Args::try_parse_from([
            "tracker-loadgen",
            "--protocol",
            "udp",
            "--udp-target",
            "127.0.0.1:6969",
            "--i-own-the-target",
        ])
        .unwrap();
        let shared = Shared::new(32768, 10);
        let empty = report(&shared, &args, "final");
        assert_eq!(empty.completed, 0);
        assert_eq!(empty.protocol_success_rate, None);
        assert_eq!(empty.success_latency_p99_ms_upper_bound, None);

        shared.sent.store(2, Ordering::Relaxed);
        shared.record(&Ok(TrackerReply::default()), Some(200), 12);
        shared.record(&Err(RunError::Timeout), None, 5000);
        assert!(shared.budget.reserve(100));
        shared.budget.settle(100, 30, 40);
        let measured = report(&shared, &args, "final");
        assert_eq!(measured.completed, 2);
        assert_eq!(measured.protocol_success, 1);
        assert_eq!(measured.protocol_success_rate, Some(0.5));
        assert_eq!(measured.timeout, 1);
        assert_eq!(measured.success_latency_p99_ms_upper_bound, Some(13));
        assert_eq!(measured.latency_p99_ms_upper_bound, Some(5001));
        assert_eq!(measured.tx_application_bytes, 30);
        assert_eq!(measured.rx_application_bytes, 40);
        assert!(measured.tx_application_mbps > 0.0);
        assert!(measured.rx_application_mbps > 0.0);
    }

    #[test]
    fn resolution_overrides_require_hostname_matching_family_and_no_credentials() {
        let base = [
            "tracker-loadgen",
            "--protocol",
            "https",
            "--http-url",
            "https://tracker.example.test/announce",
            "--i-own-the-target",
        ];
        let mut valid = base.to_vec();
        valid.extend(["--resolve-ip", "127.0.0.1"]);
        assert!(Args::try_parse_from(valid).unwrap().validate().is_ok());
        let mut wrong_family = base.to_vec();
        wrong_family.extend(["--resolve-ip", "::1"]);
        assert!(
            Args::try_parse_from(wrong_family)
                .unwrap()
                .validate()
                .is_err()
        );
        let literal = Args::try_parse_from([
            "tracker-loadgen",
            "--protocol",
            "http",
            "--http-url",
            "http://127.0.0.1/announce",
            "--resolve-ip",
            "127.0.0.1",
            "--i-own-the-target",
        ])
        .unwrap();
        assert!(literal.validate().is_err());
        let credentials = Args::try_parse_from([
            "tracker-loadgen",
            "--protocol",
            "https",
            "--http-url",
            "https://user:secret@tracker.example.test/announce",
            "--resolve-ip",
            "127.0.0.1",
            "--i-own-the-target",
        ])
        .unwrap();
        assert!(credentials.validate().is_err());
        let udp = Args::try_parse_from([
            "tracker-loadgen",
            "--protocol",
            "udp",
            "--udp-target",
            "127.0.0.1:6969",
            "--resolve-ip",
            "127.0.0.1",
            "--i-own-the-target",
        ])
        .unwrap();
        assert!(udp.validate().is_err());
        let cross = Args::try_parse_from([
            "tracker-loadgen",
            "--mode",
            "verify",
            "--protocol",
            "https",
            "--http-url",
            "https://tracker.example.test/announce",
            "--family-a",
            "ipv4",
            "--family-b",
            "ipv6",
            "--resolve-a-ip",
            "127.0.0.1",
            "--resolve-b-ip",
            "::1",
            "--expected-peer-a-ip",
            "127.0.0.1",
            "--expected-peer-b-ip",
            "::1",
            "--allow-private-test",
            "--i-own-the-target",
        ])
        .unwrap();
        assert!(cross.validate().is_ok());
        let bad_cross = Args::try_parse_from([
            "tracker-loadgen",
            "--mode",
            "verify",
            "--protocol",
            "https",
            "--http-url",
            "https://tracker.example.test/announce",
            "--family-a",
            "ipv4",
            "--family-b",
            "ipv6",
            "--resolve-ip",
            "127.0.0.1",
            "--expected-peer-a-ip",
            "127.0.0.1",
            "--expected-peer-b-ip",
            "::1",
            "--allow-private-test",
            "--i-own-the-target",
        ])
        .unwrap();
        assert!(bad_cross.validate().is_err());
    }

    #[tokio::test]
    async fn resolution_override_preserves_http_host_and_url() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mock = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut chunk = [0_u8; 4096];
            while !received.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = timeout(Duration::from_secs(2), socket.read(&mut chunk))
                    .await
                    .unwrap()
                    .unwrap();
                assert!(n > 0 && received.len() + n <= 8192);
                received.extend_from_slice(&chunk[..n]);
            }
            let text = String::from_utf8_lossy(&received).to_ascii_lowercase();
            assert!(text.contains(&format!("host: tracker.example.test:{}", addr.port())));
            let body = b"d8:intervali1800e5:peers0:e";
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(body).await.unwrap();
        });
        let url = format!("http://tracker.example.test:{}/announce", addr.port());
        let mut transport = make_http(
            &url,
            Family::Ipv4,
            true,
            1000,
            None,
            true,
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        )
        .await
        .unwrap();
        let request = Request {
            hash: [7; 20],
            peer_id: [8; 20],
            port: 20000,
            event: 2,
            numwant: 50,
            key: 1,
        };
        let mut io = IoCounts::default();
        let mut status = None;
        assert!(
            transport
                .execute(&request, Operation::Announce, &mut io, &mut status)
                .await
                .is_ok()
        );
        assert_eq!(status, Some(200));
        mock.await.unwrap();
    }

    #[tokio::test]
    async fn resolution_override_preserves_tls_sni_and_verification() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mock = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let n = timeout(Duration::from_secs(2), socket.read(&mut chunk))
                    .await
                    .unwrap()
                    .unwrap();
                assert!(n > 0 && received.len() + n <= 32768);
                received.extend_from_slice(&chunk[..n]);
                if received
                    .windows(b"tracker.example.test".len())
                    .any(|w| w == b"tracker.example.test")
                {
                    break;
                }
            }
            // Plain HTTP is not valid TLS; the client must fail rather than
            // disable certificate or protocol verification for the override.
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let url = format!("https://tracker.example.test:{}/announce", addr.port());
        let mut transport = make_http(
            &url,
            Family::Ipv4,
            true,
            3000,
            None,
            true,
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        )
        .await
        .unwrap();
        let request = Request {
            hash: [7; 20],
            peer_id: [8; 20],
            port: 20000,
            event: 2,
            numwant: 50,
            key: 1,
        };
        let mut io = IoCounts::default();
        let mut status = None;
        let result = transport
            .execute(&request, Operation::Announce, &mut io, &mut status)
            .await;
        assert!(matches!(result, Err(RunError::Transport)));
        assert_eq!(status, None);
        mock.await.unwrap();
    }

    #[test]
    fn state_benchmark_checks_logical_retention_and_incremental_expiry() {
        let args = Args::try_parse_from([
            "tracker-loadgen",
            "--state-bench",
            "--state-count",
            "16",
            "--unique-hashes",
            "16",
            "--state-memory-mib",
            "64",
        ])
        .unwrap();
        args.validate().unwrap();
        let result = state_bench_data(&args).unwrap();
        assert!(!result.network_used);
        assert_eq!(result.accepted, 16);
        assert!(result.stop_reason.is_none());
        assert_eq!(result.engine_stats_before_cleanup.peers, 16);
        assert_eq!(result.logical_delayed_peer_visible_600s, Some(true));
        assert_eq!(result.logical_delayed_peer_visible_1800s, Some(true));
        assert_eq!(result.engine_stats_after_cleanup.peers, 0);
        assert!(result.qualification.contains("not end-to-end"));
    }

    #[test]
    fn churn_visits_sizes_reuses_slots_and_cleans_up() {
        let args = Args::try_parse_from([
            "tracker-loadgen",
            "--state-bench",
            "--state-workload",
            "churn",
            "--state-count",
            "5119",
            "--unique-hashes",
            "7",
            "--state-memory-mib",
            "64",
        ])
        .unwrap();
        args.validate().unwrap();
        let result = state_churn_data(&args).unwrap();
        assert_eq!(result["accepted"], 5119);
        assert_eq!(result["finished_swarms"], 7);
        assert!(result["stop_reason"].is_null());
        for size in ["2", "3", "4", "10", "100", "1000", "4000"] {
            assert_eq!(result["full_size_swarms"][size], 1);
        }
        assert_eq!(result["engine_stats_before_cleanup"]["peers"], 7);
        assert_eq!(result["engine_stats_after_cleanup"]["peers"], 0);
        assert!(result["logical_end_seconds"].as_u64().unwrap() > 1);
        assert_eq!(result["network_used"], false);
    }

    #[test]
    fn scatter_uses_monotonic_time_and_delays_from_last_admission() {
        let args = Args::try_parse_from([
            "tracker-loadgen",
            "--state-bench",
            "--state-workload",
            "scatter",
            "--state-count",
            "12",
            "--unique-hashes",
            "3",
            "--state-memory-mib",
            "64",
        ])
        .unwrap();
        args.validate().unwrap();
        let report = state_bench_data(&args).unwrap();
        assert_eq!(report.state_workload, "scatter");
        assert_eq!(report.accepted, 12);
        assert_eq!(report.logical_admission_start_seconds, 1);
        assert_eq!(report.logical_admission_end_seconds, 1800);
        assert_eq!(report.logical_delayed_observation_origin_seconds, 1800);
        assert_eq!(report.logical_cleanup_seconds, 7201);
        assert_eq!(report.engine_stats_before_cleanup.peers, 12);
        assert_eq!(report.logical_delayed_peer_visible_600s, Some(true));
        assert_eq!(report.logical_delayed_peer_visible_1800s, Some(true));
        assert_eq!(report.engine_stats_after_cleanup.peers, 0);
    }

    #[tokio::test]
    async fn http_200_business_error_is_not_a_success() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mock = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await.unwrap();
            let body = b"d14:failure reason4:busye";
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(body).await.unwrap();
        });
        let mut transport = make_http(
            &format!("http://{addr}/announce"),
            Family::Ipv4,
            true,
            1000,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        let request = Request {
            hash: [7; 20],
            peer_id: [8; 20],
            port: 20000,
            event: 2,
            numwant: 50,
            key: 1,
        };
        let mut io = IoCounts::default();
        let mut status = None;
        let result = transport
            .execute(&request, Operation::Announce, &mut io, &mut status)
            .await;
        assert!(matches!(
            result,
            Err(RunError::Protocol(ReplyError::TrackerFailure))
        ));
        assert_eq!(status, Some(200));
        assert!(io.tx > 0 && io.rx > 0);
        mock.await.unwrap();
    }

    #[tokio::test]
    async fn http_502_is_recorded_without_waiting_for_huge_error_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mock = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 100000000\r\n\r\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let mut transport = make_http(
            &format!("http://{addr}/announce"),
            Family::Ipv4,
            true,
            3000,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        let request = Request {
            hash: [7; 20],
            peer_id: [8; 20],
            port: 20000,
            event: 2,
            numwant: 50,
            key: 1,
        };
        let mut io = IoCounts::default();
        let mut status = None;
        let result = timeout(
            Duration::from_secs(1),
            transport.execute(&request, Operation::Announce, &mut io, &mut status),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(RunError::HttpStatus(502))));
        assert_eq!(status, Some(502));
        assert_eq!(io.rx, 0);
        mock.abort();
        let _ = mock.await;
    }

    #[tokio::test]
    async fn verify_delay_uses_real_time_and_honours_cancellation() {
        let elapsed = wait_for_verify_delay(Duration::from_millis(30), std::future::pending())
            .await
            .unwrap();
        assert!(elapsed >= Duration::from_millis(30));
        let cancelled =
            wait_for_verify_delay(Duration::from_secs(1), std::future::ready(Ok(()))).await;
        assert!(cancelled.is_err());
    }

    #[tokio::test]
    async fn real_wait_does_not_hide_expired_original_peers_by_reannouncing() {
        use bt_protocol::{
            encode_http_announce, encode_http_failure, encode_http_scrape,
            parse_http_announce as decode_request, parse_http_scrape as decode_scrape,
        };
        use swarm_engine::{EngineConfig, SwarmEngine};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(SwarmEngine::new(EngineConfig {
            allow_private_ips: true,
            normal_ttl_secs: 1,
            cold_seed_ttl_secs: 1,
            ..EngineConfig::default()
        }));
        let state = engine.clone();
        let epoch = std::time::Instant::now();
        let mock = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut data = Vec::new();
                let mut chunk = [0_u8; 4096];
                while data.len() < 8192 && !data.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(n) = socket.read(&mut chunk).await else {
                        break;
                    };
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&chunk[..n]);
                }
                let input = String::from_utf8_lossy(&data);
                let target = input
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("");
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                let now = epoch.elapsed().as_secs();
                let body = match path {
                    "/announce" => decode_request(query.as_bytes(), "127.0.0.1".parse().unwrap())
                        .ok()
                        .and_then(|request| state.announce(&request, now).ok())
                        .map(|reply| encode_http_announce(&reply))
                        .unwrap_or_else(|| encode_http_failure("invalid")),
                    "/scrape" => decode_scrape(query.as_bytes())
                        .ok()
                        .and_then(|hashes| state.scrape(&hashes, now).ok())
                        .map(|reply| encode_http_scrape(&reply))
                        .unwrap_or_else(|| encode_http_failure("invalid")),
                    _ => encode_http_failure("invalid"),
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                if socket.write_all(head.as_bytes()).await.is_ok() {
                    let _ = socket.write_all(&body).await;
                }
            }
        });
        let url = format!("http://{addr}/announce");
        let args = Args::try_parse_from([
            "tracker-loadgen",
            "--mode",
            "verify",
            "--protocol",
            "http",
            "--http-url",
            url.as_str(),
            "--expected-client-ip",
            "127.0.0.1",
            "--allow-private-test",
            "--verify-delay-secs",
            "2",
            "--i-own-the-target",
        ])
        .unwrap();
        args.validate().unwrap();
        let result = timeout(Duration::from_secs(6), verify(args)).await.unwrap();
        assert!(
            result.is_err(),
            "expired registrations must not pass a real wait"
        );
        assert_eq!(
            engine.stats().peers,
            0,
            "observer and synthetic peers must be cleaned up"
        );
        mock.abort();
        let _ = mock.await;
    }

    #[tokio::test]
    async fn verify_rejects_same_port_wrong_ip_and_cleans_up() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mock = tokio::spawn(async move {
            for index in 0..6 {
                let (mut socket, _) = timeout(Duration::from_secs(3), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
                let mut request = [0_u8; 4096];
                let _ = socket.read(&mut request).await.unwrap();
                let body: &[u8] = if index == 1 {
                    b"d8:intervali1800e5:peers6:\x7f\0\0\x02\x4e\x20e"
                } else {
                    b"d8:intervali1800e5:peers0:e"
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket.write_all(head.as_bytes()).await.unwrap();
                socket.write_all(body).await.unwrap();
            }
        });
        let url = format!("http://{addr}/announce");
        let args = Args::try_parse_from([
            "tracker-loadgen",
            "--mode",
            "verify",
            "--protocol",
            "http",
            "--http-url",
            url.as_str(),
            "--expected-client-ip",
            "127.0.0.1",
            "--allow-private-test",
            "--i-own-the-target",
        ])
        .unwrap();
        args.validate().unwrap();
        assert!(verify(args).await.is_err());
        mock.await.unwrap();
    }

    #[tokio::test]
    async fn udp_discards_stale_transaction_and_renews_rejected_cookie() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = server.local_addr().unwrap();
        let mock = tokio::spawn(async move {
            let mut buf = [0_u8; 256];
            for turn in 0..4 {
                let (n, peer) = timeout(Duration::from_secs(3), server.recv_from(&mut buf))
                    .await
                    .unwrap()
                    .unwrap();
                assert!(n >= 16);
                let action = u32::from_be_bytes(buf[8..12].try_into().unwrap());
                let tx = u32::from_be_bytes(buf[12..16].try_into().unwrap());
                if turn == 0 || turn == 2 {
                    assert_eq!(action, 0);
                    if turn == 0 {
                        let mut stale = Vec::new();
                        stale.extend_from_slice(&0_u32.to_be_bytes());
                        stale.extend_from_slice(&tx.wrapping_add(1).to_be_bytes());
                        stale.extend_from_slice(&11_u64.to_be_bytes());
                        server.send_to(&stale, peer).await.unwrap();
                    }
                    let mut response = Vec::new();
                    response.extend_from_slice(&0_u32.to_be_bytes());
                    response.extend_from_slice(&tx.to_be_bytes());
                    response.extend_from_slice(&12_u64.to_be_bytes());
                    server.send_to(&response, peer).await.unwrap();
                } else {
                    assert_eq!(action, 1);
                    let mut response = Vec::new();
                    response
                        .extend_from_slice(&(if turn == 1 { 3_u32 } else { 1_u32 }).to_be_bytes());
                    response.extend_from_slice(&tx.to_be_bytes());
                    if turn == 1 {
                        response.extend_from_slice(b"invalid connection id");
                    } else {
                        response.extend_from_slice(&1800_u32.to_be_bytes());
                        response.extend_from_slice(&0_u32.to_be_bytes());
                        response.extend_from_slice(&0_u32.to_be_bytes());
                    }
                    server.send_to(&response, peer).await.unwrap();
                }
            }
        });
        let mut transport = make_udp(&target.to_string(), Family::Ipv4, None, true)
            .await
            .unwrap();
        let request = Request {
            hash: [1; 20],
            peer_id: [2; 20],
            port: 20000,
            event: 2,
            numwant: 50,
            key: 1,
        };
        let mut io = IoCounts::default();
        let mut status = None;
        let first = transport
            .execute(&request, Operation::Announce, &mut io, &mut status)
            .await;
        assert!(matches!(
            first,
            Err(RunError::Protocol(ReplyError::TrackerFailure))
        ));
        let second = transport
            .execute(&request, Operation::Announce, &mut io, &mut status)
            .await;
        assert!(second.is_ok());
        assert_eq!(io.udp_stale_packets, 1);
        assert_eq!(io.udp_connects, 2);
        assert_eq!(io.udp_connections_ok, 2);
        mock.await.unwrap();
    }

    #[tokio::test]
    async fn udp_timeout_refreshes_stale_cookie_once_and_bounds_connect_failures() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = server.local_addr().unwrap();
        let mock = tokio::spawn(async move {
            let mut buf = [0_u8; 256];
            for turn in 0..4 {
                let (n, peer) = timeout(Duration::from_secs(3), server.recv_from(&mut buf))
                    .await
                    .unwrap()
                    .unwrap();
                assert!(n >= 16);
                let action = u32::from_be_bytes(buf[8..12].try_into().unwrap());
                let tx = u32::from_be_bytes(buf[12..16].try_into().unwrap());
                if turn == 0 || turn == 2 {
                    assert_eq!(action, 0);
                    let mut response = Vec::new();
                    response.extend_from_slice(&0_u32.to_be_bytes());
                    response.extend_from_slice(&tx.to_be_bytes());
                    response.extend_from_slice(&12_u64.to_be_bytes());
                    server.send_to(&response, peer).await.unwrap();
                } else {
                    assert_eq!(action, 1);
                    if turn == 3 {
                        let mut response = Vec::new();
                        response.extend_from_slice(&1_u32.to_be_bytes());
                        response.extend_from_slice(&tx.to_be_bytes());
                        response.extend_from_slice(&1800_u32.to_be_bytes());
                        response.extend_from_slice(&0_u32.to_be_bytes());
                        response.extend_from_slice(&0_u32.to_be_bytes());
                        server.send_to(&response, peer).await.unwrap();
                    }
                }
            }
        });
        let mut transport = make_udp(&target.to_string(), Family::Ipv4, None, true)
            .await
            .unwrap();
        let request = Request {
            hash: [1; 20],
            peer_id: [2; 20],
            port: 20000,
            event: 2,
            numwant: 50,
            key: 1,
        };
        let mut io = IoCounts::default();
        let mut status = None;
        assert!(
            timeout(
                Duration::from_millis(80),
                transport.execute(&request, Operation::Announce, &mut io, &mut status)
            )
            .await
            .is_err()
        );
        transport.note_result(&Err(RunError::Timeout));
        assert!(transport.retry_at().is_none());
        let second = transport
            .execute(&request, Operation::Announce, &mut io, &mut status)
            .await;
        assert!(second.is_ok());
        transport.note_result(&second);
        assert_eq!(io.udp_connects, 2);
        transport.note_result(&Err(RunError::Timeout));
        assert!(matches!(&transport, Transport::Udp(udp) if udp.connection.is_some()));
        if let Transport::Udp(udp) = &mut transport {
            udp.connection = None;
        }
        transport.note_result(&Err(RunError::Timeout));
        assert!(transport.retry_at().is_some_and(|at| at > Instant::now()));
        mock.await.unwrap();
    }
}
