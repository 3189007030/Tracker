//! Standalone, loopback-only tracker wire checks. No project or third-party code.
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

const MAX_TOTAL: usize = 256 * 1024 - 1;
const MAX_REPLY: usize = 16 * 1024;
type R<T> = Result<T, ()>;

#[derive(Debug, PartialEq, Eq)]
enum B {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<B>),
    Dict(BTreeMap<Vec<u8>, B>),
}

fn bdecode(input: &[u8]) -> R<B> {
    fn item(x: &[u8], p: &mut usize, depth: usize) -> R<B> {
        if depth > 16 || *p >= x.len() {
            return Err(());
        }
        match x[*p] {
            b'i' => {
                *p += 1;
                let start = *p;
                while *p < x.len() && x[*p] != b'e' {
                    *p += 1;
                }
                if *p == x.len() || *p == start {
                    return Err(());
                }
                let s = std::str::from_utf8(&x[start..*p]).map_err(|_| ())?;
                if s == "-0" || (s.len() > 1 && s.starts_with('0')) || s.starts_with("-0") {
                    return Err(());
                }
                let n = s.parse().map_err(|_| ())?;
                *p += 1;
                Ok(B::Int(n))
            }
            b'l' => {
                *p += 1;
                let mut out = Vec::new();
                while *p < x.len() && x[*p] != b'e' {
                    if out.len() >= 512 {
                        return Err(());
                    }
                    out.push(item(x, p, depth + 1)?);
                }
                if *p == x.len() {
                    return Err(());
                }
                *p += 1;
                Ok(B::List(out))
            }
            b'd' => {
                *p += 1;
                let mut out = BTreeMap::new();
                while *p < x.len() && x[*p] != b'e' {
                    if out.len() >= 512 {
                        return Err(());
                    }
                    let B::Bytes(k) = item(x, p, depth + 1)? else {
                        return Err(());
                    };
                    let value = item(x, p, depth + 1)?;
                    if out.insert(k, value).is_some() {
                        return Err(());
                    }
                }
                if *p == x.len() {
                    return Err(());
                }
                *p += 1;
                Ok(B::Dict(out))
            }
            b'0'..=b'9' => {
                let start = *p;
                while *p < x.len() && x[*p].is_ascii_digit() {
                    *p += 1;
                }
                if *p == x.len() || x[*p] != b':' || (*p - start > 1 && x[start] == b'0') {
                    return Err(());
                }
                let n: usize = std::str::from_utf8(&x[start..*p])
                    .map_err(|_| ())?
                    .parse()
                    .map_err(|_| ())?;
                *p += 1;
                let end = p.checked_add(n).ok_or(())?;
                if end > x.len() {
                    return Err(());
                }
                let result = x[*p..end].to_vec();
                *p = end;
                Ok(B::Bytes(result))
            }
            _ => Err(()),
        }
    }
    if input.len() > MAX_REPLY {
        return Err(());
    }
    let mut p = 0;
    let result = item(input, &mut p, 0)?;
    if p != input.len() {
        return Err(());
    }
    Ok(result)
}
fn dict(b: &B) -> R<&BTreeMap<Vec<u8>, B>> {
    if let B::Dict(v) = b {
        Ok(v)
    } else {
        Err(())
    }
}
fn int(b: &B) -> R<i64> {
    if let B::Int(v) = b {
        Ok(*v)
    } else {
        Err(())
    }
}
fn bytes(b: &B) -> R<&[u8]> {
    if let B::Bytes(v) = b {
        Ok(v)
    } else {
        Err(())
    }
}

fn self_test() -> R<()> {
    for bad in [
        b"".as_slice(),
        b"i01e",
        b"i-0e",
        b"2:a",
        b"d1:ai1e",
        b"d1:ai1e1:ai2ee",
        b"i1ex",
    ] {
        if bdecode(bad).is_ok() {
            return Err(());
        }
    }
    let mut x = 0x59a4_1902_u32;
    for len in 0..512_usize {
        let mut value = Vec::with_capacity(len);
        for _ in 0..len {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            value.push(x as u8);
        }
        let mut encoded = format!("{}:", len).into_bytes();
        encoded.extend_from_slice(&value);
        if bdecode(&encoded)? != B::Bytes(value) {
            return Err(());
        }
        for cut in 0..encoded.len() {
            if bdecode(&encoded[..cut]).is_ok() {
                return Err(());
            }
        }
    }
    Ok(())
}

