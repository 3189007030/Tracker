//! Bounded, process-local BitTorrent swarm state shared by protocol frontends.
//!
//! Each peer has exactly one entry in a sparse second-granularity timing wheel.
//! The wheel is a map from a deadline to a compact bucket of slab indices;
//! refreshing or removing a peer removes its old timer in O(log buckets).
//! Maintenance is incremental. The memory budget is an admission estimate, not
//! an operating-system RSS limit; production must also set a cgroup limit.

use std::collections::{BTreeMap, HashMap, HashSet, hash_map::RandomState};
use std::hash::BuildHasher;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use bt_protocol::{AnnounceEvent, AnnounceRequest, AnnounceResponse, InfoHash, ScrapeEntry};
use rand::Rng;
use serde::Serialize;

const SHARDS: usize = 64;
const MAX_RESPONSE_PEERS: usize = 100;
// Conservative admission accounting, including estimated index/allocator cost.
const PEER_BYTES: usize = 512;
const SWARM_BYTES: usize = 640;
const INLINE_EXPIRY_BUDGET: usize = 128;

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub interval_secs: u32,
    pub min_interval_secs: u32,
    pub normal_ttl_secs: u64,
    pub cold_seed_ttl_secs: u64,
    pub max_peers: usize,
    pub max_swarms: usize,
    pub max_peers_per_swarm: usize,
    pub memory_budget_bytes: usize,
    pub allow_private_ips: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            interval_secs: 1800,
            min_interval_secs: 300,
            normal_ttl_secs: 3600,
            cold_seed_ttl_secs: 5400,
            max_peers: 1_500_000,
            max_swarms: 400_000,
            max_peers_per_swarm: 10_000,
            memory_budget_bytes: 768 * 1024 * 1024,
            allow_private_ips: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineError {
    InvalidAddress,
    InvalidPort,
    PeerCapacity,
    SwarmCapacity,
    SwarmPeerCapacity,
    MemoryBudget,
    Busy,
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidAddress => "invalid or non-public address",
            Self::InvalidPort => "invalid port",
            Self::PeerCapacity => "peer capacity reached",
            Self::SwarmCapacity => "swarm capacity reached",
            Self::SwarmPeerCapacity => "swarm peer capacity reached",
            Self::MemoryBudget => "memory budget reached",
            Self::Busy => "expiry backlog; please retry",
        })
    }
}
impl std::error::Error for EngineError {}

#[derive(Clone, Debug, Serialize)]
pub struct EngineStats {
    pub peers: usize,
    pub swarms: usize,
    pub estimated_bytes: usize,
    pub announces: u64,
    pub scrapes: u64,
    pub expired: u64,
    pub stopped: u64,
    pub rejected_peers: u64,
    pub rejected_swarms: u64,
    pub rejected_memory: u64,
    pub rejected_swarm_peers: u64,
    pub rejected_busy: u64,
}

#[derive(Default)]
struct Counters {
    peers: AtomicUsize,
    swarms: AtomicUsize,
    estimated_bytes: AtomicUsize,
    announces: AtomicU64,
    scrapes: AtomicU64,
    expired: AtomicU64,
    stopped: AtomicU64,
    rejected_peers: AtomicU64,
    rejected_swarms: AtomicU64,
    rejected_memory: AtomicU64,
    rejected_swarm_peers: AtomicU64,
    rejected_busy: AtomicU64,
}

struct Peer {
    hash: InfoHash,
    endpoint: SocketAddr,
    peer_id: [u8; 20],
    seed: bool,
    completed_reported: bool,
    updated: u64,
    swarm_pos: usize,
    deadline: u64,
    wheel_pos: usize,
    swarm_timer_pos: usize,
}

#[derive(Default)]
struct Swarm {
    ids: Vec<usize>,
    endpoints: HashMap<SocketAddr, usize>,
    // HashMap::capacity() can fall after removals because of tombstones even
    // while its old allocation remains. Preserve the observed allocation
    // high-water until a rebuild actually replaces the old table.
    endpoint_capacity_high_water: usize,
    timers: BTreeMap<u64, Vec<usize>>,
    v4: usize,
    v6: usize,
    seeders: u32,
    completed: u32,
}

#[derive(Default)]
struct Shard {
    swarms: HashMap<InfoHash, Swarm>,
    // Retained table capacity is accounted until the whole shard empties.
    swarm_slots: usize,
    peers: Vec<Option<Peer>>,
    free: Vec<usize>,
    // Each live peer occurs exactly once, including when its deadline changes.
    wheel: BTreeMap<u64, Vec<usize>>,
}

fn reserve(counter: &AtomicUsize, n: usize, limit: usize) -> bool {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |v| {
            v.checked_add(n).filter(|next| *next <= limit)
        })
        .is_ok()
}

fn lock(shard: &Mutex<Shard>) -> MutexGuard<'_, Shard> {
    shard
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Keep live timer buckets proportional to their remaining entries. The copy
/// preserves ordering, so stored per-peer positions remain valid. A shard-wide
/// bucket may contain peers from many swarms and may be as large as the total
/// global peer limit; this deliberately makes no constant-latency claim.
fn compact_timer_bucket(bucket: &mut Vec<usize>) {
    let len = bucket.len();
    if len > 0 && bucket.capacity() > len.saturating_mul(4) {
        *bucket = bucket.to_vec();
    }
}

