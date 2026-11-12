//! A reply body is sent from where the service put it: a client that reads
//! slowly holds at most the transport's output budget, never a copy of the
//! body. A counting allocator measures the whole process, so this file
//! holds one test.

#![cfg(feature = "http1")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serve_net::{ListenerSpec, Server, Workers};

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

const SIZE: usize = 8 << 20;
static BODY: [u8; SIZE] = [b'x'; SIZE];
const CLIENTS: usize = 8;

/// Eight clients ask for an 8 MiB static body and read nothing. Each copied
/// the whole body into its connection's buffer (64 MiB live); now each holds
/// what the transport's output budget takes (256 KiB by default).
#[test]
fn test_stalled_clients_of_a_static_body_hold_no_copy_of_it() {
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .workers(Workers::reactor(1))
        .serve(serve_net::handler(|_req, reply| {
            reply.status(200).body_static(&BODY);
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    // Warm the worker's pools with one whole exchange.
    let mut warm = TcpStream::connect(addr).unwrap();
    warm.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    warm.write_all(b"GET / HTTP/1.1\r\nHost: a\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut got = Vec::new();
    warm.read_to_end(&mut got).unwrap();
    assert!(got.len() > SIZE, "{}", got.len());
    drop(got);
    std::thread::sleep(Duration::from_millis(200));
    let before = LIVE.load(Ordering::Relaxed);
    let socks: Vec<TcpStream> = (0..CLIENTS)
        .map(|_| {
            let mut s = TcpStream::connect(addr).unwrap();
            s.write_all(b"GET / HTTP/1.1\r\nHost: a\r\n\r\n").unwrap();
            s
        })
        .collect();
    std::thread::sleep(Duration::from_secs(1));
    let grew = LIVE.load(Ordering::Relaxed).saturating_sub(before);
    // Under one body for all eight; each copy alone was a whole body.
    assert!(
        grew < SIZE,
        "{CLIENTS} stalled clients grew the live heap by {grew} bytes"
    );
    drop(socks);
    let _ = server.drain(Duration::from_secs(2));
}