struct Check {
    deadline: Instant,
    bytes: usize,
    http: SocketAddr,
    udp: SocketAddr,
    socket: UdpSocket,
}
impl Check {
    fn time(&self) -> R<Duration> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or(())?;
        if remaining.is_zero() {
            Err(())
        } else {
            Ok(remaining.min(Duration::from_millis(800)))
        }
    }
    fn charge(&mut self, n: usize) -> R<()> {
        self.bytes = self.bytes.checked_add(n).ok_or(())?;
        if self.bytes > MAX_TOTAL {
            Err(())
        } else {
            Ok(())
        }
    }
    fn http_get(&mut self, target: &str) -> R<(u16, B)> {
        let mut s = TcpStream::connect_timeout(&self.http, self.time()?).map_err(|_| ())?;
        s.set_read_timeout(Some(self.time()?)).map_err(|_| ())?;
        s.set_write_timeout(Some(self.time()?)).map_err(|_| ())?;
        let req = format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        self.charge(req.len())?;
        s.write_all(req.as_bytes()).map_err(|_| ())?;
        let mut data = Vec::new();
        loop {
            s.set_read_timeout(Some(self.time()?)).map_err(|_| ())?;
            let mut buf = [0_u8; 2048];
            let cap = (MAX_TOTAL - self.bytes).min(buf.len());
            if cap == 0 {
                return Err(());
            }
            let n = s.read(&mut buf[..cap]).map_err(|_| ())?;
            if n == 0 {
                break;
            }
            self.charge(n)?;
            data.extend_from_slice(&buf[..n]);
            if data.len() > MAX_REPLY {
                return Err(());
            }
        }
        let split = data.windows(4).position(|v| v == b"\r\n\r\n").ok_or(())?;
        let head = std::str::from_utf8(&data[..split]).map_err(|_| ())?;
        let status = head
            .lines()
            .next()
            .ok_or(())?
            .split_whitespace()
            .nth(1)
            .ok_or(())?
            .parse()
            .map_err(|_| ())?;
        if head
            .lines()
            .any(|line| line.to_ascii_lowercase().starts_with("transfer-encoding:"))
        {
            return Err(());
        }
        let body = &data[split + 4..];
        let length: usize = head
            .lines()
            .find_map(|line| {
                let (key, val) = line.split_once(':')?;
                if key.eq_ignore_ascii_case("content-length") {
                    val.trim().parse().ok()
                } else {
                    None
                }
            })
            .ok_or(())?;
        if body.len() != length {
            return Err(());
        }
        Ok((status, bdecode(body)?))
    }
    fn send(&mut self, data: &[u8]) -> R<()> {
        self.socket
            .set_write_timeout(Some(self.time()?))
            .map_err(|_| ())?;
        self.charge(data.len())?;
        self.socket.send_to(data, self.udp).map_err(|_| ())?;
        Ok(())
    }
    fn recv(&mut self, timeout_ms: u64) -> R<Option<Vec<u8>>> {
        self.socket
            .set_read_timeout(Some(self.time()?.min(Duration::from_millis(timeout_ms))))
            .map_err(|_| ())?;
        let mut b = [0_u8; 1201];
        let cap = (MAX_TOTAL - self.bytes).min(b.len());
        if cap == 0 {
            return Err(());
        }
        match self.socket.recv_from(&mut b[..cap]) {
            Ok((n, source)) => {
                self.charge(n)?;
                if source != self.udp || n > 1200 {
                    return Err(());
                }
                Ok(Some(b[..n].to_vec()))
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(_) => Err(()),
        }
    }
    fn udp_exchange(&mut self, data: &[u8]) -> R<Vec<u8>> {
        self.send(data)?;
        self.recv(800)?.ok_or(())
    }
    fn silent(&mut self, data: &[u8]) -> R<()> {
        self.send(data)?;
        if self.recv(150)?.is_some() {
            Err(())
        } else {
            Ok(())
        }
    }
}

