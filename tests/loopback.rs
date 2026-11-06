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

/// Whether a new connection gets an answer. A refusal may be a reset, and
/// the reset may arrive while the client still connects.
fn served(addr: SocketAddr) -> bool {
    let Ok(mut c) = TcpStream::connect(addr) else {
        return false;
    };
    let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = c.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    matches!(c.read(&mut [0u8; 64]), Ok(n) if n > 0)
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
        .transport_limits(
            net_io::Limits::builder()
                .max_per_source(16, net_io::PrefixPolicy::DEFAULT)
                .build()
                .unwrap(),
        )
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    let listeners = server.listeners();
    let proxied = listeners[0].local.socket_addr().unwrap();
    let direct = listeners[1].local.socket_addr().unwrap();
    // The proxy listener takes more than the per-source cap from one peer.
    let mut through: Vec<TcpStream> = (0..20).map(|_| connect(proxied)).collect();
    for (i, c) in through.iter_mut().enumerate() {
        let a = exchange(
            c,
            b"GET / HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 192.0.2.1\r\n\r\n",
        );
        assert!(a.starts_with("HTTP/1.1 200"), "proxied {i}: {a}");
    }
    // The direct listener still caps one address: the 17th is not served.
    let mut held: Vec<TcpStream> = (0..16).map(|_| connect(direct)).collect();
    for (i, c) in held.iter_mut().enumerate() {
        let a = exchange(c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(a.starts_with("HTTP/1.1 200"), "direct {i}: {a}");
    }
    assert!(!served(direct), "a 17th connection from one address");
    drop((through, held));
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

/// A server behind the PROXY protocol that answers with what it learned.
fn proxied(
    versions: serve_net::ProxyVersions,
    trusted: serve_net::TrustedProxies,
) -> serve_net::Running {
    use serve_net::{Ingress, ListenerSpec, ProxySource, Server};
    Server::builder()
        .listener(
            ListenerSpec::tcp("edge", "127.0.0.1:0").ingress(Ingress::Proxy(
                ProxySource::proxy_protocol(versions, trusted),
            )),
        )
        .serve(serve_net::handler(|req, reply| {
            let c = req.client();
            let text = format!(
                "{} {} {:?} local={}",
                c.addr().map(|a| a.to_string()).unwrap_or_default(),
                c.port(),
                c.ingress(),
                c.is_proxy_local()
            );
            reply.text(200, &text);
        }))
        .unwrap()
}

#[test]
fn test_loopback_proxy_v1_gives_the_real_client() {
    let server = proxied(
        serve_net::ProxyVersions::Both,
        serve_net::TrustedProxies::loopback(),
    );
    let addr = server.local_addr().unwrap();
    let mut c = connect(addr);
    c.write_all(b"PROXY TCP4 203.0.113.7 127.0.0.1 51000 80\r\n")
        .unwrap();
    let a = exchange(&mut c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(a.starts_with("HTTP/1.1 200"), "{a}");
    assert!(
        a.ends_with("203.0.113.7 51000 ProxyProtocol local=false"),
        "{a}"
    );
    // Keep-alive: the second request on the connection keeps the client.
    let b = exchange(&mut c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(
        b.ends_with("203.0.113.7 51000 ProxyProtocol local=false"),
        "{b}"
    );
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_loopback_proxy_v2_and_local() {
    let server = proxied(
        serve_net::ProxyVersions::V2,
        serve_net::TrustedProxies::loopback(),
    );
    let addr = server.local_addr().unwrap();
    const SIG: &[u8] = b"\r\n\r\n\0\r\nQUIT\n";
    // PROXY TCP4 198.51.100.9:443 -> 127.0.0.1:80
    let mut v2 = SIG.to_vec();
    v2.extend_from_slice(&[
        0x21, 0x11, 0, 12, 198, 51, 100, 9, 127, 0, 0, 1, 0x01, 0xBB, 0, 80,
    ]);
    let mut c = connect(addr);
    c.write_all(&v2).unwrap();
    let a = exchange(&mut c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(
        a.ends_with("198.51.100.9 443 ProxyProtocol local=false"),
        "{a}"
    );
    // LOCAL: the proxy's own health check keeps the socket peer.
    let mut local = SIG.to_vec();
    local.extend_from_slice(&[0x20, 0x00, 0, 0]);
    let mut h = connect(addr);
    h.write_all(&local).unwrap();
    let b = exchange(&mut h, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(b.contains("127.0.0.1"), "{b}");
    assert!(b.ends_with("local=true"), "{b}");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_loopback_proxy_from_untrusted_peer_is_closed_unread() {
    let server = proxied(
        serve_net::ProxyVersions::Both,
        serve_net::TrustedProxies::parse("10.0.0.0/8").unwrap(),
    );
    let addr = server.local_addr().unwrap();
    let mut c = connect(addr);
    let _ = c.write_all(
        b"PROXY TCP4 203.0.113.7 127.0.0.1 51000 80\r\nGET / HTTP/1.1\r\nHost: x\r\n\r\n",
    );
    let mut buf = [0u8; 64];
    let served = matches!(c.read(&mut buf), Ok(n) if n > 0);
    assert!(!served, "a connection from an untrusted proxy was served");
    let stats = server.stats();
    assert_eq!(stats.requests, 0, "{stats:?}");
    let _ = server.drain(Duration::from_secs(2));
}

/// A listening socket released by another server, as a supervisor would
/// hand it over.
fn released() -> (net_io::Server, SocketAddr, net_io::ListenerHandle) {
    let old = net_io::ServerBuilder::new()
        .listener(net_io::ListenerSpec::tcp("127.0.0.1:0"))
        .handler(|_c: &mut net_io::TcpConnection| Ok(()))
        .gate(net_io::AllowAll)
        .build()
        .unwrap();
    let addr = old.listeners()[0].local.socket_addr().unwrap();
    let handle = old.release_listeners().unwrap().pop().unwrap();
    (old, addr, handle)
}

#[test]
fn test_loopback_handed_off_socket_is_served_on_its_port() {
    use serve_net::{ListenerSpec, Server};
    let (old, addr, handle) = released();
    // The old server stops; the socket lives on in the new one.
    let _ = old.shutdown(std::time::Instant::now());
    let server = Server::builder()
        .listener(ListenerSpec::handed_off("web", handle))
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "new process");
        }))
        .unwrap();
    assert_eq!(server.local_addr(), Some(addr));
    let mut c = connect(addr);
    let a = exchange(&mut c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(a.ends_with("new process"), "{a}");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_loopback_handed_off_socket_of_the_wrong_kind_is_refused() {
    use serve_net::{Ingress, ListenerSpec, Server, UnixPeerTrust};
    let (_old, _addr, handle) = released();
    let err = Server::builder()
        .listener(
            ListenerSpec::handed_off("web", handle)
                .ingress(Ingress::Unix(UnixPeerTrust::AnyLocalPeer, None)),
        )
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "x");
        }))
        .unwrap_err();
    assert!(
        matches!(&err, serve_net::ServeError::HandOff { listener, .. } if listener == "web"),
        "{err}"
    );
}

#[test]
fn test_loopback_inherited_without_systemd_uses_the_fallback() {
    use serve_net::{ListenerSpec, Server};
    // Not socket-activated: without a fallback the listener has no socket.
    let err = Server::builder()
        .listener(ListenerSpec::inherited("web"))
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "x");
        }))
        .unwrap_err();
    assert!(
        matches!(&err, serve_net::ServeError::Inherited { listener, .. } if listener == "web"),
        "{err}"
    );
    let server = Server::builder()
        .listener(ListenerSpec::inherited("web").fallback_bind("127.0.0.1:0".parse().unwrap()))
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "fallback");
        }))
        .unwrap();
    let mut c = connect(server.local_addr().unwrap());
    let a = exchange(&mut c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(a.ends_with("fallback"), "{a}");
    let _ = server.drain(Duration::from_secs(2));
}

