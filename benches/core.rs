//! Hot-path benchmarks (dev/PLAN.md section 10): the keep-alive request path
//! with and without the forwarded walk, the snapshot reader, and the control
//! check. Run with `cargo bench`; a regression over 5% against the stored
//! baseline blocks a change.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use serve_net::testing::TestConnection;
use serve_net::{
    ControlLine, KeepAlive, Profile, ProfileId, ProxySource, SimpleControl, Snapshot,
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
    let mut behind = TestConnection::behind(site(), proxy, forever);
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

criterion_group!(benches, request_path, shared_state);
criterion_main!(benches);