fn pct(x: &[u8], use_plus: bool) -> String {
    let mut s = String::new();
    for &b in x {
        if use_plus && b == b' ' {
            s.push('+');
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}
fn query(
    hash: &[u8; 20],
    peer: &[u8; 20],
    port: u16,
    event: &str,
    numwant: u32,
    plus: bool,
) -> String {
    format!("/announce?info_hash={}&peer_id={}&port={port}&uploaded=0&downloaded=0&left=1&compact=1&numwant={numwant}&event={event}", pct(hash,plus),pct(peer,false))
}
fn response_peers(value: &B, ip: IpAddr) -> R<BTreeSet<SocketAddr>> {
    let d = dict(value)?;
    if d.contains_key(b"failure reason".as_slice()) {
        return Err(());
    }
    let _ = int(d.get(b"interval".as_slice()).ok_or(())?)?;
    let mut out = BTreeSet::new();
    for (key, size, v6) in [
        (b"peers".as_slice(), 6, false),
        (b"peers6".as_slice(), 18, true),
    ] {
        if let Some(v) = d.get(key) {
            let raw = bytes(v)?;
            if raw.len() % size != 0 {
                return Err(());
            }
            for p in raw.chunks_exact(size) {
                let addr = if v6 {
                    let a: [u8; 16] = p[..16].try_into().map_err(|_| ())?;
                    SocketAddr::new(
                        IpAddr::V6(Ipv6Addr::from(a)),
                        u16::from_be_bytes([p[16], p[17]]),
                    )
                } else {
                    SocketAddr::new(
                        IpAddr::V4(Ipv4Addr::new(p[0], p[1], p[2], p[3])),
                        u16::from_be_bytes([p[4], p[5]]),
                    )
                };
                if !out.insert(addr) {
                    return Err(());
                }
            }
        }
    }
    if !out.iter().all(|p| p.ip() == ip) {
        return Err(());
    }
    Ok(out)
}
fn scrape(c: &mut Check, hash: &[u8; 20]) -> R<i64> {
    let (status, body) = c.http_get(&format!("/scrape?info_hash={}", pct(hash, false)))?;
    if status != 200 {
        return Err(());
    }
    let files = dict(dict(&body)?.get(b"files".as_slice()).ok_or(())?)?;
    let row = dict(files.get(hash.as_slice()).ok_or(())?)?;
    Ok(int(row.get(b"complete".as_slice()).ok_or(())?)?
        + int(row.get(b"incomplete".as_slice()).ok_or(())?)?)
}
fn u32at(x: &[u8], n: usize) -> R<u32> {
    Ok(u32::from_be_bytes(
        x.get(n..n + 4).ok_or(())?.try_into().map_err(|_| ())?,
    ))
}
fn announce(
    cookie: u64,
    tx: u32,
    hash: &[u8; 20],
    peer: &[u8; 20],
    port: u16,
    event: u32,
    want: i32,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(cookie.to_be_bytes());
    out.extend(1_u32.to_be_bytes());
    out.extend(tx.to_be_bytes());
    out.extend(hash);
    out.extend(peer);
    out.extend(0_u64.to_be_bytes());
    out.extend(1_u64.to_be_bytes());
    out.extend(0_u64.to_be_bytes());
    out.extend(event.to_be_bytes());
    out.extend(0_u32.to_be_bytes());
    out.extend(0x1771_2972_u32.to_be_bytes());
    out.extend(want.to_be_bytes());
    out.extend(port.to_be_bytes());
    out
}
fn udp_peers(data: &[u8], tx: u32, ip: IpAddr) -> R<BTreeSet<SocketAddr>> {
    if data.len() < 20 || u32at(data, 0)? != 1 || u32at(data, 4)? != tx {
        return Err(());
    }
    let size = if ip.is_ipv6() { 18 } else { 6 };
    if (data.len() - 20) % size != 0 {
        return Err(());
    }
    let mut peers = BTreeSet::new();
    for p in data[20..].chunks_exact(size) {
        let addr = if ip.is_ipv6() {
            let a: [u8; 16] = p[..16].try_into().map_err(|_| ())?;
            SocketAddr::new(
                IpAddr::V6(Ipv6Addr::from(a)),
                u16::from_be_bytes([p[16], p[17]]),
            )
        } else {
            SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(p[0], p[1], p[2], p[3])),
                u16::from_be_bytes([p[4], p[5]]),
            )
        };
        if addr.ip() != ip || !peers.insert(addr) {
            return Err(());
        }
    }
    Ok(peers)
}
fn stage(name: &str, result: R<()>) -> R<()> {
    let ok = result.is_ok();
    println!("{{\"stage\":\"{name}\",\"ok\":{ok}}}");
    result
}

