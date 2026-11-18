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
    let mut args = std::env::args().skip(1);
    let operations = args
        .next()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50_000)
        .clamp(1, 500_000);
    let peer_filter = args.next().map(|v| v.parse::<usize>().expect("peer count"));
    let wanted_filter = args.next().map(|v| v.parse::<usize>().expect("numwant"));
    for peers in [1usize, 2, 3, 4, 5, 8, 64, 512]
        .into_iter()
        .filter(|n| peer_filter.is_none_or(|peers| *n == peers))
    {
        for wanted in [0usize, 50]
            .into_iter()
            .filter(|n| wanted_filter.is_none_or(|wanted| *n == wanted))
        {
            for fixed in [true, false] {
                let engine = SwarmEngine::new(EngineConfig {
                    allow_private_ips: true,
                    ..Default::default()
                });
                for n in 0..peers {
                    engine.announce(&request(n, 0), 100).expect("prefill");
                }
                let probe = request(usize::from(peers > 1), wanted);
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
