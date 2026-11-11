//! Bounded, passive BitTorrent peer handshake and bitmap observation.
//! No interested, request, piece or payload messages are ever sent.
use anyhow::{Context, Result, ensure};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout};

const MAX_PIECES: usize = 250_000;
const MAX_MESSAGE: usize = 32 * 1024;
const MAX_MESSAGES: usize = 32;
const MAX_OBSERVATION: Duration = Duration::from_secs(15);
const BITMAP_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct PeerEvidence {
    /// A valid BitTorrent handshake was completed for the supplied info hash.
    pub handshake: bool,
    /// A complete, validated protocol bitmap if received; absence is unknown.
    pub bitmap: Option<Vec<u8>>,
}

/// Conservative routability filter; network admission must also apply any
/// deployment-specific blocked ranges before supplying the socket address.
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0)
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            // Only public global unicast, excluding documentation, transition,
            // benchmarking and special-purpose ranges; mapped v4 is rejected.
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] == 0x0db8 || s[1] <= 0x01ff))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

fn valid_bitmap(bitmap: &[u8], pieces: usize) -> bool {
    if pieces == 0 || pieces > MAX_PIECES || bitmap.len() != pieces.div_ceil(8) {
        return false;
    }
    let extra = (8 - pieces % 8) % 8;
    extra == 0
        || bitmap
            .last()
            .is_some_and(|last| last & ((1u8 << extra) - 1) == 0)
}

/// Union of validated bitmaps, weighted by v1 piece bytes. This is the peers'
/// claim in this sample, not proof that any content can actually be obtained.
/// Returns unknown if no bitmap was observed or the geometry is inconsistent.
pub fn union_coverage(
    bitmaps: &[Vec<u8>],
    pieces: usize,
    piece_length: u64,
    total_length: u64,
) -> Option<f64> {
    if bitmaps.is_empty()
        || pieces == 0
        || pieces > MAX_PIECES
        || piece_length == 0
        || total_length == 0
        || (total_length - 1) / piece_length + 1 != pieces as u64
        || bitmaps.iter().any(|bitmap| !valid_bitmap(bitmap, pieces))
    {
        return None;
    }
    let mut union = vec![0u8; pieces.div_ceil(8)];
    for bitmap in bitmaps {
        for (target, source) in union.iter_mut().zip(bitmap) {
            *target |= *source;
        }
    }
    let mut covered = 0u64;
    for index in 0..pieces {
        if (union[index / 8] & (0x80 >> (index % 8))) != 0 {
            let weight = if index + 1 == pieces {
                (total_length - 1) % piece_length + 1
            } else {
                piece_length
            };
            covered = covered.checked_add(weight)?;
        }
    }
    Some(covered as f64 / total_length as f64)
}

/// Observe a specific public endpoint obtained from a public tracker. `peer_id`
/// must be a fresh, caller-generated 20-byte identifier. The timeout covers
/// connect, handshake and reading; no DNS lookup or inbound listener is used.
pub async fn observe_peer(
    addr: SocketAddr,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
    piece_count: usize,
    duration: Duration,
) -> Result<PeerEvidence> {
    ensure!(
        addr.port() != 0 && public_ip(addr.ip()),
        "non-public peer address"
    );
    ensure!(piece_count <= MAX_PIECES, "piece count limit");
    ensure!(
        !duration.is_zero() && duration <= MAX_OBSERVATION,
        "invalid observation duration"
    );
    let deadline = Instant::now() + duration;
    observe_inner(addr, info_hash, peer_id, piece_count, deadline).await
}

