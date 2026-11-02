//! Request admission: "can this worker take this request now?".

use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::config::ProfileTable;
use crate::envelope::Envelope;
use crate::net::{DriverKind, Lane, WorkerId};
use crate::sync::{AtomicU32, AtomicU64};

/// Request admission, after `head` and before any body byte.
///
/// `admit` and `release` run on the worker that owns the connection, and a
/// connection never moves between workers, so state indexed by `worker` is
/// written by one thread only: a per-worker counter needs no lock and no
/// read-modify-write. `release` runs once per `Admitted`, on every path.
///
/// A deployment may back admission by shared state (a cluster-wide budget);
/// it must then answer from its local share when the shared state is
/// unreachable, never block the worker waiting for the network.
pub trait Admit: Send + Sync + 'static {
    /// Runs once before `prepare`, with the driver that runs the connections
    /// and the server's soft connection cap.
    fn driver(&mut self, kind: DriverKind, connections: u32) {
        let _ = (kind, connections);
    }
    /// Runs once before any worker starts, to size per-worker state.
    fn prepare(&mut self, workers: u16, profiles: &ProfileTable) {
        let _ = (workers, profiles);
    }
    /// The share of capacity kept for lanes, in percent; `build` refuses a
    /// value above 100 (rule `ReserveOverflow`).
    fn reserved_percent(&self) -> u32 {
        0
    }
    /// Admits the request, or refuses it with the busy refusal.
    fn admit(&self, worker: WorkerId, env: &Envelope) -> Result<Admitted, Shed>;
    /// Returns what `admit` took.
    fn release(&self, worker: WorkerId, env: &Envelope, admitted: Admitted);
}

/// What `admit` took; opaque to serve-net (a slot index, a permit id).
#[must_use]
#[derive(Debug, PartialEq, Eq)]
pub struct Admitted {
    /// The implementation's own value, handed back to `release`.
    pub token: u64,
}

/// A refusal from admission: the busy refusal (503) with `Retry-After`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shed {
    /// `Retry-After`; serve-net adds jitter when it is `None`.
    pub retry_after: Option<Duration>,
    /// End the connection after the refusal.
    pub close: bool,
}

/// One worker's counters, alone on its cache line.
#[repr(align(64))]
#[derive(Debug)]
struct WorkerShard {
    in_flight: AtomicU32,
    /// Per-profile counts for profiles with per-worker shares.
    profiles: Box<[AtomicU32]>,
    /// Slots in use per reserve row, under [`ReserveScope::Worker`].
    lanes: Box<[AtomicU32]>,
    /// Slots in use from the pool, under [`ReserveScope::Worker`].
    pool: AtomicU32,
    jitter: AtomicU64,
}

/// How one profile's cap is enforced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProfileCap {
    /// No cap of its own.
    None,
    /// Per-worker shares: `share[worker]` requests on that worker.
    Shares,
    /// One shared counter (small caps, where shares would be unfair).
    Shared(u32),
}

/// Where lane reserves are counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReserveScope {
    /// Each worker's own counters: a load and a store.
    Worker,
    /// One counter per row across workers, for a worker too small to hold a
    /// share of its own (every blocking worker runs one request).
    Server,
}

/// A lane with no reserve row.
const NO_ROW: u8 = u8::MAX;

/// The slots each reserve keeps out of `total`, and what is left for the pool.
/// A reserve keeps at least one slot, but unless the reserves sum to 100%
/// other lanes always keep one too.
fn reserve_slots(total: u32, percents: &[u8]) -> (Vec<u32>, u32) {
    let sum: u32 = percents.iter().map(|p| u32::from(*p)).sum();
    let mut left = total.saturating_sub(u32::from(sum < 100));
    let slots = percents
        .iter()
        .map(|p| {
            let share = u64::from(total) * u64::from(*p) / 100;
            let s = u32::try_from(share).unwrap_or(u32::MAX).max(1).min(left);
            left -= s;
            s
        })
        .collect::<Vec<_>>();
    let pool = total.saturating_sub(slots.iter().sum());
    (slots, pool)
}