/// Records what a service manager and an observer would see.
#[derive(Default)]
struct Log(std::sync::Mutex<Vec<String>>);

impl Log {
    fn push(&self, line: impl Into<String>) {
        self.0.lock().unwrap().push(line.into());
    }
    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

struct Manager {
    log: std::sync::Arc<Log>,
    fail: bool,
}

impl serve_net::Notify for Manager {
    fn ready(&self) {
        self.log.push("ready");
    }
    fn stopping(&self) {
        self.log.push("stopping");
    }
    fn extend_timeout(&self, by: Duration) {
        self.log.push(format!("extend {}", by.as_secs()));
    }
    fn take_error(&self) -> Option<std::io::Error> {
        self.fail.then(|| std::io::Error::other("no manager"))
    }
}

struct Watch(std::sync::Arc<Log>);

impl serve_net::Observer for Watch {
    fn capacity_refused(&self, listener: &str, _profile: serve_net::ProfileId) {
        self.0.push(format!("capacity {listener}"));
    }
    fn drain_started(&self, open: u32) {
        self.0.push(format!("drain_started {open}"));
    }
    fn drain_finished(&self, report: &serve_net::DrainReport) {
        self.0.push(format!("drain_finished {}", report.killed));
    }
    fn public_bind(&self, listener: &str, reason: &'static str) {
        self.0.push(format!("public {listener} {reason}"));
    }
    fn notify_failed(&self, error: &std::io::Error) {
        self.0.push(format!("notify_failed {error}"));
    }
}

#[test]
fn test_loopback_notify_and_observer_follow_the_lifecycle() {
    use serve_net::{Limits, ListenerSpec, Server};
    let log = std::sync::Arc::new(Log::default());
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .limits(Limits::new().max_connections(1))
        .notify(Manager {
            log: std::sync::Arc::clone(&log),
            fail: false,
        })
        .observer(Watch(std::sync::Arc::clone(&log)))
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    assert_eq!(log.lines(), ["ready"]);
    let addr = server.local_addr().unwrap();
    let mut held = connect(addr);
    let a = exchange(&mut held, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(a.starts_with("HTTP/1.1 200"), "{a}");
    // The refusal comes at once, with no request to read.
    let mut over = connect(addr);
    let mut b = String::new();
    let _ = over.read_to_string(&mut b);
    assert!(b.starts_with("HTTP/1.1 503"), "{b}");
    drop((held, over));
    let report = server.drain(Duration::from_secs(10));
    assert_eq!(report.killed, 0);
    let lines = log.lines();
    assert_eq!(lines[1], "capacity web");
    assert_eq!(lines[2], "extend 10", "{lines:?}");
    assert!(lines[3].starts_with("drain_started "), "{lines:?}");
    assert_eq!(lines[4], "stopping");
    assert_eq!(lines[5], "drain_finished 0");
}

#[test]
fn test_loopback_notify_failure_reaches_observer_and_health() {
    use serve_net::{Health, HealthIssue, ListenerSpec, Server};
    let log = std::sync::Arc::new(Log::default());
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .notify(Manager {
            log: std::sync::Arc::clone(&log),
            fail: true,
        })
        .observer(Watch(std::sync::Arc::clone(&log)))
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    // A failing manager never stops the server.
    let mut c = connect(server.local_addr().unwrap());
    let a = exchange(&mut c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(a.starts_with("HTTP/1.1 200"), "{a}");
    assert!(
        log.lines().iter().any(|l| l == "notify_failed no manager"),
        "{:?}",
        log.lines()
    );
    let health = server.health();
    assert!(
        matches!(&health, Health::Degraded(i) if i.contains(&HealthIssue::NotifyFailing)),
        "{health:?}"
    );
    drop(c);
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_loopback_transport_limits_reach_net_io() {
    use serve_net::{ListenerSpec, Server};
    let one_per_source = net_io::Limits::builder()
        .max_per_source(1, net_io::PrefixPolicy::DEFAULT)
        .build()
        .unwrap();
    for server in [
        // Server-wide, and per listener.
        Server::builder()
            .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
            .transport_limits(one_per_source.clone())
            .serve(serve_net::handler(|_r, reply| {
                reply.text(200, "ok");
            }))
            .unwrap(),
        Server::builder()
            .listener(
                ListenerSpec::tcp("web", "127.0.0.1:0").transport_limits(one_per_source.clone()),
            )
            .serve(serve_net::handler(|_r, reply| {
                reply.text(200, "ok");
            }))
            .unwrap(),
    ] {
        let addr = server.local_addr().unwrap();
        let mut held = connect(addr);
        let a = exchange(&mut held, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(a.starts_with("HTTP/1.1 200"), "{a}");
        assert!(!served(addr), "a second connection from one address");
        let _ = server.drain(Duration::from_secs(1));
    }
}

// A worker blocked in a read learns of a manual clock when woken, which needs
// net-io's wait before each receive: Windows and macOS. Linux receives block
// under SO_RCVTIMEO.
#[cfg(not(target_os = "linux"))]
#[test]
fn test_loopback_a_slow_body_is_answered_by_the_request_deadline() {
    use serve_net::{Limits, ListenerSpec, Server};
    let clock = std::sync::Arc::new(net_io::ManualClock::new());
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .limits(
            Limits::new()
                .request_timeout(Duration::from_secs(120))
                .min_rate(0, Duration::from_secs(10)),
        )
        // The reactor never closes a connection on a frozen clock (reported).
        .workers(serve_net::Workers::blocking(8))
        .clock(std::sync::Arc::clone(&clock))
        .serve(serve_net::handler(|_r, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    let mut c = connect(server.local_addr().unwrap());
    c.write_all(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 10\r\n\r\nabc")
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    // Longer than net-io's default read timeout, shorter than the deadline.
    clock.advance(Duration::from_secs(60));
    server.server_handle().wake_all();
    let mut buf = [0u8; 256];
    c.set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let early = c.read(&mut buf);
    assert!(
        early.is_err(),
        "before the request deadline: {early:?} {:?}",
        String::from_utf8_lossy(&buf)
    );
    // Past the deadline, short of the read timeout behind it.
    clock.advance(Duration::from_millis(60_500));
    // A wake can land before the worker waits; a manual clock will not tick.
    c.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut n = 0;
    for _ in 0..50 {
        server.server_handle().wake_all();
        if let Ok(read @ 1..) = c.read(&mut buf) {
            n = read;
            break;
        }
    }
    let got = String::from_utf8_lossy(&buf[..n]);
    assert!(got.starts_with("HTTP/1.1 408"), "{got}");
    let _ = server.drain(Duration::from_secs(1));
}

#[test]
fn test_loopback_listen_options_apply() {
    use serve_net::{ListenerSpec, Server};
    let server = Server::builder()
        .listener(
            ListenerSpec::tcp("web", "127.0.0.1:0")
                .listen_options(net_io::ListenOptions::default()),
        )
        .serve(serve_net::handler(|_r, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    let mut c = connect(server.local_addr().unwrap());
    let a = exchange(&mut c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(a.starts_with("HTTP/1.1 200"), "{a}");
    let _ = server.drain(Duration::from_secs(1));
}

// A worker blocked in a read learns of a manual clock when woken, which needs
// net-io's wait before each receive: Windows and macOS. Linux receives block
// under SO_RCVTIMEO.
#[cfg(not(target_os = "linux"))]
#[test]
fn test_loopback_manual_clock_drives_the_head_deadline() {
    use serve_net::{Limits, ListenerSpec, Server};
    let clock = std::sync::Arc::new(net_io::ManualClock::new());
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .limits(Limits::new().head_timeout(Duration::from_secs(5)))
        // The reactor never closes a connection on a frozen clock (reported).
        .workers(serve_net::Workers::blocking(8))
        .clock(std::sync::Arc::clone(&clock))
        .serve(serve_net::handler(|_r, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    let mut c = connect(server.local_addr().unwrap());
    c.write_all(b"GET / HT").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    // No real time passes; the clock says the deadline did.
    clock.advance(Duration::from_secs(6));
    // A worker blocked in a read learns of it when woken.
    server.server_handle().wake_all();
    let mut got = String::new();
    let _ = c.read_to_string(&mut got);
    assert!(got.starts_with("HTTP/1.1 408"), "{got}");
    let _ = server.drain(Duration::from_secs(1));
}

#[test]
fn test_loopback_timeouts_answer_on_real_sockets() {
    use serve_net::{Limits, ListenerSpec, Server};
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .limits(
            Limits::new()
                .head_timeout(Duration::from_millis(200))
                .request_timeout(Duration::from_millis(400))
                .min_rate(0, Duration::from_secs(10)),
        )
        .serve(serve_net::handler(|_r, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    // A head that never finishes.
    let mut head = connect(addr);
    head.write_all(b"GET / HT").unwrap();
    let mut got = String::new();
    let _ = head.read_to_string(&mut got);
    assert!(got.starts_with("HTTP/1.1 408"), "head: {got}");
    // A body that never finishes.
    let mut body = connect(addr);
    body.write_all(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 10\r\n\r\nabc")
        .unwrap();
    let mut got = String::new();
    let _ = body.read_to_string(&mut got);
    assert!(got.starts_with("HTTP/1.1 408"), "body: {got}");
    let stats = server.stats();
    assert_eq!(stats.timed_out, 2, "{stats:?}");
    let _ = server.drain(Duration::from_secs(1));
}

#[test]
fn test_detached_head_is_owned_and_redacts_credentials() {
    let site = serve_net::handler(|req, reply| {
        let head = req.detach_head();
        let seen = std::thread::spawn(move || {
            let debug = format!("{head:?}");
            assert!(
                debug.contains("<redacted>") && !debug.contains("s3cret"),
                "{debug}"
            );
            assert_eq!(head.method_str(), "POST");
            assert_eq!(head.query(), Some("a=1"));
            assert_eq!(head.header("X-Tag"), Some(&b"one"[..]));
            assert_eq!(head.header_all("x-tag").count(), 2);
            assert_eq!(head.headers().len(), 5);
            head.path().to_owned()
        })
        .join()
        .unwrap();
        reply.text(200, &seen);
    });
    let reply = serve_net::testing::respond(
        &site,
        b"POST /jobs?a=1 HTTP/1.1\r\nHost: a\r\nX-Tag: one\r\nX-Tag: two\r\nCookie: id=s3cret\r\nContent-Length: 0\r\n\r\n",
    );
    assert_eq!(reply.text(), "/jobs");
}

/// Found in the 0.7 review: a PROXY v2 SSL TLV marks the client https only
/// when its `PP2_CLIENT_SSL` bit is set. A proxy that sends the TLV on every
/// connection (HAProxy's `send-proxy-v2-ssl`) leaves the bit clear for plain
/// clients, which must stay `http`.
#[test]
fn test_loopback_proxy_v2_ssl_tlv_needs_the_client_ssl_bit() {
    use serve_net::{
        Ingress, ListenerSpec, ProxySource, ProxyVersions, Scheme, Server, TrustedProxies,
    };
    let server = Server::builder()
        .listener(
            ListenerSpec::tcp("edge", "127.0.0.1:0").ingress(Ingress::Proxy(
                ProxySource::proxy_protocol(ProxyVersions::V2, TrustedProxies::loopback())
                    .trust_scheme(true),
            )),
        )
        .serve(serve_net::handler(|req, reply| {
            let https = req.client().scheme() == Scheme::Https;
            reply.text(200, if https { "https" } else { "http" });
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    for (client, want) in [
        (0x00u8, "http"),
        (0x01, "https"),
        (0x05, "https"),
        (0x04, "http"),
    ] {
        let mut header = [0u8; 128];
        // PP2_TYPE_SSL: the client flags, then a 32-bit verify result.
        let ssl = [client, 0, 0, 0, 0];
        let n = net_io::encode_v2(
            net_io::ProxyCommand::Proxy,
            Some(SocketAddr::from(([198, 51, 100, 9], 40000))),
            Some(SocketAddr::from(([127, 0, 0, 1], 80))),
            &[net_io::Tlv {
                kind: 0x20,
                value: &ssl,
            }],
            false,
            &mut header,
        )
        .unwrap();
        let mut c = connect(addr);
        c.write_all(&header[..n]).unwrap();
        let a = exchange(&mut c, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(
            a.ends_with(&format!("\r\n\r\n{want}")),
            "client flags {client:#x}: {a}"
        );
    }
    let _ = server.drain(Duration::from_secs(2));
}