impl Shard {
    /// Rebuild oversized per-swarm indexes only after substantial deletion.
    /// A still-live swarm must not retain every historical peak forever: its
    /// peer slots can be reused by another swarm in this shard. Rebuilding
    /// reads only this swarm's live IDs, preserves their order/positions, and
    /// occurs under its existing shard lock. No deferred work queue is needed.
    fn compact_surviving_swarm(&mut self, hash: &InfoHash) {
        let Some(swarm) = self.swarms.get(hash) else {
            return;
        };
        let len = swarm.ids.len();
        if len == 0 {
            return;
        }
        let threshold = len.saturating_mul(4);
        let compact_ids = swarm.ids.capacity() > threshold;
        let compact_endpoints = swarm.endpoint_capacity_high_water > threshold;
        if !compact_ids && !compact_endpoints {
            return;
        }

        // At the low-water mark the copied live set is at most one quarter
        // of the old capacity. Both old and new buffers coexist temporarily;
        // accounting is deliberately left conservative until the shard empties.
        let ids = compact_ids.then(|| swarm.ids.to_vec());
        let endpoints = compact_endpoints.then(|| {
            let mut compact = HashMap::with_capacity(len);
            for id in &swarm.ids {
                let peer = self.peers[*id].as_ref().expect("live peer");
                compact.insert(peer.endpoint, *id);
            }
            compact
        });
        let swarm = self.swarms.get_mut(hash).expect("live swarm");
        if let Some(ids) = ids {
            swarm.ids = ids;
        }
        if let Some(endpoints) = endpoints {
            swarm.endpoint_capacity_high_water = endpoints.capacity();
            swarm.endpoints = endpoints;
        }
    }

    fn unschedule(&mut self, id: usize) {
        let peer = self.peers[id].as_ref().expect("live peer");
        let (deadline, pos, hash, swarm_pos) = (
            peer.deadline,
            peer.wheel_pos,
            peer.hash,
            peer.swarm_timer_pos,
        );
        let (moved, empty) = {
            let bucket = self.wheel.get_mut(&deadline).expect("peer timer exists");
            bucket.swap_remove(pos);
            compact_timer_bucket(bucket);
            (bucket.get(pos).copied(), bucket.is_empty())
        };
        if let Some(moved) = moved {
            self.peers[moved].as_mut().expect("live peer").wheel_pos = pos;
        }
        if empty {
            self.wheel.remove(&deadline);
        }
        let (moved, empty) = {
            let timers = &mut self.swarms.get_mut(&hash).expect("live swarm").timers;
            let bucket = timers.get_mut(&deadline).expect("swarm timer exists");
            bucket.swap_remove(swarm_pos);
            compact_timer_bucket(bucket);
            (bucket.get(swarm_pos).copied(), bucket.is_empty())
        };
        if let Some(moved) = moved {
            self.peers[moved]
                .as_mut()
                .expect("live peer")
                .swarm_timer_pos = swarm_pos;
        }
        if empty {
            self.swarms
                .get_mut(&hash)
                .expect("live swarm")
                .timers
                .remove(&deadline);
        }
    }

    fn schedule(&mut self, id: usize, deadline: u64) {
        let bucket = self.wheel.entry(deadline).or_default();
        let pos = bucket.len();
        bucket.push(id);
        let peer = self.peers[id].as_mut().expect("live peer");
        peer.deadline = deadline;
        peer.wheel_pos = pos;
        let hash = peer.hash;
        let bucket = self
            .swarms
            .get_mut(&hash)
            .expect("live swarm")
            .timers
            .entry(deadline)
            .or_default();
        let swarm_pos = bucket.len();
        bucket.push(id);
        self.peers[id].as_mut().expect("live peer").swarm_timer_pos = swarm_pos;
    }

    // Only called on the boundary between <=3 and >=4 members; at most four
    // peers need their seed expiry recalculated, regardless of swarm size.
    fn adjust_small(&mut self, hash: &InfoHash, now: u64, cfg: &EngineConfig) {
        let Some(swarm) = self.swarms.get(hash) else {
            return;
        };
        if swarm.ids.len() > 4 {
            return;
        }
        let rare = swarm.ids.len() <= 3;
        let ids = swarm.ids.clone();
        for id in ids {
            let p = self.peers[id].as_ref().expect("live peer");
            let ttl = if p.seed && rare {
                cfg.cold_seed_ttl_secs
            } else {
                cfg.normal_ttl_secs
            };
            let new_deadline = p.updated.saturating_add(ttl);
            // A seed that was already due cannot be resurrected just because
            // removal of another expired peer made the swarm small again.
            if p.deadline != new_deadline && !(new_deadline > p.deadline && p.deadline <= now) {
                self.unschedule(id);
                self.schedule(id, new_deadline);
            }
        }
    }

