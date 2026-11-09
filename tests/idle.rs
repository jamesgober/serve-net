//! The 0.5.0 exit: 10,000 idle keep-alive connections with bounded memory.
//!
//! Ignored by default: it holds 20,000 sockets, so it needs the descriptors
//! for them (`ulimit -n 30000`). `SERVE_NET_IDLE` sets the count. CI runs it.

#![cfg(feature = "http1")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serve_net::{KeepAlive, Limits, ListenerSpec, Server, Workers};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator; the counter does not allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller's layout is passed on unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: `ptr` came from `alloc` above with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// `LIVE` counts the whole process, so the tests here run one at a time: run
/// together, one test's connections and replies were counted in another's
/// measurement (Linux CI measured 1,429 to 4,214 bytes per idle connection
/// for the same code).
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn alone() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

const GET: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";

/// The most a connection may hold while idle, excluding kernel buffers. The
/// target (NFR-3) is 4 KiB, due at 0.6; D-076 has where the rest goes.
/// NFR-3's 4 KiB. On Windows net-io's completion-port bookkeeping adds about
/// 150 bytes a connection that serve-net cannot shed: 4.25 KiB there, in the
/// register until it closes (D-094).
#[cfg(not(windows))]
const MAX_BYTES_PER_IDLE: usize = 4 * 1024;
#[cfg(windows)]
const MAX_BYTES_PER_IDLE: usize = 4 * 1024 + 256;

fn one_request(s: &mut TcpStream) {
    s.write_all(GET).unwrap();
    let mut buf = [0u8; 512];
    let n = s.read(&mut buf).unwrap();
    assert!(buf[..n].starts_with(b"HTTP/1.1 200"));
}

#[test]
#[ignore = "needs 20,000 descriptors"]
fn test_ten_thousand_idle_connections_stay_bounded() {
    let _alone = alone();
    let count: usize = std::env::var("SERVE_NET_IDLE").map_or(10_000, |n| n.parse().unwrap());
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .workers(Workers::reactor(0))
        .limits(Limits::new().max_connections(u32::try_from(count).unwrap() + 64))
        .keep_alive(
            KeepAlive::new()
                .idle(Duration::from_secs(300))
                .max_requests(1000),
        )
        .transport_limits(
            net_io::Limits::builder()
                .max_per_source(
                    u32::try_from(count).unwrap() + 64,
                    net_io::PrefixPolicy::DEFAULT,
                )
                .build()
                .unwrap(),
        )
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();

    // Warm the pools, so the baseline holds what a server keeps anyway.
    for _ in 0..64 {
        let mut s = TcpStream::connect(addr).unwrap();
        one_request(&mut s);
    }
    std::thread::sleep(Duration::from_millis(200));
    let before = LIVE.load(Ordering::Relaxed);

    let mut held = Vec::with_capacity(count);
    let started = Instant::now();
    for _ in 0..count {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        one_request(&mut s);
        held.push(s);
    }
    // Past the idle trim (one second of quiet), so what is measured is what
    // an idle connection keeps.
    std::thread::sleep(Duration::from_secs(2));
    let vec_bytes = held.capacity() * size_of::<TcpStream>();
    let used = LIVE.load(Ordering::Relaxed) - before - vec_bytes;
    let per = used / count;
    eprintln!(
        "{count} idle connections: {} MiB, {per} bytes each, opened in {:?}",
        used >> 20,
        started.elapsed()
    );
    assert_eq!(server.stats().transport.open, count as u64);

    // Every one of them still answers.
    for s in held.iter_mut().step_by(97) {
        one_request(s);
    }
    assert!(per <= MAX_BYTES_PER_IDLE, "{per} bytes per idle connection");
    drop(held);
    let _ = server.drain(Duration::from_secs(5));
}

