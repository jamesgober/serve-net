//! The 0.5.0 exit: 10,000 idle keep-alive connections with bounded memory.
//!
//! Ignored by default: it holds 20,000 sockets, so it needs the descriptors
//! for them (`ulimit -n 30000`). `SERVE_NET_IDLE` sets the count. CI runs it.

#![cfg(feature = "http1")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::net::TcpStream;
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

const GET: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";

/// The most a connection may hold while idle, excluding kernel buffers. The
/// target (NFR-3) is 4 KiB, due at 0.6; D-076 has where the rest goes.
const MAX_BYTES_PER_IDLE: usize = 12 * 1024;

fn one_request(s: &mut TcpStream) {
    s.write_all(GET).unwrap();
    let mut buf = [0u8; 512];
    let n = s.read(&mut buf).unwrap();
    assert!(buf[..n].starts_with(b"HTTP/1.1 200"));
}

#[test]
#[ignore = "needs 20,000 descriptors"]
fn test_ten_thousand_idle_connections_stay_bounded() {
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
    std::thread::sleep(Duration::from_secs(1));
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