/// The default admission control: a real cap on requests in flight per worker
/// and per profile.
///
/// Over the cap, the request gets the busy refusal with `Retry-After` = base
/// plus a jitter in `[0, base)`, so refused clients do not all return in the
/// same second.
///
/// [`reserve`](Self::reserve) keeps capacity for one lane, so a flood of
/// ordinary traffic cannot take the slots of health checks or administrators.
///
/// ```
/// use serve_net::{InFlightCap, Lane};
/// use std::time::Duration;
/// let cap = InFlightCap::new()
///     .per_worker(512)
///     .reserve(Lane(0), 10)
///     .retry_after(Duration::from_secs(2));
/// # let _ = cap;
/// ```
#[derive(Debug)]
pub struct InFlightCap {
    per_worker: u32,
    base: Duration,
    shards: Box<[WorkerShard]>,
    caps: Box<[ProfileCap]>,
    /// `shares[profile * workers + worker]`.
    shares: Box<[u32]>,
    shared: Box<[AtomicU32]>,
    workers: u16,
    /// As asked; `prepare` turns it into the rows below.
    reserves: Vec<(Lane, u8)>,
    /// The soft connection cap on the blocking driver, where one connection
    /// is one request at most.
    blocking_total: Option<u32>,
    scope: ReserveScope,
    /// Reserve row by lane; empty when nothing is reserved.
    lane_rows: Box<[u8]>,
    /// Slots per row.
    row_slots: Box<[u32]>,
    /// Slots every lane may take.
    pool_slots: u32,
    shared_rows: Box<[AtomicU32]>,
    shared_pool: AtomicU32,
}

impl Clone for InFlightCap {
    fn clone(&self) -> Self {
        // A clone is configuration only; `prepare` sizes the state again.
        let mut clone = InFlightCap::new()
            .per_worker(self.per_worker)
            .retry_after(self.base);
        clone.reserves.clone_from(&self.reserves);
        clone
    }
}

impl InFlightCap {
    /// The defaults: 1,024 in flight per worker, `Retry-After` base 1 s.
    #[must_use]
    pub fn new() -> Self {
        InFlightCap {
            per_worker: 1024,
            base: Duration::from_secs(1),
            shards: Box::new([]),
            caps: Box::new([]),
            shares: Box::new([]),
            shared: Box::new([]),
            workers: 0,
            reserves: Vec::new(),
            blocking_total: None,
            scope: ReserveScope::Worker,
            lane_rows: Box::new([]),
            row_slots: Box::new([]),
            pool_slots: 0,
            shared_rows: Box::new([]),
            shared_pool: AtomicU32::new(0),
        }
    }

    /// Requests in flight per worker.
    #[must_use]
    pub fn per_worker(mut self, n: u32) -> Self {
        self.per_worker = n.max(1);
        self
    }

    /// Keeps `percent` of each worker's cap for `lane`: a request takes from
    /// its own lane's reserve first, then from the shared pool, and lanes
    /// without a reserve use only the pool. Reserves summing above 100% are
    /// refused at `build`; a second reserve for one lane replaces the first.
    ///
    /// A worker too small to hold a share counts the reserves across all
    /// workers instead, and the blocking driver, where every connection runs
    /// one request at most, takes the shares of its soft connection cap: the
    /// same promise for one contended atomic per request, about 20 ns [est.].
    /// Reserves on larger workers cost an array index, and nothing when no
    /// reserve is set.
    #[must_use]
    pub fn reserve(mut self, lane: Lane, percent: u8) -> Self {
        self.reserves.retain(|(l, _)| *l != lane);
        if percent > 0 {
            self.reserves.push((lane, percent));
        }
        self
    }

    /// The `Retry-After` base (default 1 s).
    #[must_use]
    pub fn retry_after(mut self, base: Duration) -> Self {
        self.base = base.max(Duration::from_secs(1));
        self
    }