async fn observe_inner(
    addr: SocketAddr,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
    piece_count: usize,
    deadline: Instant,
) -> Result<PeerEvidence> {
    let mut stream = tokio::time::timeout_at(deadline, async {
        let mut stream = TcpStream::connect(addr).await?;
        let mut outgoing = [0u8; 68];
        outgoing[0] = 19;
        outgoing[1..20].copy_from_slice(b"BitTorrent protocol");
        outgoing[28..48].copy_from_slice(&info_hash);
        outgoing[48..68].copy_from_slice(&peer_id);
        stream.write_all(&outgoing).await?;
        let mut incoming = [0u8; 68];
        stream.read_exact(&mut incoming).await?;
        ensure!(
            incoming[0] == 19 && &incoming[1..20] == b"BitTorrent protocol",
            "invalid peer handshake"
        );
        ensure!(
            incoming[28..48] == info_hash && incoming[48..68] != peer_id,
            "peer handshake mismatch"
        );
        Ok::<_, anyhow::Error>(stream)
    })
    .await
    .context("peer observation timeout")??;
    let mut result = PeerEvidence {
        handshake: true,
        bitmap: None,
    };
    // A public magnet can identify a swarm without validated piece geometry.
    // Such a sample may prove a handshake, but cannot prove a bitmap.
    if piece_count == 0 {
        return Ok(result);
    }
    let deadline = deadline.min(Instant::now() + BITMAP_WAIT);
    for _ in 0..MAX_MESSAGES {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut header = [0u8; 4];
        if !matches!(
            timeout(remaining, stream.read_exact(&mut header)).await,
            Ok(Ok(_))
        ) {
            break;
        }
        // A closed or malformed peer must not turn a completed handshake into
        // unverified bitmap evidence; read again only after a complete header.
        let length = u32::from_be_bytes(header) as usize;
        if length == 0 {
            continue;
        }
        ensure!(length <= MAX_MESSAGE, "peer message size limit");
        let mut kind = [0u8; 1];
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, stream.read_exact(&mut kind)).await {
            Ok(Ok(_)) => (),
            Ok(Err(_)) | Err(_) => break,
        }
        // Never intentionally read an unsolicited payload-bearing piece.
        if kind[0] == 7 {
            break;
        }
        let mut payload = vec![0; length - 1];
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, stream.read_exact(&mut payload)).await {
            Ok(Ok(_)) => (),
            Ok(Err(_)) | Err(_) => break,
        }
        if kind[0] == 5 {
            if valid_bitmap(&payload, piece_count) {
                result.bitmap = Some(payload);
            }
            break;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unknown_geometry_observes_handshake_without_bitmap_claim() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut handshake = [0u8; 68];
            socket.read_exact(&mut handshake).await.unwrap();
            handshake[48..68].copy_from_slice(&[7; 20]);
            socket.write_all(&handshake).await.unwrap();
            let mut extra = [0u8; 1];
            assert_eq!(socket.read(&mut extra).await.unwrap(), 0);
        });
        let result = observe_inner(
            addr,
            [3; 20],
            [4; 20],
            0,
            Instant::now() + Duration::from_secs(2),
        )
        .await?;
        assert!(result.handshake);
        assert!(result.bitmap.is_none());
        server.await?;
        Ok(())
    }

    #[tokio::test]
    async fn late_handshake_survives_bitmap_timeout_within_total_budget() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let info_hash = [3; 20];
        let peer_id = [4; 20];
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 68];
            socket.read_exact(&mut request).await.unwrap();
            tokio::time::sleep(Duration::from_millis(250)).await;
            let mut response = request;
            response[48..68].copy_from_slice(&[5; 20]);
            socket.write_all(&response).await.unwrap();
            let mut extra = [0u8; 1];
            // The observer closes after its budget and sends no payload request.
            assert_eq!(socket.read(&mut extra).await.unwrap(), 0);
        });
        let started = Instant::now();
        let budget = Duration::from_millis(500);
        let result = observe_inner(addr, info_hash, peer_id, 1, started + budget).await?;
        let elapsed = started.elapsed();
        assert!(result.handshake);
        assert!(result.bitmap.is_none());
        assert!(elapsed >= Duration::from_millis(400));
        assert!(elapsed <= budget + Duration::from_secs(1));
        server.await?;
        Ok(())
    }

    #[tokio::test]
    async fn handshake_timeout_remains_failure() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let started = Instant::now();
        let budget = Duration::from_millis(150);
        let result = observe_inner(addr, [3; 20], [4; 20], 1, started + budget).await;
        assert!(result.is_err());
        assert!(started.elapsed() <= budget + Duration::from_secs(1));
        server.abort();
        Ok(())
    }

    #[test]
    fn rejects_non_public_peers() {
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "192.0.2.1",
            "100.64.0.1",
            "::1",
            "::ffff:8.8.8.8",
            "2001:db8::1",
        ] {
            assert!(!public_ip(value.parse().unwrap()));
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
    }
    #[test]
    fn validates_piece_bitmap_size_and_padding() {
        assert!(valid_bitmap(&[0xff, 0x80], 9));
        assert!(!valid_bitmap(&[0xff, 0x81], 9));
        assert!(!valid_bitmap(&[0xff], 9));
        assert!(!valid_bitmap(&[], 0));
    }
    #[test]
    fn unions_bitmaps_with_last_piece_weighted() {
        let maps = vec![vec![0b1000_0000], vec![0b0100_0000]];
        assert_eq!(union_coverage(&maps, 2, 4, 5), Some(1.0));
        assert_eq!(union_coverage(&maps[..1], 2, 4, 5), Some(0.8));
        assert_eq!(union_coverage(&[], 2, 4, 5), None);
    }
}