    fn remove(&mut self, id: usize, now: u64, cfg: &EngineConfig, counts: &Counters) {
        self.unschedule(id);
        let peer = self.peers[id].take().expect("live peer");
        let (moved, empty, size) = {
            let swarm = self.swarms.get_mut(&peer.hash).expect("live swarm");
            swarm.endpoints.remove(&peer.endpoint);
            swarm.ids.swap_remove(peer.swarm_pos);
            if peer.endpoint.is_ipv4() {
                swarm.v4 -= 1;
            } else {
                swarm.v6 -= 1;
            }
            if peer.seed {
                swarm.seeders = swarm.seeders.saturating_sub(1);
            }
            (
                swarm.ids.get(peer.swarm_pos).copied(),
                swarm.ids.is_empty(),
                swarm.ids.len(),
            )
        };
        if let Some(moved) = moved {
            self.peers[moved].as_mut().expect("live peer").swarm_pos = peer.swarm_pos;
        }
        self.free.push(id);
        counts.peers.fetch_sub(1, Ordering::AcqRel);
        if empty {
            self.swarms.remove(&peer.hash);
            counts.swarms.fetch_sub(1, Ordering::AcqRel);
        } else {
            if size <= 3 {
                self.adjust_small(&peer.hash, now, cfg);
            }
            self.compact_surviving_swarm(&peer.hash);
        }
        if self.swarms.is_empty() {
            // Reclaim a fully idle shard rather than retaining slab high-water
            // allocations forever when load shifts between shards.
            let slots = self.peers.len();
            self.peers = Vec::new();
            self.free = Vec::new();
            self.swarms = HashMap::new();
            let retained = slots
                .saturating_mul(PEER_BYTES)
                .saturating_add(self.swarm_slots.saturating_mul(SWARM_BYTES));
            self.swarm_slots = 0;
            counts.estimated_bytes.fetch_sub(retained, Ordering::AcqRel);
        }
    }

    fn expire_due(
        &mut self,
        now: u64,
        budget: usize,
        cfg: &EngineConfig,
        counts: &Counters,
    ) -> usize {
        let mut done = 0;
        while done < budget {
            let Some((&deadline, bucket)) = self.wheel.first_key_value() else {
                break;
            };
            if deadline > now {
                break;
            }
            let id = *bucket.last().expect("nonempty timer bucket");
            self.remove(id, now, cfg, counts);
            counts.expired.fetch_add(1, Ordering::Relaxed);
            done += 1;
        }
        done
    }

    fn existing_id(&self, hash: &InfoHash, endpoint: &SocketAddr) -> Option<usize> {
        self.swarms.get(hash)?.endpoints.get(endpoint).copied()
    }

    fn expire_swarm(
        &mut self,
        hash: &InfoHash,
        now: u64,
        budget: usize,
        cfg: &EngineConfig,
        counts: &Counters,
    ) -> (bool, usize) {
        let mut used = 0;
        for _ in 0..budget {
            let Some(swarm) = self.swarms.get(hash) else {
                return (true, used);
            };
            let Some((&deadline, bucket)) = swarm.timers.first_key_value() else {
                return (true, used);
            };
            if deadline > now {
                return (true, used);
            }
            let id = *bucket.last().expect("nonempty swarm timer bucket");
            self.remove(id, now, cfg, counts);
            counts.expired.fetch_add(1, Ordering::Relaxed);
            used += 1;
        }
        let complete = self
            .swarms
            .get(hash)
            .and_then(|s| s.timers.first_key_value())
            .is_none_or(|(deadline, _)| *deadline > now);
        (complete, used)
    }
}

pub struct SwarmEngine {
    cfg: EngineConfig,
    shards: Vec<Mutex<Shard>>,
    hasher: RandomState,
    counts: Counters,
    maintenance_cursor: AtomicUsize,
}

impl SwarmEngine {
    pub fn new(cfg: EngineConfig) -> Self {
        Self {
            cfg,
            shards: (0..SHARDS).map(|_| Mutex::new(Shard::default())).collect(),
            hasher: RandomState::new(),
            counts: Counters::default(),
            maintenance_cursor: AtomicUsize::new(0),
        }
    }

    fn shard(&self, hash: &InfoHash) -> usize {
        (self.hasher.hash_one(hash) as usize) % SHARDS
    }

    fn reserve_peer(&self, needs_slot: bool) -> Result<(), EngineError> {
        if !reserve(&self.counts.peers, 1, self.cfg.max_peers) {
            self.counts.rejected_peers.fetch_add(1, Ordering::Relaxed);
            return Err(EngineError::PeerCapacity);
        }
        if needs_slot
            && !reserve(
                &self.counts.estimated_bytes,
                PEER_BYTES,
                self.cfg.memory_budget_bytes,
            )
        {
            self.counts.peers.fetch_sub(1, Ordering::AcqRel);
            self.counts.rejected_memory.fetch_add(1, Ordering::Relaxed);
            return Err(EngineError::MemoryBudget);
        }
        Ok(())
    }

    fn reserve_swarm(&self, needs_slot: bool) -> Result<(), EngineError> {
        if !reserve(&self.counts.swarms, 1, self.cfg.max_swarms) {
            self.counts.rejected_swarms.fetch_add(1, Ordering::Relaxed);
            return Err(EngineError::SwarmCapacity);
        }
        if needs_slot
            && !reserve(
                &self.counts.estimated_bytes,
                SWARM_BYTES,
                self.cfg.memory_budget_bytes,
            )
        {
            self.counts.swarms.fetch_sub(1, Ordering::AcqRel);
            self.counts.rejected_memory.fetch_add(1, Ordering::Relaxed);
            return Err(EngineError::MemoryBudget);
        }
        Ok(())
    }

