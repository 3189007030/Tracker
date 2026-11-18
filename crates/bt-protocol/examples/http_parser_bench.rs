//! Run on an isolated host with `cargo run --release -p bt-protocol --example http_parser_bench`.
//! Copy this same file into the baseline checkout before comparing results.
use bt_protocol::parse_http_announce;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    net::{IpAddr, Ipv4Addr},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Instant,
};

struct CountingAllocator;

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn main() {
    let iterations = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse::<u32>().ok())
        .unwrap_or(1_000_000)
        .clamp(10_000, 1_000_000);
    let ip = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
    let cases: [(&str, &[u8]); 3] = [
        (
            "unescaped",
            b"info_hash=12345678901234567890&peer_id=ABCDEFGHIJKLMNOPQRST&port=6881&uploaded=2&downloaded=3&left=4&event=started&numwant=50&compact=1",
        ),
        (
            "mixed",
            b"info_hash=1234567890123456789%30&peer_id=ABCDEFGHIJKLMNOPQRS%54&port=6881&uploaded=2&downloaded=3&left=4&event=started&numwant=50&compact=1",
        ),
        (
            "escaped_hashes",
            b"info_hash=%31%32%33%34%35%36%37%38%39%30%31%32%33%34%35%36%37%38%39%30&peer_id=%41%42%43%44%45%46%47%48%49%4A%4B%4C%4D%4E%4F%50%51%52%53%54&port=6881&uploaded=2&downloaded=3&left=4&event=started&numwant=50&compact=1",
        ),
    ];
    let expected = parse_http_announce(cases[0].1, ip).unwrap();
    for (name, query) in cases {
        assert_eq!(parse_http_announce(query, ip).unwrap(), expected);
        for _ in 0..10_000 {
            black_box(parse_http_announce(black_box(query), ip).unwrap());
        }

        let count_iterations = 10_000;
        ALLOCATIONS.store(0, Ordering::Relaxed);
        COUNT_ALLOCATIONS.store(true, Ordering::Relaxed);
        for _ in 0..count_iterations {
            black_box(parse_http_announce(black_box(query), ip).unwrap());
        }
        COUNT_ALLOCATIONS.store(false, Ordering::Relaxed);
        let allocs = ALLOCATIONS.load(Ordering::Relaxed);
        println!(
            "{name} allocations_per_request={:.3}",
            allocs as f64 / f64::from(count_iterations)
        );

        for sample in 1..=5 {
            let start = Instant::now();
            for _ in 0..iterations {
                black_box(parse_http_announce(black_box(query), ip).unwrap());
            }
            let nanos = start.elapsed().as_nanos() as f64 / f64::from(iterations);
            println!("{name} sample={sample} nanoseconds_per_request={nanos:.2}");
        }
    }
}
