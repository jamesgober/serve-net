//! Real sockets on 127.0.0.1: the Tier 1 server end to end.

#![cfg(feature = "http1")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

fn start() -> serve_net::Running {
    serve_net::http("127.0.0.1:0", |req, reply| match req.path() {
        "/health" => {
            reply.text(200, "ok");
        }
        _ => {
            reply.text(404, "not found");
        }
    })
    .unwrap()
}

/// Sends `request` and reads one response with a `Content-Length` body.
fn exchange(stream: &mut TcpStream, request: &[u8]) -> String {
    stream.write_all(request).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).expect("read timed out");
        assert!(n > 0, "closed before a full response: {got:?}");
        got.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&got);
        if let Some(end) = text.find("\r\n\r\n") {
            let len = text[..end]
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if got.len() >= end + 4 + len {
                return text.into_owned();
            }
        }
    }
}

fn connect(addr: SocketAddr) -> TcpStream {
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s
}

#[test]
fn test_loopback_routes_and_keep_alive() {
    let server = start();
    let addr = server.local_addr().unwrap();
    let mut c = connect(addr);
    let a = exchange(&mut c, b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(a.starts_with("HTTP/1.1 200"), "{a}");
    assert!(a.ends_with("ok"), "{a}");
    // The same connection again: keep-alive.
    let b = exchange(&mut c, b"GET /missing HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(b.starts_with("HTTP/1.1 404"), "{b}");
    drop(c);
    let report = server.drain(Duration::from_secs(2));
    assert_eq!(report.killed, 0);
}

#[test]
fn test_loopback_pipelined_requests_both_answered() {
    let server = start();
    let addr = server.local_addr().unwrap();
    let mut c = connect(addr);
    c.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\nGET /other HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    while String::from_utf8_lossy(&got).matches("HTTP/1.1 ").count() < 2
        || !String::from_utf8_lossy(&got).ends_with("not found")
    {
        let n = c.read(&mut buf).expect("read timed out");
        assert!(n > 0, "closed early: {:?}", String::from_utf8_lossy(&got));
        got.extend_from_slice(&buf[..n]);
    }
    let text = String::from_utf8_lossy(&got);
    let first = text.find("HTTP/1.1 200").unwrap();
    let second = text.find("HTTP/1.1 404").unwrap();
    assert!(first < second, "{text}");
}

#[test]
fn test_loopback_sequential_connections() {
    let server = start();
    let addr = server.local_addr().unwrap();
    for i in 0..20 {
        let mut c = connect(addr);
        let a = exchange(
            &mut c,
            b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(a.starts_with("HTTP/1.1 200"), "connection {i}: {a}");
    }
    let report = server.drain(Duration::from_secs(2));
    assert_eq!(report.killed, 0);
}

#[test]
fn test_loopback_drain_handle_ends_wait() {
    let server = start();
    let addr = server.local_addr().unwrap();
    let mut idle = connect(addr);
    let a = exchange(&mut idle, b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(a.starts_with("HTTP/1.1 200"), "{a}");
    let handle = server.drain_handle();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        handle.drain(Duration::from_secs(2));
    });
    let report = server.wait();
    stopper.join().unwrap();
    assert_eq!(report.killed, 0, "{report:?}");
    // The idle keep-alive connection was closed by the drain.
    let mut buf = [0u8; 16];
    assert_eq!(idle.read(&mut buf).unwrap_or(0), 0);
}

#[test]
fn test_loopback_behind_proxy_many_connections_from_one_peer() {
    use serve_net::{Ingress, ListenerSpec, ProxySource, Server, TrustedProxies};
    let server = Server::builder()
        .listener(
            ListenerSpec::tcp("site", "127.0.0.1:0").ingress(Ingress::Proxy(
                ProxySource::forwarded_for(TrustedProxies::loopback()),
            )),
        )
        .serve(serve_net::handler(|req, reply| {
            let client = req
                .client()
                .addr()
                .map(|a| a.to_string())
                .unwrap_or_default();
            reply.text(200, &client);
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    // More connections at once than net-io's per-source default (16 on the
    // blocking driver): the proxy is one peer for every client.
    let mut conns: Vec<TcpStream> = (0..24).map(|_| connect(addr)).collect();
    for (i, c) in conns.iter_mut().enumerate() {
        let req = format!("GET / HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 192.0.2.{i}\r\n\r\n");
        let a = exchange(c, req.as_bytes());
        assert!(a.starts_with("HTTP/1.1 200"), "connection {i}: {a}");
        assert!(a.ends_with(&format!("192.0.2.{i}")), "{a}");
    }
    drop(conns);
    let report = server.drain(Duration::from_secs(2));
    assert_eq!(report.killed, 0);
}

#[test]
fn test_loopback_mixed_server_keeps_per_source_cap_on_direct_listener() {
    use serve_net::{Ingress, ListenerSpec, ProxySource, Server, TrustedProxies};
    let server = Server::builder()
        .listener(
            ListenerSpec::tcp("site", "127.0.0.1:0").ingress(Ingress::Proxy(
                ProxySource::forwarded_for(TrustedProxies::loopback()),
            )),
        )
        .listener(ListenerSpec::tcp("direct", "127.0.0.1:0"))
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    let listeners = server.listeners();
    let proxied = listeners[0].local.socket_addr().unwrap();
    let direct = listeners[1].local.socket_addr().unwrap();
    // The proxy listener takes more than the per-source default from one peer.
    let mut through: Vec<TcpStream> = (0..20).map(|_| connect(proxied)).collect();
    for (i, c) in through.iter_mut().enumerate() {
        let a = exchange(
            c,
            b"GET / HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 192.0.2.1\r\n\r\n",
        );
        assert!(a.starts_with("HTTP/1.1 200"), "proxied {i}: {a}");
    }
    // The direct listener still caps one address at net-io's default (16 on
    // the blocking driver): the 17th connection is not served.
    let mut held: Vec<TcpStream> = (0..16).map(|_| connect(direct)).collect();
    for (i, c) in held.iter_mut().enumerate() {
        let a = exchange(c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(a.starts_with("HTTP/1.1 200"), "direct {i}: {a}");
    }
    let mut extra = connect(direct);
    let _ = extra.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    let mut buf = [0u8; 64];
    let served = matches!(extra.read(&mut buf), Ok(n) if n > 0);
    assert!(!served, "a 17th connection from one address was served");
    drop((through, held, extra));
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_loopback_bind_conflict_names_listener() {
    let first = start();
    let addr = first.local_addr().unwrap();
    let err = serve_net::http(addr, |_req, reply| {
        reply.text(200, "x");
    })
    .unwrap_err();
    assert!(
        matches!(&err, serve_net::ServeError::Bind { listener, .. } if listener == "http-0"),
        "{err}"
    );
}

#[test]
fn test_loopback_http_behind_proxy_takes_forwarded_client() {
    let server = serve_net::http_behind_proxy("127.0.0.1:0", "127.0.0.1, ::1", |req, reply| {
        let client = req
            .client()
            .addr()
            .map(|a| a.to_string())
            .unwrap_or_default();
        reply.text(200, &client);
    })
    .unwrap();
    let addr = server.local_addr().unwrap();
    let mut c = connect(addr);
    let a = exchange(
        &mut c,
        b"GET / HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 203.0.113.7\r\n\r\n",
    );
    assert!(a.ends_with("203.0.113.7"), "{a}");
    // A trusted peer without the header is refused.
    let mut c2 = connect(addr);
    let b = exchange(&mut c2, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(b.starts_with("HTTP/1.1 400"), "{b}");
    let bad = serve_net::http_behind_proxy("127.0.0.1:0", "not an address", |_req, reply| {
        reply.text(200, "x");
    })
    .unwrap_err();
    assert!(
        matches!(&bad, serve_net::ServeError::Config(e) if e.has(serve_net::RuleId::TrustParse)),
        "{bad}"
    );
}

#[test]
fn test_loopback_stats_health_listeners() {
    let server = start();
    let addr = server.local_addr().unwrap();
    let mut c = connect(addr);
    for _ in 0..3 {
        let a = exchange(&mut c, b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(a.starts_with("HTTP/1.1 200"), "{a}");
    }
    let a = exchange(&mut c, b"GET /x HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(a.starts_with("HTTP/1.1 404"), "{a}");
    let stats = server.stats();
    assert_eq!(stats.requests, 4, "{stats:?}");
    assert_eq!(stats.replied, 4);
    assert_eq!(stats.refused, 0);
    assert!(stats.bytes_in > 0 && stats.bytes_out > 0);
    assert_eq!(stats.transport.accepted, 1);
    assert!(server.health().is_ok(), "{:?}", server.health());
    let listeners = server.listeners();
    assert_eq!(listeners.len(), 1);
    assert_eq!(listeners[0].name, "http-0");
    assert_eq!(listeners[0].state, serve_net::ListenerState::Accepting);
    assert_eq!(listeners[0].ingress, serve_net::IngressKind::Direct);
    server.close_door();
    assert_eq!(
        server.listeners()[0].state,
        serve_net::ListenerState::DoorClosed
    );
    drop(c);
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_loopback_malformed_request_refused_and_counted() {
    let server = start();
    let addr = server.local_addr().unwrap();
    let mut c = connect(addr);
    let a = exchange(
        &mut c,
        b"GET / HTTP/1.1\r\nHost: x\r\nContent-Length: nope\r\n\r\n",
    );
    assert!(a.starts_with("HTTP/1.1 400"), "{a}");
    let stats = server.stats();
    assert_eq!(stats.refused, 1, "{stats:?}");
    assert_eq!(
        stats.refused_for(serve_net::engine::RefusalKind::Malformed),
        1
    );
    let _ = server.drain(Duration::from_secs(2));
}

/// Denies every write after the first response of a connection.
struct DenySecond;

impl serve_net::Gate for DenySecond {
    type Ticket = u32;
    fn admit(
        &self,
        _info: &serve_net::net_io::ConnectionInfo,
        _stage: serve_net::net_io::AdmitStage,
    ) -> Result<serve_net::net_io::Admitted<u32>, serve_net::net_io::Refusal> {
        Ok(serve_net::net_io::Admitted {
            lane: serve_net::Lane(0),
            ticket: 0,
            tag: serve_net::net_io::GateTag(0),
        })
    }
    fn readmit(
        &self,
        _ticket: &mut u32,
        _info: &serve_net::net_io::ConnectionInfo,
    ) -> Result<(), serve_net::net_io::Refusal> {
        Ok(())
    }
    fn check(
        &self,
        ticket: &mut u32,
        write: &serve_net::net_io::WriteCheck,
    ) -> Result<(), serve_net::net_io::Denial> {
        if !write.response_started {
            *ticket += 1;
        }
        if *ticket >= 2 {
            Err(serve_net::net_io::Denial { code: 429 })
        } else {
            Ok(())
        }
    }
}

#[test]
fn test_loopback_gate_denial_writes_refusal_on_second_reply() {
    use serve_net::{ListenerSpec, Server};
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .gate(DenySecond)
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    let mut c = connect(addr);
    let a = exchange(&mut c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(a.starts_with("HTTP/1.1 200"), "{a}");
    // The second reply is denied before its first byte: the refusal goes out
    // in its place (the per-reply boundary) and the connection closes.
    let b = exchange(&mut c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(b.starts_with("HTTP/1.1 429"), "{b}");
    let _ = server.drain(Duration::from_secs(2));
}

#[derive(Default)]
struct Seen {
    accepted: std::sync::atomic::AtomicU32,
    ends: std::sync::Mutex<Vec<serve_net::EndReason>>,
}

struct Sink(std::sync::Arc<Seen>);

impl serve_net::EventSink for Sink {
    fn event(&self, event: &serve_net::net_io::Event) {
        if matches!(event, serve_net::net_io::Event::Accepted { .. }) {
            self.0
                .accepted
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

struct Ends(std::sync::Arc<Seen>);

impl serve_net::Observer for Ends {
    fn connection_ended(&self, _c: serve_net::ConnectionId, end: serve_net::EndReason, _n: u32) {
        self.0.ends.lock().unwrap().push(end);
    }
}

#[test]
fn test_loopback_events_and_transport_end_reason() {
    use serve_net::{KeepAlive, ListenerSpec, Server};
    let seen = std::sync::Arc::new(Seen::default());
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .keep_alive(KeepAlive::new().idle(Duration::from_millis(200)))
        .events(Sink(std::sync::Arc::clone(&seen)))
        .observer(Ends(std::sync::Arc::clone(&seen)))
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    let mut c = connect(addr);
    let a = exchange(&mut c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(a.starts_with("HTTP/1.1 200"), "{a}");
    // Idle past the keep-alive timeout: net-io closes it, and the end reason
    // is net-io's.
    let mut buf = [0u8; 16];
    assert_eq!(c.read(&mut buf).unwrap_or(0), 0);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while seen.ends.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(seen.accepted.load(std::sync::atomic::Ordering::Relaxed), 1);
    let ends = seen.ends.lock().unwrap().clone();
    assert_eq!(
        ends,
        vec![serve_net::EndReason::Transport(
            serve_net::CloseReason::Idle
        )]
    );
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_loopback_drain_with_slow_request_in_flight() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let started = std::sync::Arc::new(AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&started);
    let server = serve_net::http("127.0.0.1:0", move |_req, reply| {
        flag.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_millis(300));
        reply.text(200, "slow");
    })
    .unwrap();
    let addr = server.local_addr().unwrap();
    let client = std::thread::spawn(move || {
        let mut c = connect(addr);
        exchange(&mut c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
    });
    while !started.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let report = server.drain(Duration::from_secs(5));
    let reply = client.join().unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(
        reply.to_ascii_lowercase().contains("connection: close"),
        "{reply}"
    );
    assert_eq!(report.killed, 0, "{report:?}");
    assert_eq!(report.requests_finished, 1, "{report:?}");
}

/// NFR-19 in miniature: connections opened and closed in rounds at the soft
/// cap. A seat, an admission slot, or a connection that leaked would show as a
/// 503 in a later round or as an open connection at the end.
#[test]
fn test_loopback_rounds_at_the_soft_cap_leak_nothing() {
    use serve_net::{Limits, ListenerSpec, Server};
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .limits(Limits::new().max_connections(8))
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    for round in 0..5 {
        let mut conns: Vec<TcpStream> = (0..8).map(|_| connect(addr)).collect();
        for (i, c) in conns.iter_mut().enumerate() {
            let a = exchange(c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
            assert!(
                a.starts_with("HTTP/1.1 200"),
                "round {round}, connection {i}: {a}"
            );
        }
        // One over the cap gets the busy refusal, not a reset.
        let mut extra = connect(addr);
        let busy = exchange(&mut extra, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(busy.starts_with("HTTP/1.1 503"), "round {round}: {busy}");
        drop(extra);
        drop(conns);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while server.stats().transport.open > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(server.stats().transport.open, 0, "round {round}");
    }
    let stats = server.stats();
    assert_eq!(stats.replied, 40, "{stats:?}");
    assert_eq!(stats.shed, 5, "{stats:?}");
    let report = server.drain(Duration::from_secs(2));
    assert_eq!(report.killed, 0);
}