    pub fn announce(
        &self,
        req: &AnnounceRequest,
        now: u64,
    ) -> Result<AnnounceResponse, EngineError> {
        self.counts.announces.fetch_add(1, Ordering::Relaxed);
        if req.port == 0 {
            return Err(EngineError::InvalidPort);
        }
        if !is_valid_address(req.ip, self.cfg.allow_private_ips) {
            return Err(EngineError::InvalidAddress);
        }
        let endpoint = SocketAddr::new(req.ip, req.port);
        let mut shard = lock(&self.shards[self.shard(&req.info_hash)]);
        shard.expire_due(now, INLINE_EXPIRY_BUDGET, &self.cfg, &self.counts);
        // If the wheel has a backlog, do not revive the requesting peer's old
        // identity or return it as live merely because the timer has not run.
        if let Some(id) = shard.existing_id(&req.info_hash, &endpoint)
            && shard.peers[id].as_ref().expect("live peer").deadline <= now
        {
            shard.remove(id, now, &self.cfg, &self.counts);
            self.counts.expired.fetch_add(1, Ordering::Relaxed);
        }
        if req.event == AnnounceEvent::Stopped {
            if let Some(id) = shard.existing_id(&req.info_hash, &endpoint) {
                shard.remove(id, now, &self.cfg, &self.counts);
                self.counts.stopped.fetch_add(1, Ordering::Relaxed);
            }
            if !shard
                .expire_swarm(
                    &req.info_hash,
                    now,
                    INLINE_EXPIRY_BUDGET,
                    &self.cfg,
                    &self.counts,
                )
                .0
            {
                self.counts.rejected_busy.fetch_add(1, Ordering::Relaxed);
                return Err(EngineError::Busy);
            }
            return Ok(self.response(&shard, req, now));
        }

        if !shard
            .expire_swarm(
                &req.info_hash,
                now,
                INLINE_EXPIRY_BUDGET,
                &self.cfg,
                &self.counts,
            )
            .0
        {
            self.counts.rejected_busy.fetch_add(1, Ordering::Relaxed);
            return Err(EngineError::Busy);
        }

        let old = shard.existing_id(&req.info_hash, &endpoint);
        if let Some(id) = old {
            shard.unschedule(id);
            let (old_seed, old_id, completed_already) = {
                let peer = shard.peers[id].as_ref().expect("live peer");
                (peer.seed, peer.peer_id, peer.completed_reported)
            };
            let seed = req.left == 0;
            let completed = completed_already && old_id == req.peer_id;
            {
                let swarm = shard.swarms.get_mut(&req.info_hash).expect("live swarm");
                if old_seed != seed {
                    if seed {
                        swarm.seeders = swarm.seeders.saturating_add(1);
                    } else {
                        swarm.seeders = swarm.seeders.saturating_sub(1);
                    }
                }
                if req.event == AnnounceEvent::Completed && !completed {
                    swarm.completed = swarm.completed.saturating_add(1);
                }
            }
            let rare = shard.swarms[&req.info_hash].ids.len() <= 3;
            let p = shard.peers[id].as_mut().expect("live peer");
            p.peer_id = req.peer_id;
            p.seed = seed;
            p.updated = now;
            p.completed_reported = completed || req.event == AnnounceEvent::Completed;
            let ttl = if seed && rare {
                self.cfg.cold_seed_ttl_secs
            } else {
                self.cfg.normal_ttl_secs
            };
            shard.schedule(id, now.saturating_add(ttl));
        } else {
            let new_swarm = !shard.swarms.contains_key(&req.info_hash);
            if !new_swarm && shard.swarms[&req.info_hash].ids.len() >= self.cfg.max_peers_per_swarm
            {
                self.counts
                    .rejected_swarm_peers
                    .fetch_add(1, Ordering::Relaxed);
                return Err(EngineError::SwarmPeerCapacity);
            }
            if self.cfg.max_peers_per_swarm == 0 {
                self.counts
                    .rejected_swarm_peers
                    .fetch_add(1, Ordering::Relaxed);
                return Err(EngineError::SwarmPeerCapacity);
            }
            let needs_swarm_slot = new_swarm && shard.swarms.len() >= shard.swarm_slots;
            if new_swarm {
                self.reserve_swarm(needs_swarm_slot)?;
            }
            let needs_slot = shard.free.is_empty();
            if let Err(error) = self.reserve_peer(needs_slot) {
                if new_swarm {
                    self.counts.swarms.fetch_sub(1, Ordering::AcqRel);
                    if needs_swarm_slot {
                        self.counts
                            .estimated_bytes
                            .fetch_sub(SWARM_BYTES, Ordering::AcqRel);
                    }
                }
                return Err(error);
            }
            if new_swarm {
                shard.swarms.insert(req.info_hash, Swarm::default());
                if needs_swarm_slot {
                    shard.swarm_slots += 1;
                }
            }
            let swarm = shard.swarms.get_mut(&req.info_hash).expect("live swarm");
            let pos = swarm.ids.len();
            let seed = req.left == 0;
            let completed = req.event == AnnounceEvent::Completed;
            if seed {
                swarm.seeders = swarm.seeders.saturating_add(1);
            }
            if completed {
                swarm.completed = swarm.completed.saturating_add(1);
            }
            // Reserve slab position after dropping the mutable swarm borrow.
            let _ = swarm;
            let id = if let Some(id) = shard.free.pop() {
                id
            } else {
                shard.peers.push(None);
                shard.peers.len() - 1
            };
            shard.peers[id] = Some(Peer {
                hash: req.info_hash,
                endpoint,
                peer_id: req.peer_id,
                seed,
                completed_reported: completed,
                updated: now,
                swarm_pos: pos,
                deadline: 0,
                wheel_pos: 0,
                swarm_timer_pos: 0,
            });
            let swarm = shard.swarms.get_mut(&req.info_hash).expect("live swarm");
            swarm.ids.push(id);
            swarm.endpoints.insert(endpoint, id);
            swarm.endpoint_capacity_high_water = swarm
                .endpoint_capacity_high_water
                .max(swarm.endpoints.capacity());
            if endpoint.is_ipv4() {
                swarm.v4 += 1;
            } else {
                swarm.v6 += 1;
            }
            let rare = swarm.ids.len() <= 3;
            let ttl = if seed && rare {
                self.cfg.cold_seed_ttl_secs
            } else {
                self.cfg.normal_ttl_secs
            };
            shard.schedule(id, now.saturating_add(ttl));
            if pos == 3 {
                shard.adjust_small(&req.info_hash, now, &self.cfg);
            }
        }
        // Growth from a rare to a regular swarm can make old seed timers due.
        shard.expire_due(now, INLINE_EXPIRY_BUDGET, &self.cfg, &self.counts);
        if !shard
            .expire_swarm(
                &req.info_hash,
                now,
                INLINE_EXPIRY_BUDGET,
                &self.cfg,
                &self.counts,
            )
            .0
        {
            self.counts.rejected_busy.fetch_add(1, Ordering::Relaxed);
            return Err(EngineError::Busy);
        }
        Ok(self.response(&shard, req, now))
    }

