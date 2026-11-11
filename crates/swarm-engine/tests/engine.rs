use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::thread;

use bt_protocol::{AnnounceEvent, AnnounceRequest};
use swarm_engine::{EngineConfig, EngineError, SwarmEngine};

fn request(hash: u8, ip: IpAddr, port: u16, left: u64) -> AnnounceRequest {
    AnnounceRequest {
        info_hash: [hash; 20],
        peer_id: [port as u8; 20],
        ip,
        port,
        uploaded: 0,
        downloaded: 0,
        left,
        event: AnnounceEvent::None,
        numwant: 50,
    }
}

fn v4(n: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
}
fn v6(n: u16) -> IpAddr {
    IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, n))
}
fn engine() -> SwarmEngine {
    SwarmEngine::new(EngineConfig {
        allow_private_ips: true,
        ..EngineConfig::default()
    })
}

#[test]
fn peers_survive_ten_and_thirty_minutes_and_cross_address_families() {
    let engine = engine();
    let a = request(1, v4(1), 1001, 0);
    let b = request(1, v6(2), 1002, 20);
    engine.announce(&a, 0).unwrap();
    let reply = engine.announce(&b, 600).unwrap();
    assert!(reply.peers.contains(&SocketAddr::new(a.ip, a.port)));
    assert_eq!((reply.complete, reply.incomplete), (1, 1));
    let reply = engine.announce(&a, 1800).unwrap();
    assert!(reply.peers.contains(&SocketAddr::new(b.ip, b.port)));
    assert_eq!((reply.interval, reply.min_interval), (1800, 300));
}

#[test]
fn expiry_cold_seeds_and_return_to_normal_at_four_peers() {
    let engine = engine();
    let seed = request(2, v4(1), 1001, 0);
    let leecher = request(2, v4(2), 1002, 1);
    engine.announce(&seed, 0).unwrap();
    engine.announce(&leecher, 0).unwrap();
    engine.maintenance(3600, 512);
    assert_eq!(engine.stats().peers, 1);
    assert_eq!(
        engine.scrape(&[seed.info_hash], 3600).unwrap()[0].complete,
        1
    );
    engine.maintenance(5400, 512);
    assert_eq!(engine.stats().peers, 0);
    assert_eq!(engine.stats().swarms, 0);

    for n in 1..=4 {
        engine
            .announce(&request(3, v4(n), 2000 + n as u16, 0), 0)
            .unwrap();
    }
    engine.maintenance(3600, 512);
    // Already expired seeds are not resurrected when the swarm shrinks.
    assert_eq!(engine.stats().peers, 0);
}

#[test]
fn completion_is_idempotent_and_stopped_removes_only_its_endpoint() {
    let engine = engine();
    let a = request(4, v4(1), 1001, 1);
    let mut b = request(4, v4(1), 1002, 1);
    engine.announce(&a, 0).unwrap();
    engine.announce(&b, 0).unwrap();
    b.event = AnnounceEvent::Completed;
    b.left = 0;
    engine.announce(&b, 1).unwrap();
    engine.announce(&b, 2).unwrap();
    let s = &engine.scrape(&[a.info_hash], 2).unwrap()[0];
    assert_eq!((s.complete, s.incomplete, s.downloaded), (1, 1, 1));
    b.event = AnnounceEvent::Stopped;
    let result = engine.announce(&b, 3).unwrap();
    assert_eq!((result.complete, result.incomplete), (0, 1));
    assert_eq!(engine.stats().peers, 1);
    engine.announce(&b, 4).unwrap();
    assert_eq!(engine.stats().peers, 1);
}

#[test]
fn zero_numwant_still_registers_but_never_returns_peers() {
    let engine = engine();
    let a = request(5, v4(1), 1001, 0);
    let mut b = request(5, v4(2), 1002, 1);
    engine.announce(&a, 0).unwrap();
    b.numwant = 0;
    let reply = engine.announce(&b, 1).unwrap();
    assert!(reply.peers.is_empty());
    assert_eq!(engine.stats().peers, 2);
}

#[test]
fn numwant_is_total_budget_and_both_families_are_represented() {
    let engine = engine();
    engine.announce(&request(11, v4(1), 1001, 1), 0).unwrap();
    engine.announce(&request(11, v4(2), 1002, 1), 0).unwrap();
    engine.announce(&request(11, v6(1), 1003, 1), 0).unwrap();
    engine.announce(&request(11, v6(2), 1004, 1), 0).unwrap();
    let mut client = request(11, v4(3), 1005, 1);
    client.numwant = 2;
    let reply = engine.announce(&client, 1).unwrap();
    assert_eq!(reply.peers.len(), 2);
    assert!(reply.peers.iter().any(SocketAddr::is_ipv4));
    assert!(reply.peers.iter().any(SocketAddr::is_ipv6));
}

#[test]
fn expired_backlog_returns_busy_and_stopped_still_removes_client() {
    let engine = engine();
    for n in 0..500 {
        engine
            .announce(&request(12, v4(1), 1000 + n, 1), 0)
            .unwrap();
    }
    let mut stopped = request(12, v4(1), 1499, 1);
    engine.announce(&stopped, 1000).unwrap();
    assert_eq!(
        engine.scrape(&[stopped.info_hash], 3600),
        Err(EngineError::Busy)
    );
    stopped.event = AnnounceEvent::Stopped;
    assert_eq!(engine.announce(&stopped, 3600), Err(EngineError::Busy));
    assert_eq!(engine.stats().stopped, 1);
    engine.maintenance(3600, 1000);
    let result = engine.scrape(&[stopped.info_hash], 3600).unwrap();
    assert_eq!((result[0].complete, result[0].incomplete), (0, 0));
}

