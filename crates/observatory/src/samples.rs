//! Public v1 torrent sample validation. Hashes and metadata are private state.
//!
//! Fetching is intentionally outside this module: callers must use an explicit
//! official-host allowlist, bounded reads and a public-address-pinned client.
use anyhow::{Context, Result, bail, ensure};
use bt_protocol::bencode::{Value, decode};
use sha1::{Digest, Sha1};

const MAX_TORRENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PIECES: usize = 250_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    pub info_hash: [u8; 20],
    pub piece_count: usize,
    pub piece_length: u64,
    pub total_length: u64,
}

// Locate original bytes, not a re-encoded dictionary: a hash is of the bytes
// on the wire. Full structural validation is also performed by `decode`.
fn item_end(input: &[u8], start: usize, depth: usize) -> Result<usize> {
    ensure!(
        depth <= 64 && start < input.len(),
        "invalid bencode depth/input"
    );
    match input[start] {
        b'i' => input[start + 1..]
            .iter()
            .position(|x| *x == b'e')
            .map(|p| start + p + 2)
            .context("unterminated integer"),
        b'l' | b'd' => {
            let mut pos = start + 1;
            while input.get(pos) != Some(&b'e') {
                pos = item_end(input, pos, depth + 1)?;
            }
            Ok(pos + 1)
        }
        b'0'..=b'9' => {
            let colon = input[start..]
                .iter()
                .position(|x| *x == b':')
                .map(|p| start + p)
                .context("unterminated length")?;
            let length: usize = std::str::from_utf8(&input[start..colon])?.parse()?;
            let end = colon
                .checked_add(1)
                .and_then(|p| p.checked_add(length))
                .context("length overflow")?;
            ensure!(end <= input.len(), "truncated string");
            Ok(end)
        }
        _ => bail!("invalid bencode item"),
    }
}

fn info_span(input: &[u8]) -> Result<&[u8]> {
    ensure!(input.first() == Some(&b'd'), "torrent must be dictionary");
    let mut pos = 1;
    let mut info = None;
    while input.get(pos) != Some(&b'e') {
        let key_end = item_end(input, pos, 1)?;
        let key = decode(&input[pos..key_end])?;
        pos = key_end;
        let end = item_end(input, pos, 1)?;
        if key == Value::Bytes(b"info".to_vec()) {
            ensure!(info.is_none(), "duplicate info");
            info = Some(&input[pos..end]);
        }
        pos = end;
    }
    info.context("missing info dictionary")
}

fn positive_int(value: Option<&Value>) -> Option<u64> {
    match value {
        Some(Value::Int(v)) if *v > 0 => Some(*v as u64),
        _ => None,
    }
}

/// Validate a public v1 sample, retaining no title, paths or torrent bytes.
/// Unsupported v2-only, private or malformed torrents are rejected.
pub fn validate_torrent(input: &[u8]) -> Result<Sample> {
    ensure!(
        !input.is_empty() && input.len() <= MAX_TORRENT_BYTES,
        "torrent size limit"
    );
    let top = decode(input)?;
    ensure!(matches!(top, Value::Dict(_)), "torrent must be dictionary");
    let raw_info = info_span(input)?;
    let Value::Dict(info) = decode(raw_info)? else {
        bail!("info must be dictionary")
    };
    match info.get(b"private".as_slice()) {
        None | Some(Value::Int(0)) => (),
        Some(Value::Int(1)) => bail!("private torrent"),
        _ => bail!("invalid private marker"),
    }
    let piece_length =
        positive_int(info.get(b"piece length".as_slice())).context("missing piece length")?;
    let total_length = match (
        info.get(b"length".as_slice()),
        info.get(b"files".as_slice()),
    ) {
        (Some(Value::Int(n)), None) if *n > 0 => *n as u64,
        (None, Some(Value::List(files))) if !files.is_empty() => {
            let mut sum = 0u64;
            for file in files {
                let Value::Dict(file) = file else {
                    bail!("invalid file")
                };
                let Some(Value::Int(length)) = file.get(b"length".as_slice()) else {
                    bail!("invalid file length")
                };
                ensure!(*length >= 0, "invalid file length");
                sum = sum
                    .checked_add(*length as u64)
                    .context("file length overflow")?;
            }
            ensure!(sum > 0, "empty torrent");
            sum
        }
        _ => bail!("invalid file geometry"),
    };
    let Some(Value::Bytes(pieces)) = info.get(b"pieces".as_slice()) else {
        bail!("v1 pieces required")
    };
    ensure!(pieces.len() % 20 == 0, "invalid pieces length");
    let piece_count = pieces.len() / 20;
    ensure!(
        piece_count > 0 && piece_count <= MAX_PIECES,
        "piece count limit"
    );
    ensure!(
        (total_length - 1) / piece_length + 1 == piece_count as u64,
        "piece geometry mismatch"
    );
    let digest = Sha1::digest(raw_info);
    let mut info_hash = [0; 20];
    info_hash.copy_from_slice(&digest);
    Ok(Sample {
        info_hash,
        piece_count,
        piece_length,
        total_length,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hashes_exact_info_and_checks_geometry() {
        let input =
            b"d4:infod6:lengthi1e4:name1:x12:piece lengthi1e6:pieces20:12345678901234567890ee";
        let sample = validate_torrent(input).unwrap();
        let raw = info_span(input).unwrap();
        assert_eq!(sample.info_hash.as_slice(), Sha1::digest(raw).as_slice());
        assert_eq!(sample.piece_count, 1);
    }
    #[test]
    fn rejects_private_and_bad_geometry() {
        let private = b"d4:infod6:lengthi1e4:name1:x12:piece lengthi1e6:pieces20:123456789012345678907:privatei1eee";
        assert!(validate_torrent(private).is_err());
        let bad =
            b"d4:infod6:lengthi2e4:name1:x12:piece lengthi1e6:pieces20:12345678901234567890ee";
        assert!(validate_torrent(bad).is_err());
    }
}