/// NFR-19, the leak test: 100,000 connections opened, answered, and closed
/// (the server closes, so client ports are not held in TIME_WAIT); the heap
/// comes back to where it was. A seat, an admission slot, a deferral slot, an
/// observer entry, or a buffer that one connection kept would show as growth.
/// Linux only: macOS and Windows have about 16,000 ephemeral ports.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "opens 100,000 connections"]
fn test_hundred_thousand_connections_leak_nothing() {
    let _alone = alone();
    const CLOSE: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";
    let count: usize = std::env::var("SERVE_NET_LEAK").map_or(100_000, |n| n.parse().unwrap());
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        // Two workers: their buffer pools fill in the warm-up, so what the
        // halves measure is what connections keep, not pools still filling.
        .workers(Workers::reactor(2))
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    let round = |n: usize| {
        for _ in 0..n {
            let mut s = TcpStream::connect(addr).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            s.write_all(CLOSE).unwrap();
            let mut got = Vec::new();
            let _ = s.read_to_end(&mut got);
            assert!(
                got.starts_with(b"HTTP/1.1 200"),
                "{:?}",
                String::from_utf8_lossy(&got)
            );
        }
    };
    // Warm the pools and the per-worker caches, then measure.
    round(5_000);
    let settle = || {
        let mut last = LIVE.load(Ordering::Relaxed);
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            let now = LIVE.load(Ordering::Relaxed);
            if now == last && server.stats().transport.open == 0 {
                break;
            }
            last = now;
        }
        LIVE.load(Ordering::Relaxed)
    };
    // Two equal halves: pools and caches may grow in the first, a leak grows
    // in both, so the second half is what has to stay flat.
    let before = settle();
    let started = Instant::now();
    round(count / 2);
    let mid = settle();
    round(count / 2);
    let after = settle();
    let first = mid.saturating_sub(before);
    let second = after.saturating_sub(mid);
    eprintln!(
        "{count} connections in {:?}: live heap {before} -> {mid} -> {after} bytes (+{first}, then +{second})",
        started.elapsed()
    );
    assert_eq!(server.stats().transport.open, 0);
    // A byte per connection in the second half would be 50 KB at the default
    // count: allow pool churn, nothing that scales with the count.
    assert!(
        second < 32 * 1024,
        "{second} bytes more over the second {} connections",
        count / 2
    );
    let _ = server.drain(Duration::from_secs(2));
}

/// T-33: large replies on many connections at once. Buffers come from pools
/// and go back to them; the live heap must not grow from one round to the
/// next. net-io 0.6.0 charged its buffer budget anew on every flush of a
/// reply larger than one buffer and never took it back: this would grow.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "moves about 1 GB over loopback"]
fn test_t33_large_replies_on_many_connections_stay_bounded() {
    let _alone = alone();
    static BIG: [u8; 256 * 1024] = [b'x'; 256 * 1024];
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .workers(Workers::reactor(2))
        .keep_alive(KeepAlive::new().max_requests(u32::MAX))
        .serve(serve_net::handler(|_req, reply| {
            reply.status(200).body_static(&BIG);
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    let mut conns: Vec<TcpStream> = (0..64)
        .map(|_| {
            let s = TcpStream::connect(addr).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
            s
        })
        .collect();
    let mut round = || {
        std::thread::scope(|scope| {
            for s in conns.iter_mut() {
                scope.spawn(move || {
                    for _ in 0..8 {
                        s.write_all(GET).unwrap();
                        let mut got = 0usize;
                        let mut head = Vec::new();
                        let mut buf = vec![0u8; 64 * 1024];
                        // Read the head, then the 256 KiB body.
                        loop {
                            let n = s.read(&mut buf).unwrap();
                            assert!(n > 0, "closed");
                            if head.len() < 4096 && got == 0 {
                                head.extend_from_slice(&buf[..n]);
                                if let Some(end) = head.windows(4).position(|w| w == b"\r\n\r\n") {
                                    got = head.len() - (end + 4);
                                }
                            } else {
                                got += n;
                            }
                            if got >= BIG.len() {
                                break;
                            }
                        }
                    }
                });
            }
        });
    };
    // Past the idle trim (one second), so both measurements see connections
    // that gave their reply buffers back.
    let settle = || {
        std::thread::sleep(Duration::from_secs(2));
        LIVE.load(Ordering::Relaxed)
    };
    for _ in 0..2 {
        round();
    }
    let warm = settle();
    for _ in 0..3 {
        round();
    }
    let after = settle();
    let grew = after.saturating_sub(warm);
    eprintln!("64 connections, 256 KiB replies: live heap {warm} -> {after} bytes (+{grew})");
    // Pools may take a buffer or two more; growth with the rounds is a leak.
    assert!(
        grew < 1024 * 1024,
        "{grew} bytes more after three more rounds"
    );
    drop(conns);
    let _ = server.drain(Duration::from_secs(2));
}