    fn retry(&self, shard: &WorkerShard) -> Duration {
        // xorshift64: not secret, it only spreads retries.
        let mut x = shard.jitter.load(Ordering::Relaxed);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        shard.jitter.store(x, Ordering::Relaxed);
        let base_ms = u64::try_from(self.base.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let jitter_secs = (x % base_ms) / 1000;
        self.base + Duration::from_secs(jitter_secs)
    }

    fn shed(&self, shard: &WorkerShard) -> Shed {
        Shed {
            retry_after: Some(self.retry(shard)),
            close: false,
        }
    }

    /// Turns the reserves into rows, slots, and counters.
    fn prepare_reserves(&mut self) {
        if self.reserves.is_empty() {
            return;
        }
        let percents: Vec<u8> = self.reserves.iter().map(|(_, p)| *p).collect();
        let fits = |p: &u8| u64::from(self.per_worker) * u64::from(*p) >= 100;
        let (scope, total) = match self.blocking_total {
            Some(total) => (ReserveScope::Server, total),
            None if percents.iter().all(fits) => (ReserveScope::Worker, self.per_worker),
            None => {
                let total = u64::from(self.per_worker) * u64::from(self.workers);
                (
                    ReserveScope::Server,
                    u32::try_from(total).unwrap_or(u32::MAX),
                )
            }
        };
        let (slots, pool) = reserve_slots(total, &percents);
        let mut rows = vec![NO_ROW; 256];
        for (row, (lane, _)) in self.reserves.iter().enumerate() {
            rows[usize::from(lane.0)] = u8::try_from(row).unwrap_or(NO_ROW);
        }
        self.scope = scope;
        self.lane_rows = rows.into_boxed_slice();
        self.shared_rows = slots.iter().map(|_| AtomicU32::new(0)).collect();
        self.row_slots = slots.into_boxed_slice();
        self.pool_slots = pool;
    }

    fn take_slot(&self, counter: &AtomicU32, limit: u32) -> bool {
        match self.scope {
            ReserveScope::Server => take_shared(counter, limit),
            ReserveScope::Worker => {
                // Single writer: this worker only.
                let c = counter.load(Ordering::Relaxed);
                let free = c < limit;
                if free {
                    counter.store(c + 1, Ordering::Relaxed);
                }
                free
            }
        }
    }

    fn give_slot(&self, counter: &AtomicU32) {
        match self.scope {
            ReserveScope::Server => give_shared(counter),
            ReserveScope::Worker => {
                let c = counter.load(Ordering::Relaxed);
                counter.store(c.saturating_sub(1), Ordering::Relaxed);
            }
        }
    }

    /// The pool and row counters of this scope.
    fn slot_counters<'a>(&'a self, shard: &'a WorkerShard) -> (&'a AtomicU32, &'a [AtomicU32]) {
        match self.scope {
            ReserveScope::Server => (&self.shared_pool, &self.shared_rows),
            ReserveScope::Worker => (&shard.pool, &shard.lanes),
        }
    }

    /// A slot for `lane`, as `Admitted::token` bits; `None` when both its
    /// reserve and the pool are full.
    fn take_lane(&self, shard: &WorkerShard, lane: Lane) -> Option<u64> {
        let Some(row) = self.lane_rows.get(usize::from(lane.0)).copied() else {
            return Some(0);
        };
        let (pool, rows) = self.slot_counters(shard);
        if row != NO_ROW {
            let r = usize::from(row);
            if self.take_slot(&rows[r], self.row_slots[r]) {
                return Some(COUNTED_ROW | u64::from(row) << ROW_SHIFT);
            }
        }
        self.take_slot(pool, self.pool_slots)
            .then_some(COUNTED_POOL)
    }

    fn give_profile(&self, shard: &WorkerShard, profile: usize, token: u64) {
        if token & COUNTED_SHARE != 0 {
            if let Some(count) = shard.profiles.get(profile) {
                let c = count.load(Ordering::Relaxed);
                count.store(c.saturating_sub(1), Ordering::Relaxed);
            }
        }
        if token & COUNTED_SHARED != 0 {
            if let Some(counter) = self.shared.get(profile) {
                give_shared(counter);
            }
        }
    }
}

/// Takes one of `limit` slots on a counter several workers share.
fn take_shared(counter: &AtomicU32, limit: u32) -> bool {
    let mut c = counter.load(Ordering::Relaxed);
    loop {
        if c >= limit {
            return false;
        }
        match counter.compare_exchange_weak(c, c + 1, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(actual) => c = actual,
        }
    }
}

/// Returns a slot; never below zero, even if release ran twice for one
/// admission.
fn give_shared(counter: &AtomicU32) {
    let mut c = counter.load(Ordering::Relaxed);
    while c > 0 {
        match counter.compare_exchange_weak(c, c - 1, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => break,
            Err(actual) => c = actual,
        }
    }
}

impl Default for InFlightCap {
    fn default() -> Self {
        InFlightCap::new()
    }
}