    fn response(&self, shard: &Shard, req: &AnnounceRequest, now: u64) -> AnnounceResponse {
        let mut result = AnnounceResponse {
            interval: self.cfg.interval_secs,
            min_interval: self.cfg.min_interval_secs,
            complete: 0,
            incomplete: 0,
            peers: Vec::new(),
        };
        let Some(swarm) = shard.swarms.get(&req.info_hash) else {
            return result;
        };
        result.complete = swarm.seeders;
        result.incomplete = (swarm.ids.len() as u32).saturating_sub(swarm.seeders);
        let wanted = req
            .numwant
            .min(MAX_RESPONSE_PEERS)
            .min(swarm.ids.len().saturating_sub(1));
        if wanted == 0 {
            return result;
        }
        let self_endpoint = SocketAddr::new(req.ip, req.port);
        let mut rng = rand::rng();
        // Bounded sampling avoids scanning arbitrarily large hot swarms. Small
        // swarms are scanned fully to guarantee their only candidate is found.
        let mut v4 = Vec::with_capacity(wanted);
        let mut v6 = Vec::with_capacity(wanted);
        let self_present = swarm.endpoints.contains_key(&self_endpoint);
        let v4_target = swarm
            .v4
            .saturating_sub(usize::from(self_present && self_endpoint.is_ipv4()))
            .min(wanted);
        let v6_target = swarm
            .v6
            .saturating_sub(usize::from(self_present && self_endpoint.is_ipv6()))
            .min(wanted);
        if swarm.ids.len() <= MAX_RESPONSE_PEERS * 2 + 1 {
            let start = rng.random_range(0..swarm.ids.len());
            let mut v4_other = Vec::new();
            let mut v6_other = Vec::new();
            for offset in 0..swarm.ids.len() {
                let peer = shard.peers[swarm.ids[(start + offset) % swarm.ids.len()]]
                    .as_ref()
                    .expect("live peer");
                if peer.endpoint != self_endpoint && peer.deadline > now {
                    let complementary = peer.seed != (req.left == 0);
                    let bucket = match (peer.endpoint.is_ipv4(), complementary) {
                        (true, true) => &mut v4,
                        (false, true) => &mut v6,
                        (true, false) => &mut v4_other,
                        (false, false) => &mut v6_other,
                    };
                    if bucket.len() < wanted {
                        bucket.push(peer.endpoint);
                    }
                }
            }
            fn mix<R: Rng>(
                primary: &mut Vec<SocketAddr>,
                other: &mut Vec<SocketAddr>,
                count: usize,
                rng: &mut R,
            ) {
                let mut output = Vec::with_capacity(count);
                while output.len() < count && (!primary.is_empty() || !other.is_empty()) {
                    let next = if rng.random_range(0..4) != 0 {
                        primary.pop().or_else(|| other.pop())
                    } else {
                        other.pop().or_else(|| primary.pop())
                    };
                    if let Some(endpoint) = next {
                        output.push(endpoint);
                    }
                }
                *primary = output;
            }
            mix(&mut v4, &mut v4_other, v4_target, &mut rng);
            mix(&mut v6, &mut v6_other, v6_target, &mut rng);
        } else {
            let mut selected = HashSet::with_capacity(wanted.saturating_mul(2));
            for _ in 0..wanted.saturating_mul(16).max(32) {
                let peer = shard.peers[swarm.ids[rng.random_range(0..swarm.ids.len())]]
                    .as_ref()
                    .expect("live peer");
                if peer.endpoint == self_endpoint
                    || peer.deadline <= now
                    || selected.contains(&peer.endpoint)
                {
                    continue;
                }
                let complementary = peer.seed != (req.left == 0);
                if !complementary && rng.random_range(0..4) != 0 {
                    continue;
                }
                let bucket = if peer.endpoint.is_ipv4() {
                    &mut v4
                } else {
                    &mut v6
                };
                if bucket.len() >= wanted {
                    continue;
                }
                bucket.push(peer.endpoint);
                selected.insert(peer.endpoint);
                if v4.len() == v4_target && v6.len() == v6_target {
                    break;
                }
            }
        }
        // Total output never exceeds numwant. Alternate families where both
        // exist, starting with the caller's family for UDP compatibility.
        let mut prefer_v4 = req.ip.is_ipv4();
        while result.peers.len() < wanted && (!v4.is_empty() || !v6.is_empty()) {
            let next = if prefer_v4 {
                v4.pop().or_else(|| v6.pop())
            } else {
                v6.pop().or_else(|| v4.pop())
            };
            if let Some(endpoint) = next {
                result.peers.push(endpoint);
            }
            prefer_v4 = !prefer_v4;
        }
        result
    }

