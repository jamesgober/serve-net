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
