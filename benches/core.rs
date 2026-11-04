//! Hot-path benchmarks (dev/PLAN.md section 10): the keep-alive request path
//! with and without the forwarded walk, with `InFlightCap` profile caps, the
//! snapshot reader, and the control check. Run with `cargo bench`; a regression over 5% against the stored
//! baseline blocks a change.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use serve_net::testing::TestConnection;
use serve_net::{
    Budget, ControlLine, KeepAlive, Profile, ProfileId, ProxySource, SimpleControl, Snapshot,
    TrustedProxies,
};

const GET: &[u8] =
    b"GET /index.html HTTP/1.1\r\nHost: example.com\r\nUser-Agent: bench\r\nAccept: */*\r\n\r\n";
const GET_XFF: &[u8] =
    b"GET /index.html HTTP/1.1\r\nHost: example.com\r\nX-Forwarded-For: 198.51.100.7, 203.0.113.9, 127.0.0.1\r\nAccept: */*\r\n\r\n";

fn site() -> impl serve_net::HttpService {
    serve_net::handler(|_req, reply| {
        reply
            .status(200)
            .header("content-type", "text/plain")
            .body_static(b"hello world");
    })
}

fn request_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("request");
    group.throughput(Throughput::Elements(1));
    let forever = Profile::new(ProfileId(0)).keep_alive(KeepAlive::new().max_requests(u32::MAX));
    let mut conn = TestConnection::with_profile(site(), forever.clone());
    let mut out = Vec::with_capacity(64 * 1024);
    group.bench_function("keep_alive_get", |b| {
        b.iter(|| {
            out.clear();
            conn.send_into(black_box(GET), &mut out);
        });
    });
    group.bench_function("keep_alive_get_xff_header_direct", |b| {
        b.iter(|| {
            out.clear();
            conn.send_into(black_box(GET_XFF), &mut out);
        });
    });
    let proxy = ProxySource::forwarded_for(TrustedProxies::loopback());
    let mut behind = TestConnection::behind(site(), proxy, forever.clone());
    group.bench_function("keep_alive_get_xff_3_entries", |b| {
        b.iter(|| {
            out.clear();
            behind.send_into(black_box(GET_XFF), &mut out);
        });
    });
    assert!(
        conn.is_open() && behind.is_open(),
        "a benchmark connection closed"
    );
    // InFlightCap: a profile cap of at least 4 per worker is split into exact
    // per-worker shares (single writer); a smaller one is one shared counter
    // (a compare-and-swap per admission).
    let capped = |n| forever.clone().budget(Budget::new().max_in_flight(n));
    let mut shares = TestConnection::with_profile(site(), capped(64));
    group.bench_function("keep_alive_get_in_flight_shares", |b| {
        b.iter(|| {
            out.clear();
            shares.send_into(black_box(GET), &mut out);
        });
    });
    let mut shared = TestConnection::with_profile(site(), capped(2));
    group.bench_function("keep_alive_get_in_flight_shared_counter", |b| {
        b.iter(|| {
            out.clear();
            shared.send_into(black_box(GET), &mut out);
        });
    });
    assert!(
        shares.is_open() && shared.is_open(),
        "a benchmark connection closed"
    );
    group.finish();
}

/// NFR-1 by subtraction: http-net's `ServerConn` alone, parsing the same
/// request and writing the same reply with a cached `Date`, the calls
/// serve-net's HTTP engine makes. `request/keep_alive_get` less this is
/// serve-net's own work per request (with the test harness's copy of the
/// output, so an upper bound).
fn engine_alone(c: &mut Criterion) {
    use http_net::{OutgoingBody, ResponseHeadRef, ServerConn, ServerEvent};
    let mut group = c.benchmark_group("engine");
    group.throughput(Throughput::Elements(1));
    let mut conn = ServerConn::new();
    let mut dates = http_net::DateCache::new();
    let mut out: Vec<u8> = Vec::with_capacity(64 * 1024);
    let fields: [(&str, &[u8]); 1] = [("content-type", b"text/plain")];
    group.bench_function("http_net_alone_keep_alive_get", |b| {
        b.iter(|| {
            out.clear();
            let n = conn.feed(black_box(GET));
            assert_eq!(n, GET.len());
            loop {
                match conn.poll() {
                    Ok(ServerEvent::AwaitingResponse) => {
                        let head = ResponseHeadRef::new(200, &fields, OutgoingBody::Full(11));
                        let date = dates.update(1_762_000_000);
                        if let Ok(http_net::BodyPlan::Raw) =
                            conn.write_head(&head, Some(date), &mut out)
                        {
                            out.extend_from_slice(b"hello world");
                        }
                    }
                    Ok(ServerEvent::NeedInput) => break,
                    Ok(_) => {}
                    Err(e) => panic!("{e:?}"),
                }
            }
            black_box(out.len())
        });
    });
    // Idea I-3: what feeding the engine from net-io's buffer costs, the one
    // copy of each input byte, for this head and for a 4 KiB body.
    let mut buf: Vec<u8> = Vec::with_capacity(16 * 1024);
    group.bench_function("input_copy_head", |b| {
        b.iter(|| {
            buf.clear();
            buf.extend_from_slice(black_box(GET));
            black_box(buf.len())
        });
    });
    let body = vec![b'x'; 4096];
    group.bench_function("input_copy_4k", |b| {
        b.iter(|| {
            buf.clear();
            buf.extend_from_slice(black_box(&body));
            black_box(buf.len())
        });
    });
    group.finish();
}

fn shared_state(c: &mut Criterion) {
    let table = Snapshot::new(vec![1u64; 16]);
    let mut reader = table.reader();
    c.bench_function("snapshot_reader_get_unchanged", |b| {
        b.iter(|| black_box(reader.get(&table).len()));
    });
    let line = SimpleControl::new();
    let seen = line.epoch();
    c.bench_function("control_checkpoint_unchanged", |b| {
        b.iter(|| black_box(line.epoch() == seen));
    });
}

criterion_group!(benches, request_path, engine_alone, shared_state);
criterion_main!(benches);