    pub fn scrape(&self, hashes: &[InfoHash], now: u64) -> Result<Vec<ScrapeEntry>, EngineError> {
        self.counts.scrapes.fetch_add(1, Ordering::Relaxed);
        let mut remaining = INLINE_EXPIRY_BUDGET;
        let mut result = Vec::with_capacity(hashes.len());
        for hash in hashes {
            let mut shard = lock(&self.shards[self.shard(hash)]);
            let (complete, used) =
                shard.expire_swarm(hash, now, remaining, &self.cfg, &self.counts);
            remaining -= used;
            if !complete {
                self.counts.rejected_busy.fetch_add(1, Ordering::Relaxed);
                return Err(EngineError::Busy);
            }
            let mut entry = ScrapeEntry {
                info_hash: *hash,
                complete: 0,
                downloaded: 0,
                incomplete: 0,
            };
            if let Some(swarm) = shard.swarms.get(hash) {
                entry.complete = swarm.seeders;
                entry.downloaded = swarm.completed;
                entry.incomplete = (swarm.ids.len() as u32).saturating_sub(swarm.seeders);
            }
            result.push(entry);
        }
        Ok(result)
    }

    /// Expire at most `budget` peer records, distributing work across shards.
    pub fn maintenance(&self, now: u64, budget: usize) {
        if budget == 0 {
            return;
        }
        let start = self.maintenance_cursor.fetch_add(1, Ordering::Relaxed) % SHARDS;
        let mut remaining = budget;
        let slice = (budget / SHARDS).clamp(1, 64);
        loop {
            let before = remaining;
            for offset in 0..SHARDS {
                if remaining == 0 {
                    break;
                }
                let mut shard = lock(&self.shards[(start + offset) % SHARDS]);
                // Round-robin batches let a hot shard use spare capacity after
                // other shards have had an opportunity to make progress.
                remaining -= shard.expire_due(now, remaining.min(slice), &self.cfg, &self.counts);
            }
            if remaining == 0 || remaining == before {
                break;
            }
        }
    }

    pub fn stats(&self) -> EngineStats {
        let c = &self.counts;
        EngineStats {
            peers: c.peers.load(Ordering::Acquire),
            swarms: c.swarms.load(Ordering::Acquire),
            estimated_bytes: c.estimated_bytes.load(Ordering::Acquire),
            announces: c.announces.load(Ordering::Relaxed),
            scrapes: c.scrapes.load(Ordering::Relaxed),
            expired: c.expired.load(Ordering::Relaxed),
            stopped: c.stopped.load(Ordering::Relaxed),
            rejected_peers: c.rejected_peers.load(Ordering::Relaxed),
            rejected_swarms: c.rejected_swarms.load(Ordering::Relaxed),
            rejected_memory: c.rejected_memory.load(Ordering::Relaxed),
            rejected_swarm_peers: c.rejected_swarm_peers.load(Ordering::Relaxed),
            rejected_busy: c.rejected_busy.load(Ordering::Relaxed),
        }
    }
}