/// Bit set in `Admitted::token` when the profile was counted on its shared counter.
const COUNTED_SHARED: u64 = 1;
/// Bit set when it was counted on the worker's share.
const COUNTED_SHARE: u64 = 2;
/// Bit set when it took a slot from its lane's reserve.
const COUNTED_ROW: u64 = 4;
/// Bit set when it took a slot from the pool.
const COUNTED_POOL: u64 = 8;
/// Where the reserve row sits in `Admitted::token`.
const ROW_SHIFT: u32 = 8;

impl Admit for InFlightCap {
    fn driver(&mut self, kind: DriverKind, connections: u32) {
        self.blocking_total = (kind == DriverKind::Blocking).then_some(connections);
    }

    fn reserved_percent(&self) -> u32 {
        self.reserves.iter().map(|(_, p)| u32::from(*p)).sum()
    }

    fn prepare(&mut self, workers: u16, profiles: &ProfileTable) {
        let workers = workers.max(1);
        let n = profiles.len();
        self.workers = workers;
        let w = usize::from(workers);
        let mut caps = Vec::with_capacity(n);
        let mut shares = vec![0u32; n * w];
        for (i, p) in profiles.iter().enumerate() {
            caps.push(match p.budget.max_in_flight {
                None => ProfileCap::None,
                Some(cap) if u64::from(cap) >= 4 * u64::from(workers) => {
                    // Exact shares: floor plus the remainder spread over the first workers.
                    let floor = cap / u32::from(workers);
                    let rest = cap % u32::from(workers);
                    for k in 0..w {
                        shares[i * w + k] = floor + u32::from((k as u32) < rest);
                    }
                    ProfileCap::Shares
                }
                Some(cap) => ProfileCap::Shared(cap),
            });
        }
        self.caps = caps.into_boxed_slice();
        self.shares = shares.into_boxed_slice();
        self.shared = (0..n).map(|_| AtomicU32::new(0)).collect();
        self.shards = (0..w)
            .map(|k| WorkerShard {
                in_flight: AtomicU32::new(0),
                profiles: (0..n).map(|_| AtomicU32::new(0)).collect(),
                lanes: self.reserves.iter().map(|_| AtomicU32::new(0)).collect(),
                pool: AtomicU32::new(0),
                jitter: AtomicU64::new(
                    0x9E37_79B9_7F4A_7C15 ^ (k as u64 + 1).wrapping_mul(0xBF58_476D_1CE4_E5B9),
                ),
            })
            .collect();
        self.prepare_reserves();
    }

    fn admit(&self, worker: WorkerId, env: &Envelope) -> Result<Admitted, Shed> {
        self.admit_at(worker.get(), env)
    }

    fn release(&self, worker: WorkerId, env: &Envelope, admitted: Admitted) {
        self.release_at(worker.get(), env, admitted);
    }
}

impl InFlightCap {
    /// `admit` for the worker with this index.
    pub(crate) fn admit_at(&self, worker: u16, env: &Envelope) -> Result<Admitted, Shed> {
        let Some(shard) = self.shards.get(usize::from(worker)) else {
            // Not prepared: fail closed.
            return Err(Shed {
                retry_after: Some(self.base),
                close: true,
            });
        };
        // Single writer: this worker only.
        let now = shard.in_flight.load(Ordering::Relaxed);
        if now >= self.per_worker {
            return Err(self.shed(shard));
        }
        let p = usize::from(env.profile().0);
        let mut token = 0;
        match self.caps.get(p).copied().unwrap_or(ProfileCap::None) {
            ProfileCap::None => {}
            ProfileCap::Shares => {
                let share = self.shares[p * usize::from(self.workers) + usize::from(worker)];
                let count = &shard.profiles[p];
                let c = count.load(Ordering::Relaxed);
                if c >= share {
                    return Err(self.shed(shard));
                }
                count.store(c + 1, Ordering::Relaxed);
                token |= COUNTED_SHARE;
            }
            ProfileCap::Shared(cap) => {
                if !take_shared(&self.shared[p], cap) {
                    return Err(self.shed(shard));
                }
                token |= COUNTED_SHARED;
            }
        }
        match self.take_lane(shard, env.lane()) {
            Some(bits) => token |= bits,
            None => {
                self.give_profile(shard, p, token);
                return Err(self.shed(shard));
            }
        }
        shard.in_flight.store(now + 1, Ordering::Relaxed);
        Ok(Admitted { token })
    }