fn run(c: &mut Check) -> R<()> {
    let mut random = [0_u8; 80];
    File::open("/dev/urandom")
        .map_err(|_| ())?
        .read_exact(&mut random)
        .map_err(|_| ())?;
    let mut hash: [u8; 20] = random[0..20].try_into().map_err(|_| ())?;
    hash[0] = 0;
    hash[1] = 255;
    hash[2] = b' ';
    let mut other: [u8; 20] = random[20..40].try_into().map_err(|_| ())?;
    if hash == other {
        other[0] ^= 1;
    }
    let peer_a: [u8; 20] = random[40..60].try_into().map_err(|_| ())?;
    let peer_b: [u8; 20] = random[60..80].try_into().map_err(|_| ())?;
    let a = 40000 + u16::from_be_bytes([random[6], random[7]]) % 18000;
    let b = a + 1;
    let ip = c.http.ip();
    let endpoint_a = SocketAddr::new(ip, a);
    let endpoint_b = SocketAddr::new(ip, b);
    let mut touched = false;
    let result = (|| -> R<()> {
        stage(
            "empty_scrape",
            if scrape(c, &hash)? == 0 && scrape(c, &other)? == 0 {
                Ok(())
            } else {
                Err(())
            },
        )?;
        // BEP 15 connection magic and transaction ID are written as literal bytes.
        let connect: [u8; 16] = [
            0, 0, 4, 0x17, 0x27, 0x10, 0x19, 0x80, 0, 0, 0, 0, 0x12, 0x34, 0x56, 0x78,
        ];
        c.silent(&connect[..15])?;
        let response = c.udp_exchange(&connect)?;
        let cookie = if response.len() == 16
            && u32at(&response, 0)? == 0
            && u32at(&response, 4)? == 0x1234_5678
        {
            u64::from_be_bytes(response[8..16].try_into().map_err(|_| ())?)
        } else {
            return stage("udp_connect", Err(()));
        };
        stage("udp_connect", Ok(()))?;
        let packet = announce(cookie, 0x11aa_0001, &hash, &peer_b, b, 2, 50);
        if packet.len() != 98 {
            return stage("length_boundaries", Err(()));
        }
        c.silent(&packet[..97])?;
        let mut malformed_scrape = Vec::new();
        malformed_scrape.extend(cookie.to_be_bytes());
        malformed_scrape.extend(2_u32.to_be_bytes());
        malformed_scrape.extend(0x22aa_0001_u32.to_be_bytes());
        malformed_scrape.extend(&hash[..19]);
        c.silent(&malformed_scrape)?;
        let forged = announce(cookie ^ 1, 0x11aa_0002, &hash, &peer_b, b, 2, 50);
        c.silent(&forged)?;
        stage(
            "udp_short_and_forged",
            if scrape(c, &hash)? == 0 {
                Ok(())
            } else {
                Err(())
            },
        )?;
        let mut scrape_packet = Vec::new();
        scrape_packet.extend(cookie.to_be_bytes());
        scrape_packet.extend(2_u32.to_be_bytes());
        scrape_packet.extend(0x22aa_0002_u32.to_be_bytes());
        c.silent(&scrape_packet)?;
        scrape_packet.extend(hash);
        let scraped = c.udp_exchange(&scrape_packet)?;
        if scraped.len() != 20
            || u32at(&scraped, 0)? != 2
            || u32at(&scraped, 4)? != 0x22aa_0002
            || u32at(&scraped, 8)? != 0
            || u32at(&scraped, 16)? != 0
        {
            return stage("udp_scrape_lengths", Err(()));
        }
        for _ in 1..64 {
            scrape_packet.extend(hash);
        }
        let scraped = c.udp_exchange(&scrape_packet)?;
        if scraped.len() != 8 + 64 * 12
            || u32at(&scraped, 0)? != 2
            || u32at(&scraped, 4)? != 0x22aa_0002
        {
            return stage("udp_scrape_lengths", Err(()));
        }
        scrape_packet.extend(hash);
        c.silent(&scrape_packet)?;
        stage("udp_scrape_lengths", Ok(()))?;

        for len in [19_usize, 21] {
            let h = vec![b'a'; len];
            let q = format!(
                "/announce?info_hash={}&peer_id={}&port={a}&uploaded=0&downloaded=0&left=1",
                pct(&h, false),
                pct(&peer_a, false)
            );
            let (status, body) = c.http_get(&q)?;
            if status != 200 || !dict(&body)?.contains_key(b"failure reason".as_slice()) {
                return stage("http_failure_and_lengths", Err(()));
            }
        }
        stage("http_failure_and_lengths", Ok(()))?;
        for count in [0_usize, 64, 65] {
            let q = format!(
                "/scrape?{}",
                (0..count)
                    .map(|_| format!("info_hash={}", pct(&hash, false)))
                    .collect::<Vec<_>>()
                    .join("&")
            );
            let (status, body) = c.http_get(&q)?;
            let failed = dict(&body)?.contains_key(b"failure reason".as_slice());
            if status != 200 || failed != (count != 64) {
                return stage("http_scrape_lengths", Err(()));
            }
        }
        if scrape(c, &hash)? != 0 {
            return stage("http_scrape_lengths", Err(()));
        }
        stage("http_scrape_lengths", Ok(()))?;

        touched = true;
        let (status, body) = c.http_get(&query(&hash, &peer_a, a, "started", 0, true))?;
        if status != 200 || !response_peers(&body, ip)?.is_empty() {
            return stage("binary_query_numwant_zero", Err(()));
        }
        stage("binary_query_numwant_zero", Ok(()))?;
        let response = c.udp_exchange(&packet)?;
        if udp_peers(&response, 0x11aa_0001, ip)? != BTreeSet::from([endpoint_a]) {
            return stage("udp_discovers_http", Err(()));
        }
        stage("udp_discovers_http", Ok(()))?;
        let (status, body) = c.http_get(&query(&hash, &peer_a, a, "", 50, false))?;
        if status != 200 || response_peers(&body, ip)? != BTreeSet::from([endpoint_b]) {
            return stage("http_discovers_udp", Err(()));
        }
        stage("http_discovers_udp", Ok(()))?;
        if scrape(c, &hash)? != 2 || scrape(c, &hash)? != 2 {
            return stage("scrape_no_mutation", Err(()));
        }
        stage("scrape_no_mutation", Ok(()))?;
        let (status, body) = c.http_get(&query(&hash, &peer_a, a, "stopped", 0, false))?;
        if status != 200
            || dict(&body)?.contains_key(b"failure reason".as_slice())
            || scrape(c, &hash)? != 1
        {
            return stage("stopped_two_to_one", Err(()));
        }
        stage("stopped_two_to_one", Ok(()))?;
        let stopped = announce(cookie, 0x11aa_0003, &hash, &peer_b, b, 3, 0);
        let response = c.udp_exchange(&stopped)?;
        if !udp_peers(&response, 0x11aa_0003, ip)?.is_empty() || scrape(c, &hash)? != 0 {
            return stage("stopped_one_to_zero", Err(()));
        }
        stage("stopped_one_to_zero", Ok(()))?;
        let (status, body) = c.http_get(&query(&other, &peer_a, a, "started", 50, false))?;
        if status != 200 || !response_peers(&body, ip)?.is_empty() {
            return stage("unrelated_hash_empty", Err(()));
        }
        stage("unrelated_hash_empty", Ok(()))?;
        let (status, body) = c.http_get(&query(&other, &peer_a, a, "stopped", 0, false))?;
        if status != 200
            || dict(&body)?.contains_key(b"failure reason".as_slice())
            || scrape(c, &other)? != 0
        {
            return stage("cleanup", Err(()));
        }
        touched = false;
        stage("cleanup", Ok(()))?;
        Ok(())
    })();
    if touched {
        // Best effort removal through the shared engine, even if a reply was lost.
        for (h, p, port) in [
            (&hash, &peer_a, a),
            (&hash, &peer_b, b),
            (&other, &peer_a, a),
        ] {
            let _ = c.http_get(&query(h, p, port, "stopped", 0, false));
        }
    }
    result
}