fn is_valid_address(ip: IpAddr, allow_private: bool) -> bool {
    match ip {
        IpAddr::V4(v) => {
            if v.is_unspecified() || v.is_multicast() || v.is_broadcast() {
                return false;
            }
            if allow_private {
                return true;
            }
            let [a, b, c, _] = v.octets();
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_documentation()
                || a == 0
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19)))
        }
        IpAddr::V6(v) => {
            if v.is_unspecified() || v.is_multicast() {
                return false;
            }
            if allow_private {
                return true;
            }
            let s = v.segments();
            // Conservatively accept global-unicast space while excluding IETF
            // special-purpose and documentation ranges; v4-mapped is outside.
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] <= 0x01ff || s[1] == 0x0db8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] <= 0x0fff)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn request_for(hash: InfoHash, port: u16, seed: bool) -> AnnounceRequest {
        AnnounceRequest {
            info_hash: hash,
            peer_id: [port as u8; 20],
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            port,
            uploaded: 0,
            downloaded: 0,
            left: if seed { 0 } else { 1 },
            event: AnnounceEvent::None,
            numwant: 0,
        }
    }

    fn assert_timer_positions(shard: &Shard) {
        for (deadline, bucket) in &shard.wheel {
            for (position, id) in bucket.iter().enumerate() {
                let peer = shard.peers[*id].as_ref().expect("live wheel peer");
                assert_eq!(peer.deadline, *deadline);
                assert_eq!(peer.wheel_pos, position);
            }
        }
        for (hash, swarm) in &shard.swarms {
            for (deadline, bucket) in &swarm.timers {
                for (position, id) in bucket.iter().enumerate() {
                    let peer = shard.peers[*id].as_ref().expect("live swarm timer peer");
                    assert_eq!(&peer.hash, hash);
                    assert_eq!(peer.deadline, *deadline);
                    assert_eq!(peer.swarm_timer_pos, position);
                }
            }
        }
    }

    #[test]
    fn same_deadline_many_swarms_compact_both_timer_layers() {
        const SWARMS: usize = 8;
        const PER_SWARM: usize = 128;
        let engine = SwarmEngine::new(EngineConfig {
            allow_private_ips: true,
            ..EngineConfig::default()
        });
        let target = engine.shard(&[0; 20]);
        let hashes: Vec<InfoHash> = (0..100_000_u32)
            .filter_map(|n| {
                let mut hash = [0; 20];
                hash[..4].copy_from_slice(&n.to_be_bytes());
                (engine.shard(&hash) == target).then_some(hash)
            })
            .take(SWARMS)
            .collect();
        assert_eq!(hashes.len(), SWARMS);
        for hash in &hashes {
            for n in 0..PER_SWARM {
                engine
                    .announce(&request_for(*hash, 10_000 + n as u16, false), 0)
                    .unwrap();
            }
        }
        {
            let shard = lock(&engine.shards[target]);
            assert_eq!(shard.wheel[&3600].len(), SWARMS * PER_SWARM);
        }
        for hash in &hashes {
            for n in 1..PER_SWARM {
                let mut request = request_for(*hash, 10_000 + n as u16, false);
                request.event = AnnounceEvent::Stopped;
                engine.announce(&request, 1).unwrap();
            }
        }
        {
            let shard = lock(&engine.shards[target]);
            let global_bucket = &shard.wheel[&3600];
            assert_eq!(global_bucket.len(), SWARMS);
            assert!(global_bucket.capacity() <= global_bucket.len() * 4);
            for hash in &hashes {
                let bucket = &shard.swarms[hash].timers[&3600];
                assert_eq!(bucket.len(), 1);
                assert!(bucket.capacity() <= 4);
            }
            assert_timer_positions(&shard);
        }
        assert_eq!(engine.stats().peers, SWARMS);
        for hash in &hashes {
            let mut probe = request_for(*hash, 50_000, false);
            probe.numwant = 1;
            let response = engine.announce(&probe, 2).unwrap();
            assert_eq!(response.peers, vec![SocketAddr::new(probe.ip, 10_000)]);
            probe.event = AnnounceEvent::Stopped;
            engine.announce(&probe, 2).unwrap();
        }
        engine.maintenance(3600, 256);
        assert_eq!(engine.stats().peers, 0);
    }

    #[test]
    fn dispersed_deadline_buckets_compact_without_changing_positions() {
        const DEADLINES: usize = 8;
        const PER_DEADLINE: usize = 128;
        let engine = SwarmEngine::new(EngineConfig {
            allow_private_ips: true,
            ..EngineConfig::default()
        });
        let hash = [99; 20];
        for group in 0..DEADLINES {
            for n in 0..PER_DEADLINE {
                let port = 10_000 + (group * PER_DEADLINE + n) as u16;
                engine
                    .announce(&request_for(hash, port, false), group as u64)
                    .unwrap();
            }
        }
        for group in 0..DEADLINES {
            for n in 1..PER_DEADLINE {
                let port = 10_000 + (group * PER_DEADLINE + n) as u16;
                let mut request = request_for(hash, port, false);
                request.event = AnnounceEvent::Stopped;
                engine.announce(&request, 100).unwrap();
            }
        }
        {
            let shard = lock(&engine.shards[engine.shard(&hash)]);
            let swarm = &shard.swarms[&hash];
            assert_eq!(swarm.timers.len(), DEADLINES);
            for group in 0..DEADLINES {
                let deadline = 3600 + group as u64;
                let local = &swarm.timers[&deadline];
                let global = &shard.wheel[&deadline];
                assert_eq!(local.len(), 1);
                assert_eq!(global.len(), 1);
                assert!(local.capacity() <= 4);
                assert!(global.capacity() <= 4);
            }
            assert_timer_positions(&shard);
        }
        let entry = &engine.scrape(&[hash], 100).unwrap()[0];
        assert_eq!((entry.complete, entry.incomplete), (0, DEADLINES as u32));
        engine.maintenance(3600, 256);
        assert_eq!(engine.stats().peers, DEADLINES - 1);
        engine.maintenance(3600 + DEADLINES as u64 - 1, 256);
        assert_eq!(engine.stats().peers, 0);
    }

    #[test]
    fn sequential_swarm_churn_releases_per_swarm_high_water() {
        const SWARMS: usize = 8;
        const PEERS_PER_SWARM: usize = 256;
        let engine = SwarmEngine::new(EngineConfig {
            allow_private_ips: true,
            max_peers: PEERS_PER_SWARM + SWARMS - 1,
            ..EngineConfig::default()
        });
        let target = engine.shard(&[0; 20]);
        let hashes: Vec<InfoHash> = (0..100_000_u32)
            .filter_map(|n| {
                let mut hash = [0; 20];
                hash[..4].copy_from_slice(&n.to_be_bytes());
                (engine.shard(&hash) == target).then_some(hash)
            })
            .take(SWARMS)
            .collect();
        assert_eq!(hashes.len(), SWARMS);

        for hash in &hashes {
            for n in 0..PEERS_PER_SWARM {
                let request = request_for(*hash, 10_000 + n as u16, n == 0);
                engine.announce(&request, n as u64).unwrap();
            }
            for n in 1..PEERS_PER_SWARM {
                let mut request = request_for(*hash, 10_000 + n as u16, false);
                request.event = AnnounceEvent::Stopped;
                engine.announce(&request, 300).unwrap();
            }
            let shard = lock(&engine.shards[target]);
            let swarm = shard.swarms.get(hash).expect("anchor retained");
            assert_eq!(swarm.ids.len(), 1);
            assert_eq!(swarm.endpoints.len(), 1);
            assert!(swarm.ids.capacity() <= 4);
            assert!(swarm.endpoint_capacity_high_water <= 4);
            assert!(swarm.endpoints.capacity() <= 4);
        }

        let stats = engine.stats();
        assert_eq!(stats.peers, SWARMS);
        assert_eq!(stats.swarms, SWARMS);
        assert_eq!(
            stats.estimated_bytes,
            (PEERS_PER_SWARM + SWARMS - 1) * PEER_BYTES + SWARMS * SWARM_BYTES
        );
        for hash in &hashes {
            let mut probe = request_for(*hash, 50_000, false);
            probe.numwant = 1;
            let result = engine.announce(&probe, 301).unwrap();
            assert_eq!(result.peers, vec![SocketAddr::new(probe.ip, 10_000)]);
            probe.event = AnnounceEvent::Stopped;
            engine.announce(&probe, 301).unwrap();
        }

        // Filling the released slab to the configured peer limit must reject
        // the next client rather than evicting any retained anchor.
        for n in 1..PEERS_PER_SWARM {
            engine
                .announce(&request_for(hashes[0], 20_000 + n as u16, false), 302)
                .unwrap();
        }
        assert_eq!(
            engine.announce(&request_for(hashes[0], 30_000, false), 302),
            Err(EngineError::PeerCapacity)
        );
        assert_eq!(engine.stats().peers, PEERS_PER_SWARM + SWARMS - 1);
        for hash in &hashes {
            let shard = lock(&engine.shards[target]);
            assert!(shard.swarms[hash].endpoints.contains_key(&SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                10_000
            )));
        }
    }

    #[test]
    fn concurrent_churn_preserves_dispersed_deadlines_and_discovery() {
        const WORKERS: usize = 6;
        const PER_WORKER: usize = 96;
        let engine = Arc::new(SwarmEngine::new(EngineConfig {
            allow_private_ips: true,
            ..EngineConfig::default()
        }));
        let barrier = Arc::new(Barrier::new(WORKERS));
        let hash = [42; 20];
        let handles: Vec<_> = (0..WORKERS)
            .map(|worker| {
                let engine = Arc::clone(&engine);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let base = 10_000 + (worker * PER_WORKER) as u16;
                    for n in 0..PER_WORKER {
                        let request = request_for(hash, base + n as u16, false);
                        engine.announce(&request, n as u64).unwrap();
                    }
                    barrier.wait();
                    for n in 1..PER_WORKER {
                        let mut request = request_for(hash, base + n as u16, false);
                        request.event = AnnounceEvent::Stopped;
                        engine.announce(&request, 500 + worker as u64).unwrap();
                        if n % 16 == 0 {
                            engine
                                .announce(&request_for(hash, base, false), 1000 + worker as u64)
                                .unwrap();
                        }
                    }
                    engine
                        .announce(&request_for(hash, base, false), 1000 + worker as u64)
                        .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(engine.stats().peers, WORKERS);
        {
            let shard = lock(&engine.shards[engine.shard(&hash)]);
            let swarm = &shard.swarms[&hash];
            assert_eq!(swarm.ids.len(), WORKERS);
            assert_eq!(swarm.endpoints.len(), WORKERS);
            assert_eq!(swarm.timers.len(), WORKERS);
            assert!(swarm.ids.capacity() <= WORKERS * 4);
            assert!(swarm.endpoint_capacity_high_water <= WORKERS * 4);
            for (position, id) in swarm.ids.iter().enumerate() {
                let peer = shard.peers[*id].as_ref().unwrap();
                assert_eq!(peer.swarm_pos, position);
                assert_eq!(swarm.endpoints.get(&peer.endpoint), Some(id));
                assert_eq!(swarm.timers[&peer.deadline][peer.swarm_timer_pos], *id);
                assert_eq!(shard.wheel[&peer.deadline][peer.wheel_pos], *id);
            }
        }
        for worker in 0..WORKERS {
            let base = 10_000 + (worker * PER_WORKER) as u16;
            let mut request = request_for(hash, base, false);
            request.numwant = 50;
            let result = engine.announce(&request, 2000).unwrap();
            assert_eq!(result.peers.len(), WORKERS - 1);
            for other in 0..WORKERS {
                if other != worker {
                    let port = 10_000 + (other * PER_WORKER) as u16;
                    assert!(result.peers.contains(&SocketAddr::new(request.ip, port)));
                }
            }
        }
        engine.maintenance(5599, 256);
        assert_eq!(engine.stats().peers, WORKERS);
        engine.maintenance(5600, 256);
        assert_eq!(engine.stats().peers, 0);
    }

    #[test]
    fn shared_deadline_bucket_removal_preserves_other_swarm_index() {
        let engine = SwarmEngine::new(EngineConfig {
            allow_private_ips: true,
            ..EngineConfig::default()
        });
        let mut first = [None; SHARDS];
        let mut pair = None;
        for n in 0..=255u8 {
            let hash = [n; 20];
            let shard = engine.shard(&hash);
            if let Some(old) = first[shard] {
                pair = Some((old, hash));
                break;
            }
            first[shard] = Some(hash);
        }
        let (a, b) = pair.expect("pigeonhole collision");
        let req = |hash, port| AnnounceRequest {
            info_hash: hash,
            peer_id: [port as u8; 20],
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            port,
            uploaded: 0,
            downloaded: 0,
            left: 1,
            event: AnnounceEvent::None,
            numwant: 0,
        };
        let mut one = req(a, 1001);
        let two = req(b, 1002);
        engine.announce(&one, 0).unwrap();
        engine.announce(&two, 0).unwrap();
        one.event = AnnounceEvent::Stopped;
        engine.announce(&one, 1).unwrap();
        engine.announce(&two, 2).unwrap();
        engine.maintenance(3600, 512);
        assert_eq!(engine.stats().peers, 1);
        engine.maintenance(3602, 512);
        assert_eq!(engine.stats().peers, 0);
        assert_eq!(engine.stats().estimated_bytes, 0);
    }
}