    /// `release` for the worker with this index.
    pub(crate) fn release_at(&self, worker: u16, env: &Envelope, admitted: Admitted) {
        let Some(shard) = self.shards.get(usize::from(worker)) else {
            return;
        };
        let now = shard.in_flight.load(Ordering::Relaxed);
        shard
            .in_flight
            .store(now.saturating_sub(1), Ordering::Relaxed);
        self.give_profile(shard, usize::from(env.profile().0), admitted.token);
        let (pool, rows) = self.slot_counters(shard);
        if admitted.token & COUNTED_POOL != 0 {
            self.give_slot(pool);
        }
        if admitted.token & COUNTED_ROW != 0 {
            let row = usize::from((admitted.token >> ROW_SHIFT) as u8);
            if let Some(counter) = rows.get(row) {
                self.give_slot(counter);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Budget, KeepAlive, Limits, Profile, ProfileId};

    fn table(caps: &[Option<u32>]) -> ProfileTable {
        let profiles: Vec<Profile> = caps
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let b = match c {
                    Some(n) => Budget::new().max_in_flight(*n),
                    None => Budget::new(),
                };
                Profile::new(ProfileId(i as u8)).budget(b)
            })
            .collect();
        ProfileTable::compile(&profiles, Limits::new(), KeepAlive::new())
    }

    fn env(profile: u8) -> Envelope {
        let mut e = crate::envelope::tests::sample();
        e.profile = ProfileId(profile);
        e
    }

    fn on_lane(lane: u8) -> Envelope {
        let mut e = env(0);
        e.lane = Lane(lane);
        e
    }

    #[test]
    fn test_reserve_slots_keep_a_slot_for_other_lanes() {
        assert_eq!(reserve_slots(100, &[10, 20]), (vec![10, 20], 70));
        // Too small for the share: each keeps one, the pool keeps one.
        assert_eq!(reserve_slots(4, &[10, 10]), (vec![1, 1], 2));
        assert_eq!(reserve_slots(1, &[10]), (vec![0], 1));
        assert_eq!(reserve_slots(2, &[10, 10]), (vec![1, 0], 1));
        // All of it was asked for.
        assert_eq!(reserve_slots(10, &[100]), (vec![10], 0));
    }

