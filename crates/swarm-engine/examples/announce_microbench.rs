//! In-process, no-network comparison of existing peer refresh paths.
//! Run identical binaries on the same host: cargo run --release -p swarm-engine
//! --example announce_microbench -- 50000
use bt_protocol::{AnnounceEvent, AnnounceRequest};
use std::{
    hint::black_box,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Instant,
};
use swarm_engine::{EngineConfig, SwarmEngine};

fn request(index: usize, wanted: usize) -> AnnounceRequest {
    AnnounceRequest {
        info_hash: [7; 20],
        peer_id: [(index % 251) as u8; 20],
        ip: if index.is_multiple_of(2) {
            IpAddr::V4(Ipv4Addr::new(
                10,
                0,
                (index / 250) as u8,
                (index % 250 + 1) as u8,
            ))
        } else {
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, index as u16))
        },
        port: 10000 + index as u16,
        uploaded: 0,
        downloaded: 0,
        left: if index.is_multiple_of(3) { 0 } else { 1 },
        event: AnnounceEvent::None,
        numwant: wanted,
    }
}
fn main() {
    let operations = std::env::args()
        .nth(1)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50_000)
        .clamp(1, 500_000);
    for peers in [3usize, 64, 512] {
        for wanted in [0usize, 50] {
            for fixed in [true, false] {
                let engine = SwarmEngine::new(EngineConfig {
                    allow_private_ips: true,
                    ..Default::default()
                });
                for n in 0..peers {
                    engine.announce(&request(n, 0), 100).expect("prefill");
                }
                let probe = request(1, wanted);
                let start = Instant::now();
                let mut returned = 0usize;
                for index in 0..operations {
                    let now = if fixed {
                        100
                    } else {
                        100 + (index as u64 / 200)
                    };
                    let reply = engine
                        .announce(black_box(&probe), black_box(now))
                        .expect("announce");
                    returned = returned.saturating_add(black_box(reply.peers.len()));
                }
                let seconds = start.elapsed().as_secs_f64();
                println!(
                    "{{\"peers\":{peers},\"numwant\":{wanted},\"fixed_second\":{fixed},\"operations\":{operations},\"seconds\":{seconds:.9},\"operations_per_second\":{:.3},\"returned_peers\":{returned}}}",
                    operations as f64 / seconds
                );
            }
        }
    }
}
