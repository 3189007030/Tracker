//! Independent, bounded TCP/HTTP health probe. No tracker protocol or peer data.
use std::env;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_HEAD: usize = 8 * 1024;
const MAX_BODY: usize = 16 * 1024;
const CLOSE_REQ: &[u8] = b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
const CLIENT_REQ: &[u8] = b"GET /healthz HTTP/1.1\r\nHost: localhost\r\n\r\n";

type R<T> = Result<T, ()>;
#[derive(Clone, Copy)]
enum Mode {
    Server,
    Client,
}
struct Config {
    target: SocketAddr,
    mode: Mode,
    rate: u64,
    duration: u64,
    threads: usize,
}
#[derive(Default)]
struct Counters {
    next: AtomicU64,
    attempted: AtomicU64,
    completed: AtomicU64,
    http200: AtomicU64,
    connect_timeout: AtomicU64,
    other_timeout: AtomicU64,
    io_error: AtomicU64,
    invalid_reply: AtomicU64,
    bytes: AtomicUsize,
    stop: AtomicBool,
    budget_hit: AtomicBool,
}
#[derive(Clone, Copy)]
enum Error {
    ConnectTimeout,
    OtherTimeout,
    Io,
    Invalid,
    Budget,
}

fn head(data: &[u8]) -> R<(usize, u16, usize)> {
    let split = data.windows(4).position(|x| x == b"\r\n\r\n").ok_or(())?;
    let head_len = split + 4;
    if head_len > MAX_HEAD {
        return Err(());
    }
    let h = std::str::from_utf8(&data[..split]).map_err(|_| ())?;
    let mut lines = h.split("\r\n");
    let mut status = lines.next().ok_or(())?.splitn(3, ' ');
    if !matches!(status.next(), Some("HTTP/1.0" | "HTTP/1.1")) {
        return Err(());
    }
    let code = status.next().ok_or(())?;
    if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(());
    }
    let code = code.parse::<u16>().map_err(|_| ())?;
    if !(200..=599).contains(&code) || status.next().is_none() {
        return Err(());
    }
    let mut length = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(())?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(());
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(());
        }
        if name.eq_ignore_ascii_case("content-length") {
            let value = value.trim();
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(());
            }
            let n = value.parse::<usize>().map_err(|_| ())?;
            if n > MAX_BODY {
                return Err(());
            }
            length = Some(n);
        }
    }
    Ok((head_len, code, length.ok_or(())?))
}
fn remaining(deadline: Instant) -> Result<Duration, Error> {
    let value = deadline
        .checked_duration_since(Instant::now())
        .ok_or(Error::OtherTimeout)?;
    if value.is_zero() {
        Err(Error::OtherTimeout)
    } else {
        Ok(value)
    }
}
fn timed(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}
fn reserve(c: &Counters, wanted: usize) -> Result<usize, Error> {
    let mut old = c.bytes.load(Ordering::Relaxed);
    loop {
        let amount = wanted.min(MAX_BYTES.saturating_sub(old));
        if amount == 0 {
            c.budget_hit.store(true, Ordering::Relaxed);
            c.stop.store(true, Ordering::Relaxed);
            return Err(Error::Budget);
        }
        match c
            .bytes
            .compare_exchange_weak(old, old + amount, Ordering::AcqRel, Ordering::Relaxed)
        {
            Ok(_) => return Ok(amount),
            Err(next) => old = next,
        }
    }
}
fn read_some(
    s: &mut TcpStream,
    out: &mut [u8],
    c: &Counters,
    deadline: Instant,
) -> Result<usize, Error> {
    s.set_read_timeout(Some(remaining(deadline)?))
        .map_err(|_| Error::Io)?;
    let cap = reserve(c, out.len())?;
    let value = s.read(&mut out[..cap]);
    match value {
        Ok(n) => {
            c.bytes.fetch_sub(cap - n, Ordering::AcqRel);
            Ok(n)
        }
        Err(e) => {
            c.bytes.fetch_sub(cap, Ordering::AcqRel);
            Err(if timed(&e) {
                Error::OtherTimeout
            } else {
                Error::Io
            })
        }
    }
}
fn one(cfg: &Config, c: &Counters, end: Instant) -> Result<bool, Error> {
    let deadline = (Instant::now() + Duration::from_secs(5)).min(end);
    let timeout = remaining(deadline).map_err(|_| Error::ConnectTimeout)?;
    let mut s = TcpStream::connect_timeout(&cfg.target, timeout).map_err(|e| {
        if timed(&e) {
            Error::ConnectTimeout
        } else {
            Error::Io
        }
    })?;
    let request = match cfg.mode {
        Mode::Server => CLOSE_REQ,
        Mode::Client => CLIENT_REQ,
    };
    let cap = reserve(c, request.len())?;
    if cap != request.len() {
        c.bytes.fetch_sub(cap, Ordering::AcqRel);
        c.budget_hit.store(true, Ordering::Relaxed);
        c.stop.store(true, Ordering::Relaxed);
        return Err(Error::Budget);
    }
    // Reserve first; release bytes that were never written on every error path.
    let mut rest = request;
    while !rest.is_empty() {
        let write = (|| -> Result<usize, Error> {
            s.set_write_timeout(Some(remaining(deadline)?))
                .map_err(|_| Error::Io)?;
            s.write(rest).map_err(|e| {
                if timed(&e) {
                    Error::OtherTimeout
                } else {
                    Error::Io
                }
            })
        })();
        match write {
            Ok(n) if n > 0 => rest = &rest[n..],
            Ok(_) => {
                c.bytes.fetch_sub(rest.len(), Ordering::AcqRel);
                return Err(Error::Io);
            }
            Err(e) => {
                c.bytes.fetch_sub(rest.len(), Ordering::AcqRel);
                return Err(e);
            }
        }
    }
    let mut data = Vec::with_capacity(1024);
    let (offset, code, length) = loop {
        if data.windows(4).any(|x| x == b"\r\n\r\n") {
            break head(&data).map_err(|_| Error::Invalid)?;
        }
        if data.len() >= MAX_HEAD {
            return Err(Error::Invalid);
        }
        let mut buf = [0u8; 1024];
        let limit = buf.len().min(MAX_HEAD - data.len());
        let n = read_some(&mut s, &mut buf[..limit], c, deadline)?;
        if n == 0 {
            return Err(Error::Invalid);
        }
        data.extend_from_slice(&buf[..n]);
    };
    let total = offset.checked_add(length).ok_or(Error::Invalid)?;
    if data.len() > total {
        return Err(Error::Invalid);
    }
    while data.len() < total {
        let mut buf = [0u8; 1024];
        let limit = buf.len().min(total - data.len());
        let n = read_some(&mut s, &mut buf[..limit], c, deadline)?;
        if n == 0 {
            return Err(Error::Invalid);
        }
        data.extend_from_slice(&buf[..n]);
    }
    match cfg.mode {
        Mode::Server => {
            let mut extra = [0u8; 1];
            if read_some(&mut s, &mut extra, c, deadline)? != 0 {
                return Err(Error::Invalid);
            }
        }
        Mode::Client => {
            s.shutdown(Shutdown::Both).map_err(|_| Error::Io)?;
        }
    }
    Ok(code == 200)
}
fn config() -> R<Option<Config>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args == ["--self-test"] {
        return Ok(None);
    }
    let mut target = None;
    let mut mode = None;
    let mut owned = false;
    let mut rate = 100;
    let mut duration = 120;
    let mut threads = 64;
    let mut i = 0;
    while i < args.len() {
        let key = &args[i];
        i += 1;
        if key == "--i-own-the-target" {
            if owned {
                return Err(());
            }
            owned = true;
            continue;
        }
        if i == args.len() {
            return Err(());
        }
        let value = &args[i];
        i += 1;
        match key.as_str() {
            "--target" if target.is_none() => {
                target = Some(value.parse::<SocketAddr>().map_err(|_| ())?)
            }
            "--closure-mode" if mode.is_none() => {
                mode = Some(match value.as_str() {
                    "server-close" => Mode::Server,
                    "client-close" => Mode::Client,
                    _ => return Err(()),
                })
            }
            "--rate" => rate = value.parse::<u64>().map_err(|_| ())?,
            "--duration" => duration = value.parse::<u64>().map_err(|_| ())?,
            "--threads" => threads = value.parse::<usize>().map_err(|_| ())?,
            _ => return Err(()),
        }
    }
    let target = target.ok_or(())?;
    if !owned
        || target.port() != 18080
        || !(1..=100).contains(&rate)
        || !(1..=120).contains(&duration)
        || !(1..=128).contains(&threads)
    {
        return Err(());
    }
    Ok(Some(Config {
        target,
        mode: mode.ok_or(())?,
        rate,
        duration,
        threads,
    }))
}
fn self_test() -> R<()> {
    let good = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
    if head(good)? != (38, 200, 2) {
        return Err(());
    }
    if head(b"HTTP/1.0 503 Busy\r\nContent-Length: 16384\r\n\r\n")?.1 != 503 {
        return Err(());
    }
    for bad in [
        b"HTTP/1.1 200 OK\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 1\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 16385\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n",
        b"HTTP/1.1 20 OK\r\nContent-Length: 0\r\n\r\n",
        b"HTTP/1.1 100 Continue\r\nContent-Length: 0\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: +1\r\n\r\n",
    ] {
        if head(bad).is_ok() {
            return Err(());
        }
    }
    let mut long = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
    long.extend(std::iter::repeat_n(b'a', MAX_HEAD));
    long.extend(b"\r\nContent-Length: 0\r\n\r\n");
    if head(&long).is_ok() {
        return Err(());
    }
    Ok(())
}
fn run(cfg: Config) -> R<()> {
    let cfg = Arc::new(cfg);
    let c = Arc::new(Counters::default());
    let started = Instant::now();
    let admission_end = started + Duration::from_secs(cfg.duration);
    let hard_end = admission_end + Duration::from_secs(7);
    let total = cfg.rate * cfg.duration;
    let mut workers = Vec::new();
    for _ in 0..cfg.threads {
        let cfg = Arc::clone(&cfg);
        let c = Arc::clone(&c);
        workers.push(thread::spawn(move || {
            loop {
                if c.stop.load(Ordering::Relaxed) {
                    break;
                }
                let elapsed = started.elapsed().as_nanos();
                let current = ((elapsed * u128::from(cfg.rate)) / 1_000_000_000)
                    .min(u128::from(total)) as u64;
                c.next.fetch_max(current, Ordering::Relaxed);
                let index = c.next.fetch_add(1, Ordering::Relaxed);
                if index >= total {
                    break;
                }
                let due = started + Duration::from_nanos(index * 1_000_000_000 / cfg.rate);
                if let Some(delay) = due.checked_duration_since(Instant::now()) {
                    thread::sleep(delay);
                }
                if c.stop.load(Ordering::Relaxed) || Instant::now() >= admission_end {
                    break;
                }
                let interval = Duration::from_nanos(1_000_000_000 / cfg.rate);
                if Instant::now().saturating_duration_since(due) > interval {
                    continue;
                }
                c.attempted.fetch_add(1, Ordering::Relaxed);
                match one(&cfg, &c, hard_end) {
                    Ok(ok) => {
                        c.completed.fetch_add(1, Ordering::Relaxed);
                        if ok {
                            c.http200.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(Error::ConnectTimeout) => {
                        c.connect_timeout.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(Error::OtherTimeout) => {
                        c.other_timeout.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(Error::Invalid) => {
                        c.invalid_reply.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(Error::Io | Error::Budget) => {
                        c.io_error.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }
    let mut healthy = true;
    let join_deadline = admission_end + Duration::from_secs(8);
    for worker in workers {
        while !worker.is_finished() && Instant::now() < join_deadline {
            let wait = join_deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10));
            if !wait.is_zero() {
                thread::sleep(wait);
            }
        }
        if !worker.is_finished() {
            healthy = false;
            c.stop.store(true, Ordering::Relaxed);
        } else if worker.join().is_err() {
            healthy = false;
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    let attempted = c.attempted.load(Ordering::Relaxed);
    let successes = c.http200.load(Ordering::Relaxed);
    let success = if attempted == 0 {
        0.0
    } else {
        successes as f64 / attempted as f64
    };
    let actual = if elapsed == 0.0 {
        0.0
    } else {
        attempted as f64 / elapsed
    };
    println!(
        "{{\"attempted\":{},\"completed\":{},\"http200\":{},\"successrate\":{:.6},\"connect_timeout\":{},\"other_timeout\":{},\"io_error\":{},\"invalid_reply\":{},\"bytes\":{},\"elapsed\":{:.6},\"actualrps\":{:.6},\"closuremode\":\"{}\",\"family\":\"{}\"}}",
        attempted,
        c.completed.load(Ordering::Relaxed),
        successes,
        success,
        c.connect_timeout.load(Ordering::Relaxed),
        c.other_timeout.load(Ordering::Relaxed),
        c.io_error.load(Ordering::Relaxed),
        c.invalid_reply.load(Ordering::Relaxed),
        c.bytes.load(Ordering::Relaxed),
        elapsed,
        actual,
        match cfg.mode {
            Mode::Server => "server-close",
            Mode::Client => "client-close",
        },
        if cfg.target.is_ipv4() { "ipv4" } else { "ipv6" }
    );
    if !healthy || c.budget_hit.load(Ordering::Relaxed) {
        return Err(());
    }
    Ok(())
}
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    match config() {
        Ok(None) => {
            if self_test().is_err() {
                std::process::exit(1);
            }
            println!("{{\"self_test\":true}}");
        }
        Ok(Some(cfg)) => {
            if run(cfg).is_err() {
                std::process::exit(1);
            }
        }
        Err(()) => std::process::exit(2),
    }
}
