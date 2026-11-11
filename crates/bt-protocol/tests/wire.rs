use bt_protocol::{AnnounceEvent, AnnounceRequest, AnnounceResponse, ScrapeEntry, UdpRequest};
use bt_protocol::{
    bencode, decode_udp_announce_response, decode_udp_connect_response, encode_http_announce,
    encode_http_announce_query, encode_http_failure, encode_http_scrape, encode_udp_announce,
    encode_udp_announce_request, encode_udp_connect, encode_udp_connect_request, encode_udp_error,
    encode_udp_scrape, parse_http_announce, parse_http_scrape, parse_udp, udp_cookie,
    validate_udp_cookie,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

fn request() -> AnnounceRequest {
    AnnounceRequest {
        info_hash: *b"abc\x00\xff+&=0123456789!!",
        peer_id: *b"-TEST01-123456789012",
        ip: IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
        port: 6881,
        uploaded: 2,
        downloaded: 3,
        left: 4,
        event: AnnounceEvent::Started,
        numwant: 0,
    }
}

#[test]
fn http_binary_query_round_trips_and_ignores_claimed_ip() {
    let req = request();
    let mut encoded = encode_http_announce_query(&req);
    assert!(encoded.contains("%00%FF%2B%26%3D"));
    encoded.push_str("&ip=10.0.0.1");
    assert_eq!(
        parse_http_announce(encoded.as_bytes(), req.ip).unwrap(),
        req
    );
}

#[test]
fn http_rejects_ambiguous_bad_and_oversized_inputs() {
    let req = request();
    let encoded = encode_http_announce_query(&req);
    for suffix in ["&port=9", "&event=evil", "&bad=%X0", "&other=%"] {
        assert!(parse_http_announce(format!("{encoded}{suffix}").as_bytes(), req.ip).is_err());
    }
    assert!(parse_http_announce(&vec![b'a'; 8193], req.ip).is_err());
    assert!(parse_http_announce(b"info_hash=abc", req.ip).is_err());
    let missing = encoded.replace("&left=4", "");
    assert!(parse_http_announce(missing.as_bytes(), req.ip).is_err());
}

#[test]
fn numwant_zero_default_and_ceiling_are_preserved() {
    let req = request();
    let raw = encode_http_announce_query(&req);
    assert_eq!(
        parse_http_announce(raw.as_bytes(), req.ip).unwrap().numwant,
        0
    );
    assert_eq!(
        parse_http_announce(raw.replace("numwant=0", "numwant=-1").as_bytes(), req.ip)
            .unwrap()
            .numwant,
        50
    );
    assert_eq!(
        parse_http_announce(raw.replace("numwant=0", "numwant=5000").as_bytes(), req.ip)
            .unwrap()
            .numwant,
        100
    );
    let source = SocketAddr::new(req.ip, 49152);
    let mut udp = encode_udp_announce_request(1, 2, &req);
    assert!(matches!(
        parse_udp(&udp, source).unwrap(),
        UdpRequest::Announce {
            request: AnnounceRequest { numwant: 0, .. },
            ..
        }
    ));
    udp[92..96].copy_from_slice(&(-1i32).to_be_bytes());
    assert!(matches!(
        parse_udp(&udp, source).unwrap(),
        UdpRequest::Announce {
            request: AnnounceRequest { numwant: 50, .. },
            ..
        }
    ));
    udp[92..96].copy_from_slice(&(-2i32).to_be_bytes());
    assert!(parse_udp(&udp, source).is_err());
}

#[test]
fn scrape_has_a_strict_64_hash_limit() {
    let encoded = "info_hash=12345678901234567890";
    assert_eq!(
        parse_http_scrape(vec![encoded; 64].join("&").as_bytes())
            .unwrap()
            .len(),
        64
    );
    assert!(parse_http_scrape(vec![encoded; 65].join("&").as_bytes()).is_err());
    assert!(parse_http_scrape(b"info_hash=short").is_err());
    assert!(parse_http_scrape(b"").is_err());
}

#[test]
fn http_responses_are_valid_bencode_and_compact_dual_stack() {
    let r = AnnounceResponse {
        interval: 1800,
        min_interval: 300,
        complete: 1,
        incomplete: 2,
        peers: vec![
            "8.8.8.8:6881".parse().unwrap(),
            "[2001:4860:4860::8888]:6882".parse().unwrap(),
        ],
    };
    let bencode::Value::Dict(map) = bencode::decode(&encode_http_announce(&r)).unwrap() else {
        panic!()
    };
    assert_eq!(
        map[b"peers".as_slice()],
        bencode::Value::Bytes(vec![8, 8, 8, 8, 0x1a, 0xe1])
    );
    let bencode::Value::Bytes(v6) = &map[b"peers6".as_slice()] else {
        panic!()
    };
    assert_eq!(v6.len(), 18);
    assert_eq!(map[b"interval".as_slice()], bencode::Value::Int(1800));
    let bencode::Value::Dict(failure) = bencode::decode(&encode_http_failure("full")).unwrap()
    else {
        panic!()
    };
    assert_eq!(
        failure[b"failure reason".as_slice()],
        bencode::Value::Bytes(b"full".to_vec())
    );
    assert!(!failure.contains_key(b"interval".as_slice()));
    assert!(!failure.contains_key(b"peers".as_slice()));
    let item = ScrapeEntry {
        info_hash: [1; 20],
        complete: 3,
        downloaded: 4,
        incomplete: 5,
    };
    let bencode::Value::Dict(root) = bencode::decode(&encode_http_scrape(&[item])).unwrap() else {
        panic!()
    };
    let bencode::Value::Dict(files) = &root[b"files".as_slice()] else {
        panic!()
    };
    let bencode::Value::Dict(counts) = &files[&[1u8; 20][..]] else {
        panic!()
    };
    assert_eq!(counts[b"downloaded".as_slice()], bencode::Value::Int(4));
}

#[test]
fn http_encoder_never_publishes_more_than_100_total_candidates() {
    let v4: SocketAddr = "8.8.8.8:6881".parse().unwrap();
    let v6: SocketAddr = "[2001:4860:4860::8888]:6881".parse().unwrap();
    let mut r = AnnounceResponse::default();
    r.peers.extend(vec![v4; 60]);
    r.peers.extend(vec![v6; 60]);
    let bencode::Value::Dict(map) = bencode::decode(&encode_http_announce(&r)).unwrap() else {
        panic!()
    };
    let bencode::Value::Bytes(p4) = &map[b"peers".as_slice()] else {
        panic!()
    };
    let bencode::Value::Bytes(p6) = &map[b"peers6".as_slice()] else {
        panic!()
    };
    assert_eq!(p4.len() / 6 + p6.len() / 18, 100);
}

#[test]
fn udp_connect_and_announce_round_trip_v4_and_v6() {
    let req = request();
    let source: SocketAddr = "8.8.8.8:49152".parse().unwrap();
    let connect = encode_udp_connect_request(123);
    assert_eq!(
        parse_udp(&connect, source).unwrap(),
        UdpRequest::Connect {
            transaction_id: 123
        }
    );
    assert_eq!(
        decode_udp_connect_response(&encode_udp_connect(123, 456), 123).unwrap(),
        456
    );
    assert!(decode_udp_connect_response(&encode_udp_connect(123, 456), 124).is_err());
    let packet = encode_udp_announce_request(456, 123, &req);
    assert_eq!(packet.len(), 98);
    assert_eq!(
        parse_udp(&packet, source).unwrap(),
        UdpRequest::Announce {
            connection_id: 456,
            transaction_id: 123,
            request: req
        }
    );
    let r = AnnounceResponse {
        interval: 1800,
        min_interval: 300,
        complete: 1,
        incomplete: 2,
        peers: vec![
            "8.8.8.8:6881".parse().unwrap(),
            "[2001:4860:4860::8888]:6882".parse().unwrap(),
        ],
    };
    for family in [false, true] {
        let packet = encode_udp_announce(123, &r, family);
        let decoded = decode_udp_announce_response(&packet, 123, family).unwrap();
        assert_eq!(decoded.peers.len(), 1);
        assert_eq!(decoded.peers[0].is_ipv6(), family);
        assert_eq!(decoded.min_interval, 0);
        assert!(decode_udp_announce_response(&packet, 124, family).is_err());
    }
}

#[test]
fn udp_scrape_and_all_response_sizes_are_bounded() {
    let source: SocketAddr = "8.8.8.8:20000".parse().unwrap();
    let mut packet = Vec::new();
    packet.extend(123u64.to_be_bytes());
    packet.extend(2u32.to_be_bytes());
    packet.extend(99u32.to_be_bytes());
    packet.extend(vec![7u8; 20 * 64]);
    let UdpRequest::Scrape {
        hashes,
        transaction_id,
        ..
    } = parse_udp(&packet, source).unwrap()
    else {
        panic!()
    };
    assert_eq!(hashes.len(), 64);
    assert_eq!(transaction_id, 99);
    packet.extend([0u8; 20]);
    assert!(parse_udp(&packet, source).is_err());
    let entries = vec![ScrapeEntry::default(); 100];
    assert_eq!(encode_udp_scrape(1, &entries).len(), 8 + 64 * 12);
    assert_eq!(encode_udp_error(1, &"x".repeat(5000)).len(), 1200);
    let r = AnnounceResponse {
        peers: vec!["[2001:4860:4860::8888]:6881".parse().unwrap(); 500],
        ..Default::default()
    };
    assert_eq!(encode_udp_announce(1, &r, true).len(), 20 + 50 * 18);
}

#[test]
fn cookies_expire_bind_ip_and_never_revive_on_slot_wrap() {
    let ip: IpAddr = "8.8.8.8".parse().unwrap();
    let other: IpAddr = "1.1.1.1".parse().unwrap();
    let secret = b"32-byte-test-secret-1234567890123";
    let id = udp_cookie(secret, ip, 119);
    assert!(validate_udp_cookie(secret, ip, 239, id));
    assert!(!validate_udp_cookie(secret, ip, 240, id));
    assert!(!validate_udp_cookie(secret, other, 119, id));
    assert!(!validate_udp_cookie(b"different secret", ip, 119, id));
    assert!(!validate_udp_cookie(secret, ip, 119 + 65536 * 60, id));
    let v6: IpAddr = "2001:4860:4860::8888".parse().unwrap();
    assert!(!validate_udp_cookie(secret, v6, 119, id));
    let v6_id = udp_cookie(secret, v6, 120);
    assert!(validate_udp_cookie(secret, v6, 240, v6_id));
    assert!(!validate_udp_cookie(secret, v6, 300, v6_id));
}

#[test]
fn bencode_is_bounded_and_handles_torrent_sized_bytes() {
    let val = bencode::Value::Bytes(vec![42; 4 * 1024 * 1024]);
    assert_eq!(bencode::decode(&bencode::encode(&val)).unwrap(), val);
    for raw in [
        b"i-0e".as_slice(),
        b"i01e",
        b"i1ee",
        b"03:abc",
        b"d1:ai1e1:ai2ee",
        b"l",
        b"i999999999999999999999999999999e",
    ] {
        assert!(bencode::decode(raw).is_err(), "accepted {raw:?}");
    }
    assert!(bencode::decode(&vec![b'x'; bencode::MAX_INPUT_BYTES + 1]).is_err());
    let mut deep = vec![b'l'; bencode::MAX_DEPTH + 2];
    deep.extend(vec![b'e'; bencode::MAX_DEPTH + 2]);
    assert!(bencode::decode(&deep).is_err());
}

#[test]
fn malformed_packets_do_not_panic() {
    // Deterministic malformed inputs cover parser boundaries and odd layouts.
    let source: SocketAddr = "[2001:4860:4860::8888]:49152".parse().unwrap();
    let mut x = 0x12345678u32;
    for len in 0..512 {
        let mut bytes = vec![0u8; len];
        for b in &mut bytes {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8;
        }
        let _ = parse_udp(&bytes, source);
        let _ = parse_http_announce(&bytes, source.ip());
        let _ = bencode::decode(&bytes);
    }
}