#[test]
fn hard_limits_reject_without_evicting_existing_peer() {
    let engine = SwarmEngine::new(EngineConfig {
        allow_private_ips: true,
        max_peers: 1,
        ..EngineConfig::default()
    });
    let a = request(6, v4(1), 1001, 0);
    let b = request(6, v4(2), 1002, 1);
    engine.announce(&a, 0).unwrap();
    assert_eq!(engine.announce(&b, 1), Err(EngineError::PeerCapacity));
    assert_eq!(engine.stats().peers, 1);
    assert_eq!(engine.scrape(&[a.info_hash], 1).unwrap()[0].complete, 1);
    assert_eq!(engine.stats().rejected_peers, 1);
}

#[test]
fn memory_limit_rolls_back_and_empty_shards_release_reservations() {
    let engine = SwarmEngine::new(EngineConfig {
        allow_private_ips: true,
        memory_budget_bytes: 1200,
        ..EngineConfig::default()
    });
    let a = request(7, v4(1), 1001, 1);
    let b = request(7, v4(2), 1002, 1);
    engine.announce(&a, 0).unwrap();
    assert_eq!(engine.announce(&b, 1), Err(EngineError::MemoryBudget));
    assert_eq!((engine.stats().peers, engine.stats().swarms), (1, 1));
    let mut stop = a;
    stop.event = AnnounceEvent::Stopped;
    engine.announce(&stop, 2).unwrap();
    assert_eq!(engine.stats().estimated_bytes, 0);
}

#[test]
fn freed_slab_slots_remain_accounted_until_reused_or_released() {
    let engine = engine();
    let keep = request(13, v4(1), 1001, 1);
    let mut drop = request(13, v4(2), 1002, 1);
    engine.announce(&keep, 0).unwrap();
    engine.announce(&drop, 0).unwrap();
    let before = engine.stats().estimated_bytes;
    drop.event = AnnounceEvent::Stopped;
    engine.announce(&drop, 1).unwrap();
    assert_eq!(engine.stats().estimated_bytes, before);
    drop.event = AnnounceEvent::None;
    engine.announce(&drop, 2).unwrap();
    assert_eq!(engine.stats().estimated_bytes, before);
    drop.event = AnnounceEvent::Stopped;
    engine.announce(&drop, 3).unwrap();
    let mut keep = keep;
    keep.event = AnnounceEvent::Stopped;
    engine.announce(&keep, 3).unwrap();
    assert_eq!(engine.stats().estimated_bytes, 0);
}

#[test]
fn concurrent_admission_never_exceeds_global_capacity() {
    let engine = Arc::new(SwarmEngine::new(EngineConfig {
        allow_private_ips: true,
        max_peers: 100,
        ..EngineConfig::default()
    }));
    let workers: Vec<_> = (0..8)
        .map(|worker| {
            let engine = Arc::clone(&engine);
            thread::spawn(move || {
                for i in 0..50 {
                    let port = 1000 + worker * 50 + i;
                    let _ = engine.announce(&request(8, v4(1), port, 1), 0);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(engine.stats().peers, 100);
    assert_eq!(engine.scrape(&[[8; 20]], 1).unwrap()[0].incomplete, 100);
}

#[test]
fn concurrent_cross_shard_admission_respects_shared_capacity() {
    let engine = Arc::new(SwarmEngine::new(EngineConfig {
        allow_private_ips: true,
        max_peers: 100,
        ..EngineConfig::default()
    }));
    let workers: Vec<_> = (0..8)
        .map(|worker| {
            let engine = Arc::clone(&engine);
            thread::spawn(move || {
                for i in 0..30 {
                    let hash = (worker * 30 + i) as u8;
                    let _ = engine.announce(&request(hash, v4(1), 1000 + hash as u16, 1), 0);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(engine.stats().peers, 100);
    assert_eq!(engine.stats().swarms, 100);
}

#[test]
fn hot_swarm_sampling_is_bounded_and_timer_refresh_is_not_duplicated() {
    let engine = engine();
    for n in 1..=300 {
        engine
            .announce(&request(9, v4(1), 1000 + n, n as u64 % 2), 0)
            .unwrap();
    }
    let a = request(9, v4(1), 1001, 1);
    for t in 1..=1000 {
        engine.announce(&a, t).unwrap();
    }
    let reply = engine.announce(&a, 1000).unwrap();
    assert!(reply.peers.len() <= 50);
    assert!(!reply.peers.contains(&SocketAddr::new(a.ip, a.port)));
    engine.maintenance(3600, 500);
    assert_eq!(engine.stats().peers, 1);
    engine.maintenance(4600, 500);
    assert_eq!(engine.stats().peers, 0);
}

#[test]
fn production_address_policy_rejects_private_and_unspecified() {
    let engine = SwarmEngine::new(EngineConfig::default());
    assert_eq!(
        engine.announce(&request(10, v4(1), 1001, 1), 0),
        Err(EngineError::InvalidAddress)
    );
    assert_eq!(
        engine.announce(&request(10, IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1001, 1), 0),
        Err(EngineError::InvalidAddress)
    );
    assert!(
        engine
            .announce(
                &request(10, IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 1001, 1),
                0
            )
            .is_ok()
    );
}
