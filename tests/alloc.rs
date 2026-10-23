//! NFR-2 and P-14: no allocation per request in steady state, proved with a
//! counting allocator. Counts are per thread, so tests running in parallel do
//! not disturb each other.

#![cfg(feature = "http1")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use serve_net::testing::TestConnection;
use serve_net::{KeepAlive, Profile, ProfileId};

struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

// SAFETY: every method forwards to the system allocator with the caller's
// arguments; the counter is a thread-local `Cell` that allocates nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

/// Sends `input` `warm` times, then `n` times, and returns the allocations of
/// the last `n`.
fn measure<S: serve_net::HttpService>(conn: &mut TestConnection<S>, input: &[u8], n: usize) -> u64 {
    let mut out = Vec::with_capacity(64 * 1024);
    for _ in 0..200 {
        out.clear();
        conn.send_into(input, &mut out);
        assert!(
            out.starts_with(b"HTTP/1.1 "),
            "{:?}",
            String::from_utf8_lossy(&out)
        );
    }
    let before = allocations();
    for _ in 0..n {
        out.clear();
        conn.send_into(input, &mut out);
    }
    let made = allocations() - before;
    assert!(conn.is_open());
    made
}

/// A connection that keeps alive for as many requests as a test sends.
fn connection() -> TestConnection<impl serve_net::HttpService> {
    let profile = Profile::new(ProfileId(0)).keep_alive(KeepAlive::new().max_requests(u32::MAX));
    TestConnection::with_profile(site(), profile)
}

fn site() -> impl serve_net::HttpService {
    struct Site;
    impl serve_net::HttpService for Site {
        fn head(
            &self,
            h: &serve_net::HttpHead<'_>,
            _e: &mut serve_net::Envelope,
        ) -> serve_net::HeadAction {
            if h.path() == "/no" {
                serve_net::HeadAction::refuse(403)
            } else {
                serve_net::HeadAction::Proceed
            }
        }
        fn call(
            &self,
            req: &serve_net::HttpRequest<'_>,
            _e: &serve_net::Envelope,
            reply: &mut serve_net::HttpReply,
        ) -> serve_net::Outcome {
            respond(req, reply);
            serve_net::Outcome::Done
        }
    }
    fn respond(req: &serve_net::HttpRequest<'_>, reply: &mut serve_net::HttpReply) {
        if req.body_bytes().is_empty() {
            reply
                .status(200)
                .header("content-type", "text/plain")
                .body_static(b"hello world");
        } else {
            reply.status(201).body(&req.body_bytes()[..16]);
        }
    }
    Site
}

#[test]
fn test_no_allocation_per_get_after_warm_up() {
    let mut conn = connection();
    let input =
        b"GET /index.html HTTP/1.1\r\nHost: example.com\r\nAccept: */*\r\nUser-Agent: test\r\n\r\n";
    assert_eq!(
        measure(&mut conn, input, 10_000),
        0,
        "allocations during 10,000 keep-alive GETs"
    );
}

#[test]
fn test_no_allocation_per_post_with_buffered_body() {
    let mut conn = connection();
    let mut input =
        b"POST /form HTTP/1.1\r\nHost: example.com\r\nContent-Length: 4096\r\n\r\n".to_vec();
    input.extend_from_slice(&[b'x'; 4096]);
    assert_eq!(
        measure(&mut conn, &input, 5_000),
        0,
        "allocations during 5,000 POSTs with a 4 KiB body"
    );
}

#[test]
fn test_no_allocation_per_pipelined_batch() {
    let mut conn = connection();
    let one = b"GET / HTTP/1.1\r\nHost: a\r\n\r\n";
    let input: Vec<u8> = one.iter().copied().cycle().take(one.len() * 8).collect();
    assert_eq!(
        measure(&mut conn, &input, 2_000),
        0,
        "allocations during 2,000 batches of 8 pipelined requests"
    );
}

#[test]
fn test_refusal_allocates_nothing_after_warm_up() {
    // A refusal closes its connection, so it is measured once, on a warm one.
    let mut conn = connection();
    let input = b"GET / HTTP/1.1\r\nHost: a\r\n\r\n";
    let _ = measure(&mut conn, input, 10);
    let mut out = Vec::with_capacity(4096);
    let before = allocations();
    conn.send_into(b"GET /no HTTP/1.1\r\nHost: a\r\n\r\n", &mut out);
    let made = allocations() - before;
    assert!(
        out.starts_with(b"HTTP/1.1 403 "),
        "{:?}",
        String::from_utf8_lossy(&out)
    );
    assert!(!conn.is_open());
    assert_eq!(made, 0, "the refusal path allocated");
}