fn main() {
    let start = Instant::now();
    if stage("offline_parser", self_test()).is_err() {
        std::process::exit(1);
    }
    let args: Vec<String> = env::args().skip(1).collect();
    if args == ["--self-test"] {
        println!("{{\"ok\":true,\"network_used\":false}}");
        return;
    }
    if args.len() != 4 || args[0] != "--http" || args[2] != "--udp" {
        println!("{{\"stage\":\"arguments\",\"ok\":false}}");
        std::process::exit(2);
    }
    let pair = (args[1].parse::<SocketAddr>(), args[3].parse::<SocketAddr>());
    let (http, udp) = match pair {
        (Ok(h), Ok(u))
            if (h.port() == 18081 || h.port() == 18080)
                && u.port() == 16969
                && h.ip() == u.ip()
                && (h.ip() == IpAddr::V4(Ipv4Addr::LOCALHOST)
                    || h.ip() == IpAddr::V6(Ipv6Addr::LOCALHOST)) =>
        {
            (h, u)
        }
        _ => {
            println!("{{\"stage\":\"arguments\",\"ok\":false}}");
            std::process::exit(2);
        }
    };
    let source = SocketAddr::new(http.ip(), 0);
    let socket = match UdpSocket::bind(source) {
        Ok(s) => s,
        Err(_) => {
            println!("{{\"stage\":\"socket\",\"ok\":false}}");
            std::process::exit(1);
        }
    };
    let mut c = Check {
        deadline: start + Duration::from_secs(29),
        bytes: 0,
        http,
        udp,
        socket,
    };
    let ok = run(&mut c).is_ok();
    println!(
        "{{\"stage\":\"final\",\"ok\":{ok},\"application_bytes\":{},\"elapsed_ms\":{}}}",
        c.bytes,
        start.elapsed().as_millis()
    );
    if !ok {
        std::process::exit(1);
    }
}