    #[test]
    fn test_reserve_flood_cannot_take_a_lanes_slots() {
        let mut cap = InFlightCap::new().per_worker(10).reserve(Lane(0), 20);
        cap.prepare(1, &table(&[None]));
        // Ordinary traffic stops at the pool.
        let mut bulk = Vec::new();
        for _ in 0..8 {
            bulk.push(cap.admit_at(0, &on_lane(4)).unwrap());
        }
        assert!(cap.admit_at(0, &on_lane(4)).is_err());
        // The reserved lane still has its two slots, and no more with the pool full.
        let a = cap.admit_at(0, &on_lane(0)).unwrap();
        let b = cap.admit_at(0, &on_lane(0)).unwrap();
        assert!(cap.admit_at(0, &on_lane(0)).is_err());
        // A pool slot freed by ordinary traffic serves either lane.
        cap.release_at(0, &on_lane(4), bulk.pop().unwrap());
        let c = cap.admit_at(0, &on_lane(0)).unwrap();
        assert_eq!(c.token, COUNTED_POOL);
        cap.release_at(0, &on_lane(0), c);
        cap.release_at(0, &on_lane(0), a);
        cap.release_at(0, &on_lane(0), b);
        assert_eq!(cap.shards[0].pool.load(Ordering::Relaxed), 7);
        assert_eq!(cap.shards[0].lanes[0].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_reserve_counted_across_workers_on_the_blocking_driver() {
        let mut cap = InFlightCap::new().reserve(Lane(0), 25);
        cap.driver(DriverKind::Blocking, 4);
        cap.prepare(8, &table(&[None]));
        assert_eq!(cap.scope, ReserveScope::Server);
        let a = cap.admit_at(0, &on_lane(4)).unwrap();
        let b = cap.admit_at(1, &on_lane(4)).unwrap();
        let c = cap.admit_at(2, &on_lane(4)).unwrap();
        // The fourth worker is free, but only for the reserved lane.
        assert!(cap.admit_at(3, &on_lane(4)).is_err());
        let d = cap.admit_at(3, &on_lane(0)).unwrap();
        cap.release_at(3, &on_lane(0), d);
        cap.release_at(0, &on_lane(4), a);
        assert!(cap.admit_at(3, &on_lane(4)).is_ok());
        cap.release_at(1, &on_lane(4), b);
        cap.release_at(2, &on_lane(4), c);
    }

    #[test]
    fn test_reserve_refusal_gives_the_profile_slot_back() {
        let mut cap = InFlightCap::new().reserve(Lane(0), 100);
        cap.driver(DriverKind::Blocking, 2);
        cap.prepare(2, &table(&[Some(1)]));
        // Every slot is reserved, so another lane is refused and keeps nothing.
        assert!(cap.admit_at(0, &on_lane(4)).is_err());
        assert_eq!(cap.shared[0].load(Ordering::Relaxed), 0);
        assert!(cap.admit_at(0, &on_lane(0)).is_ok());
    }

    #[test]
    fn test_reserve_settings_and_clone() {
        let cap = InFlightCap::new()
            .reserve(Lane(0), 30)
            .reserve(Lane(1), 40)
            .reserve(Lane(0), 10);
        assert_eq!(cap.reserved_percent(), 50);
        assert_eq!(cap.clone().reserved_percent(), 50);
        assert_eq!(cap.reserve(Lane(1), 0).reserved_percent(), 10);
    }

    #[test]
    fn test_in_flight_cap_per_worker_boundary() {
        let mut cap = InFlightCap::new().per_worker(2);
        cap.prepare(1, &table(&[None]));
        let w = 0;
        let a = cap.admit_at(w, &env(0)).unwrap();
        let b = cap.admit_at(w, &env(0)).unwrap();
        let shed = cap.admit_at(w, &env(0)).unwrap_err();
        assert!(shed.retry_after.unwrap() >= Duration::from_secs(1));
        assert!(shed.retry_after.unwrap() < Duration::from_secs(2));
        cap.release_at(w, &env(0), a);
        assert!(cap.admit_at(w, &env(0)).is_ok());
        cap.release_at(w, &env(0), b);
    }

    #[test]
    fn test_in_flight_cap_small_profile_cap_is_shared_and_exact() {
        let mut cap = InFlightCap::new();
        cap.prepare(8, &table(&[Some(3)]));
        let mut held = Vec::new();
        for k in 0..3u16 {
            held.push((k, cap.admit_at(k, &env(0)).unwrap()));
        }
        // Any worker is refused once the profile is full.
        assert!(cap.admit_at(7, &env(0)).is_err());
        let (k, a) = held.pop().unwrap();
        cap.release_at(k, &env(0), a);
        assert!(cap.admit_at(7, &env(0)).is_ok());
    }

    #[test]
    fn test_in_flight_cap_large_profile_cap_exact_shares() {
        let mut cap = InFlightCap::new();
        cap.prepare(4, &table(&[Some(18)]));
        let mut total = 0;
        for k in 0..4u16 {
            while cap.admit_at(k, &env(0)).is_ok() {
                total += 1;
            }
        }
        assert_eq!(total, 18);
    }

    #[test]
    fn test_in_flight_cap_unprepared_fails_closed() {
        let cap = InFlightCap::new();
        assert!(cap.admit_at(0, &env(0)).is_err());
    }

    #[test]
    fn test_in_flight_cap_counters_return_to_zero() {
        let mut cap = InFlightCap::new().per_worker(10);
        cap.prepare(2, &table(&[Some(1), None]));
        let w = 1;
        let a = cap.admit_at(w, &env(0)).unwrap();
        let b = cap.admit_at(w, &env(1)).unwrap();
        cap.release_at(w, &env(0), a);
        cap.release_at(w, &env(1), b);
        assert_eq!(cap.shards[1].in_flight.load(Ordering::Relaxed), 0);
        assert_eq!(cap.shared[0].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_jitter_bounds_hold() {
        let mut cap = InFlightCap::new()
            .per_worker(1)
            .retry_after(Duration::from_secs(5));
        cap.prepare(1, &table(&[None]));
        let w = 0;
        let _held = cap.admit_at(w, &env(0)).unwrap();
        for _ in 0..1000 {
            let r = cap.admit_at(w, &env(0)).unwrap_err().retry_after.unwrap();
            assert!(
                r >= Duration::from_secs(5) && r < Duration::from_secs(10),
                "{r:?}"
            );
        }
    }
}
