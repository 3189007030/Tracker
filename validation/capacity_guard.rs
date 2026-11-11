use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
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

fn decode(input: &[u8]) -> R<B> {
    fn item(x: &[u8], p: &mut usize, depth: usize, nodes: &mut usize) -> R<B> {
        *nodes += 1;
        if depth > 16 || *nodes > 1024 || *p >= x.len() {
            return Err(());
        }
        match x[*p] {
            b'i' => {
                *p += 1;
                let start = *p;
                while *p < x.len() && x[*p] != b'e' {
                    *p += 1;
                }
                if *p == x.len() || start == *p {
                    return Err(());
                }
                let s = std::str::from_utf8(&x[start..*p]).map_err(|_| ())?;
                if s == "-0"
                    || s.starts_with("-0")
                    || (s.len() > 1 && s.starts_with('0'))
                    || s.starts_with('+')
                {
                    return Err(());
                }
                let n = s.parse::<i64>().map_err(|_| ())?;
                *p += 1;
                Ok(B::Int(n))
            }
            b'l' => {
                *p += 1;
                let mut a = Vec::new();
                while *p < x.len() && x[*p] != b'e' {
                    a.push(item(x, p, depth + 1, nodes)?);
                }
                if *p == x.len() {
                    return Err(());
                }
                *p += 1;
                Ok(B::List(a))
            }
            b'd' => {
                *p += 1;
                let mut m = BTreeMap::new();
                let mut previous: Option<Vec<u8>> = None;
                while *p < x.len() && x[*p] != b'e' {
                    let B::Bytes(k) = item(x, p, depth + 1, nodes)? else {
                        return Err(());
                    };
                    if previous.as_ref().is_some_and(|v| v >= &k) {
                        return Err(());
                    }
                    previous = Some(k.clone());
                    let v = item(x, p, depth + 1, nodes)?;
                    m.insert(k, v);
                }
                if *p == x.len() {
                    return Err(());
                }
                *p += 1;
                Ok(B::Dict(m))
            }
            b'0'..=b'9' => {
                let start = *p;
                while *p < x.len() && x[*p].is_ascii_digit() {
                    *p += 1;
                }
                if *p == x.len() || x[*p] != b':' || (*p - start > 1 && x[start] == b'0') {
                    return Err(());
                }
                let n = std::str::from_utf8(&x[start..*p])
                    .map_err(|_| ())?
                    .parse::<usize>()
                    .map_err(|_| ())?;
                *p += 1;
                let end = p.checked_add(n).ok_or(())?;
                if end > x.len() {
                    return Err(());
                }
                let value = x[*p..end].to_vec();
                *p = end;
                Ok(B::Bytes(value))
            }
            _ => Err(()),
        }
    }
    let mut p = 0;
    let mut nodes = 0;
    let v = item(input, &mut p, 0, &mut nodes)?;
    if p != input.len() {
        return Err(());
    }
    Ok(v)
}
fn dict(v: &B) -> R<&BTreeMap<Vec<u8>, B>> {
    if let B::Dict(m) = v { Ok(m) } else { Err(()) }
}
fn integer(v: Option<&B>) -> R<i64> {
    if let Some(B::Int(n)) = v {
        Ok(*n)
    } else {
        Err(())
    }
}
fn bytes(v: Option<&B>) -> R<&[u8]> {
    if let Some(B::Bytes(b)) = v {
        Ok(b)
    } else {
        Err(())
    }
}
fn pct(v: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut s = String::with_capacity(v.len() * 3);
    for b in v {
        s.push('%');
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}
fn peers(v: &B, expected: Option<u16>) -> R<()> {
    let d = dict(v)?;
    if d.contains_key(b"failure reason".as_slice()) || integer(d.get(b"interval".as_slice()))? <= 0
    {
        return Err(());
    }
    let raw = bytes(d.get(b"peers".as_slice()))?;
    if raw.len() % 6 != 0 {
        return Err(());
    }
    if let Some(v6) = d.get(b"peers6".as_slice()) {
        if !bytes(Some(v6))?.is_empty() {
            return Err(());
        }
    }
    let mut seen = BTreeSet::new();
    for p in raw.chunks_exact(6) {
        if p[..4] != [127, 0, 0, 1] {
            return Err(());
        }
        let port = u16::from_be_bytes([p[4], p[5]]);
        if !seen.insert(port) {
            return Err(());
        }
    }
    match expected {
        Some(p) if seen.len() == 1 && seen.contains(&p) => Ok(()),
        None if seen.is_empty() => Ok(()),
        _ => Err(()),
    }
}
fn failure(v: &B) -> R<()> {
    let d = dict(v)?;
    if bytes(d.get(b"failure reason".as_slice()))? != b"memory budget reached" {
        return Err(());
    }
    for k in [
        b"peers".as_slice(),
        b"peers6",
        b"interval",
        b"complete",
        b"incomplete",
    ] {
        if d.contains_key(k) {
            return Err(());
        }
    }
    Ok(())
}
fn counts(v: &B, hash: &[u8; 20], complete: i64, incomplete: i64) -> R<()> {
    let d = dict(v)?;
    if d.contains_key(b"failure reason".as_slice()) {
        return Err(());
    }
    let files = dict(d.get(b"files".as_slice()).ok_or(())?)?;
    if files.len() != 1 {
        return Err(());
    }
    let row = dict(files.get(hash.as_slice()).ok_or(())?)?;
    if integer(row.get(b"complete".as_slice()))? != complete
        || integer(row.get(b"incomplete".as_slice()))? != incomplete
    {
        return Err(());
    }
    Ok(())
}
struct Guard {
    deadline: Instant,
    cleanup_deadline: Instant,
    used: usize,
    hash: [u8; 20],
    other: [u8; 20],
    ids: [[u8; 20]; 2],
}
impl Guard {
    fn remaining(deadline: Instant) -> R<Duration> {
        let t = deadline.checked_duration_since(Instant::now()).ok_or(())?;
        if t.is_zero() { Err(()) } else { Ok(t) }
    }
    fn http(&mut self, target: &str, cleanup: bool) -> R<B> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let deadline = deadline.min(if cleanup {
            self.cleanup_deadline
        } else {
            self.deadline
        });
        let endpoint = SocketAddr::from((Ipv4Addr::LOCALHOST, 18081));
        let mut s =
            TcpStream::connect_timeout(&endpoint, Self::remaining(deadline)?).map_err(|_| ())?;
        let request =
            format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        if self.used.checked_add(request.len()).ok_or(())? > MAX_TOTAL {
            return Err(());
        }
        self.used += request.len();
        let mut rest = request.as_bytes();
        while !rest.is_empty() {
            s.set_write_timeout(Some(Self::remaining(deadline)?))
                .map_err(|_| ())?;
            let n = s.write(rest).map_err(|_| ())?;
            if n == 0 {
                return Err(());
            }
            rest = &rest[n..];
        }
        let mut data = Vec::new();
        loop {
            s.set_read_timeout(Some(Self::remaining(deadline)?))
                .map_err(|_| ())?;
            let mut buf = [0u8; 2048];
            let cap = (MAX_TOTAL - self.used)
                .min(MAX_REPLY - data.len())
                .min(buf.len());
            if cap == 0 {
                return Err(());
            }
            let n = s.read(&mut buf[..cap]).map_err(|_| ())?;
            if n == 0 {
                break;
            }
            self.used += n;
            data.extend_from_slice(&buf[..n]);
        }
        let split = data.windows(4).position(|x| x == b"\r\n\r\n").ok_or(())?;
        let head = std::str::from_utf8(&data[..split]).map_err(|_| ())?;
        let mut lines = head.split("\r\n");
        let status = lines.next().ok_or(())?;
        if !status.starts_with("HTTP/1.1 200 ") && !status.starts_with("HTTP/1.0 200 ") {
            return Err(());
        }
        let mut length = None;
        for line in lines {
            let (name, value) = line.split_once(':').ok_or(())?;
            if name.eq_ignore_ascii_case("transfer-encoding") {
                return Err(());
            }
            if name.eq_ignore_ascii_case("content-length") {
                if length.is_some() {
                    return Err(());
                }
                length = Some(value.trim().parse::<usize>().map_err(|_| ())?);
            }
        }
        let body = &data[split + 4..];
        if length != Some(body.len()) {
            return Err(());
        }
        decode(body)
    }
    fn announce(&mut self, index: usize, event: &str, cleanup: bool) -> R<B> {
        let target = format!(
            "/announce?info_hash={}&peer_id={}&port={}&uploaded=0&downloaded=0&left={}&compact=1&numwant=50&event={}",
            pct(&self.hash),
            pct(&self.ids[index]),
            59000 + index,
            if index == 0 { 1 } else { 0 },
            event
        );
        self.http(&target, cleanup)
    }
    fn scrape(&mut self, complete: i64, incomplete: i64) -> R<()> {
        let target = format!("/scrape?info_hash={}", pct(&self.hash));
        let v = self.http(&target, false)?;
        counts(&v, &self.hash, complete, incomplete)
    }
    fn mutual(&mut self) -> R<()> {
        peers(&self.announce(0, "started", false)?, Some(59001))?;
        peers(&self.announce(1, "started", false)?, Some(59000))
    }
    fn arm(&mut self) -> R<()> {
        peers(&self.announce(0, "started", false)?, None)?;
        peers(&self.announce(1, "started", false)?, Some(59000))?;
        self.mutual()?;
        self.scrape(1, 1)
    }
    fn check(&mut self) -> R<()> {
        self.scrape(1, 1)?;
        self.mutual()?;
        let target = format!(
            "/announce?info_hash={}&peer_id={}&port=59000&uploaded=0&downloaded=0&left=1&compact=1&numwant=50&event=started",
            pct(&self.other),
            pct(&self.ids[0])
        );
        failure(&self.http(&target, false)?)?;
        self.scrape(1, 1)
    }
    fn recover(&mut self) -> R<()> {
        let v = self.announce(0, "stopped", false)?;
        peers(&v, None)?;
        self.scrape(1, 0)?;
        peers(&self.announce(1, "stopped", false)?, None)?;
        self.scrape(0, 0)?;
        peers(&self.announce(0, "started", false)?, None)?;
        self.scrape(0, 1)?;
        peers(&self.announce(0, "stopped", false)?, None)?;
        self.scrape(0, 0)
    }
    fn cleanup(&mut self) {
        let _ = self.announce(0, "stopped", true);
        let _ = self.announce(1, "stopped", true);
        // An unexpectedly successful admission of the probe hash must also be removed.
        let target = format!(
            "/announce?info_hash={}&peer_id={}&port=59000&uploaded=0&downloaded=0&left=1&compact=1&numwant=0&event=stopped",
            pct(&self.other),
            pct(&self.ids[0])
        );
        let _ = self.http(&target, true);
    }
}
fn self_test() -> R<()> {
    for bad in [
        b"i-0e".as_slice(),
        b"i01e",
        b"i+1e",
        b"3:ab",
        b"01:a",
        b"d1:ai1e1:ai2ee",
        b"i1ee",
        b"d1:bi1e1:ai2ee",
    ] {
        if decode(bad).is_ok() {
            return Err(());
        }
    }
    if decode(b"d1:ai1e1:bl1:xe e".as_slice()).is_ok() {
        return Err(());
    }
    if decode(b"d1:ai1e1:bl1:xee").is_err() {
        return Err(());
    }
    let good = decode(b"d8:intervali1800e5:peers6:\x7f\0\0\x01\xe6\x79e")?;
    peers(&good, Some(59001))?;
    if peers(&good, Some(59000)).is_ok() || peers(&good, None).is_ok() {
        return Err(());
    }
    let duplicate = decode(b"d8:intervali1e5:peers12:\x7f\0\0\x01\xe6\x79\x7f\0\0\x01\xe6\x79e")?;
    if peers(&duplicate, Some(59001)).is_ok() {
        return Err(());
    }
    let foreign = decode(b"d8:intervali1e5:peers6:\x7f\0\0\x02\xe6\x79e")?;
    if peers(&foreign, Some(59001)).is_ok() {
        return Err(());
    }
    failure(&decode(b"d14:failure reason21:memory budget reachede")?)?;
    if failure(&decode(
        b"d14:failure reason21:memory budget reached8:intervali1ee",
    )?)
    .is_ok()
    {
        return Err(());
    }
    Ok(())
}
fn send(stage: &'static str, ok: bool) -> R<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "{{\"stage\":\"{stage}\",\"ok\":{ok}}}").map_err(|_| ())?;
    output.flush().map_err(|_| ())
}
fn execute(start: Instant) -> R<()> {
    let mut random = [0u8; 80];
    File::open("/dev/urandom")
        .map_err(|_| ())?
        .read_exact(&mut random)
        .map_err(|_| ())?;
    let hash: [u8; 20] = random[0..20].try_into().map_err(|_| ())?;
    let mut other: [u8; 20] = random[20..40].try_into().map_err(|_| ())?;
    if hash == other {
        other[0] ^= 1;
    }
    let a: [u8; 20] = random[40..60].try_into().map_err(|_| ())?;
    let mut b: [u8; 20] = random[60..80].try_into().map_err(|_| ())?;
    if a == b {
        b[0] ^= 1;
    }
    let mut g = Guard {
        deadline: start + Duration::from_secs(594),
        cleanup_deadline: start + Duration::from_secs(600),
        used: 0,
        hash,
        other,
        ids: [a, b],
    };
    let (tx, rx) = mpsc::sync_channel::<Option<Vec<u8>>>(1);
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        for _ in 0..3 {
            let mut line = Vec::new();
            loop {
                let mut b = [0u8; 1];
                match input.read(&mut b) {
                    Ok(1) if b[0] == b'\n' => break,
                    Ok(1) if line.len() < 16 => line.push(b[0]),
                    _ => {
                        let _ = tx.send(None);
                        return;
                    }
                }
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if tx.send(Some(line)).is_err() {
                return;
            }
        }
    });
    let result = (|| -> R<()> {
        g.arm()?;
        send("armed", true)?;
        let mut checked = false;
        for _ in 0..3 {
            let timeout = Guard::remaining(g.deadline)?;
            let line = match rx.recv_timeout(timeout) {
                Ok(Some(x)) => x,
                Ok(None) | Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(());
                }
            };
            if line == b"check" {
                g.check()?;
                checked = true;
                send("check", true)?;
            } else if line == b"recover" && checked {
                g.recover()?;
                send("recover", true)?;
                return Ok(());
            } else {
                return Err(());
            }
        }
        Err(())
    })();
    if result.is_err() {
        g.cleanup();
    }
    result
}
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let start = Instant::now();
    let args: Vec<String> = env::args().skip(1).collect();
    if args == ["--self-test"] {
        let ok = self_test().is_ok();
        if send("self_test", ok).is_err() {
            std::process::exit(1);
        }
        if !ok {
            std::process::exit(1);
        }
        return;
    }
    if !args.is_empty() {
        let _ = send("arguments", false);
        std::process::exit(2);
    }
    if execute(start).is_err() {
        let _ = send("failed", false);
        std::process::exit(1);
    }
}
