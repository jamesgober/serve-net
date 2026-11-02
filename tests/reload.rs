//! `Running::reload` over real sockets.

#![cfg(feature = "http1")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use serve_net::{
    Ingress, Limits, ListenerSpec, ProxySource, ProxyVersions, RuleId, ServeError, Server,
    ServerConfig, TrustedProxies,
};

fn site() -> impl serve_net::HttpService {
    serve_net::handler(|req, reply| {
        let client = req
            .client()
            .addr()
            .map(|a| a.to_string())
            .unwrap_or_default();
        reply.text(200, &format!("{client} {}", req.body_bytes().len()));
    })
}

/// A port on loopback for a listener that keeps its address across a reload
/// (port 0 would be a different socket each time). Taken from a per-process
/// run below the ephemeral range, so parallel tests never collide and a
/// connection's source port never takes one.
fn free_addr() -> SocketAddr {
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let base = 20_000 + (std::process::id() % 10_000) as u16;
    loop {
        let port = base + NEXT.fetch_add(1, Ordering::Relaxed);
        if let Ok(l) = TcpListener::bind(("127.0.0.1", port)) {
            return l.local_addr().unwrap();
        }
    }
}

fn connect(addr: SocketAddr) -> TcpStream {
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s
}

fn exchange(stream: &mut TcpStream, request: &[u8]) -> String {
    stream.write_all(request).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).expect("read timed out");
        assert!(n > 0, "closed: {:?}", String::from_utf8_lossy(&got));
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

const GET: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";

fn config(listeners: Vec<ListenerSpec>) -> ServerConfig {
    let mut c = ServerConfig::new();
    c.listeners = listeners;
    c
}

#[test]
fn test_reload_adds_and_removes_listeners() {
    let a = free_addr();
    let b = free_addr();
    let server = Server::builder()
        .config(config(vec![ListenerSpec::tcp("one", a)]))
        .serve(site())
        .unwrap();
    let mut held = connect(a);
    assert!(exchange(&mut held, GET).starts_with("HTTP/1.1 200"));

    // "two" joins, "one" stays.
    let summary = server
        .reload(config(vec![
            ListenerSpec::tcp("one", a),
            ListenerSpec::tcp("two", b),
        ]))
        .unwrap();
    assert_eq!(summary.added, ["two"]);
    assert!(summary.removed.is_empty() && summary.changed.is_empty());
    assert!(exchange(&mut connect(b), GET).starts_with("HTTP/1.1 200"));
    // The kept listener's connection is untouched.
    assert!(exchange(&mut held, GET).starts_with("HTTP/1.1 200"));
    assert_eq!(server.listeners().len(), 2);

    // "one" goes: its idle connection closes, new connections are refused.
    let summary = server
        .reload(config(vec![ListenerSpec::tcp("two", b)]))
        .unwrap();
    assert_eq!(summary.removed, ["one"]);
    assert!(
        TcpStream::connect(a).is_err(),
        "the removed port still accepts"
    );
    assert!(exchange(&mut connect(b), GET).starts_with("HTTP/1.1 200"));
    assert_eq!(server.listeners().len(), 1);
    assert_eq!(server.listeners()[0].name, "two");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_reload_limits_reach_an_open_connection_at_its_next_request() {
    let a = free_addr();
    let server = Server::builder()
        .config(config(vec![ListenerSpec::tcp("web", a)]))
        .serve(site())
        .unwrap();
    let post = b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 2048\r\n\r\n";
    let mut body = post.to_vec();
    body.extend_from_slice(&[b'x'; 2048]);
    let mut c = connect(a);
    let before = exchange(&mut c, &body);
    assert!(before.ends_with(" 2048"), "{before}");

    let mut next = config(vec![ListenerSpec::tcp("web", a)]);
    next.limits = Limits::new().max_body(1024);
    let summary = server.reload(next).unwrap();
    assert!(summary.added.is_empty() && summary.removed.is_empty());

    // The same keep-alive connection, the new limit.
    let after = exchange(&mut c, &body);
    assert!(after.starts_with("HTTP/1.1 413"), "{after}");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_reload_changes_ingress_in_place() {
    let a = free_addr();
    let server = Server::builder()
        .config(config(vec![ListenerSpec::tcp("web", a)]))
        .serve(site())
        .unwrap();
    let forged = b"GET / HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 203.0.113.9\r\n\r\n";
    let direct = exchange(&mut connect(a), forged);
    assert!(direct.contains("127.0.0.1"), "{direct}");

    let behind = ListenerSpec::tcp("web", a).ingress(Ingress::Proxy(ProxySource::forwarded_for(
        TrustedProxies::loopback(),
    )));
    let summary = server.reload(config(vec![behind])).unwrap();
    assert_eq!(summary.changed, ["web"]);
    let trusted = exchange(&mut connect(a), forged);
    assert!(trusted.contains("203.0.113.9"), "{trusted}");
    assert_eq!(
        server.listeners()[0].ingress,
        serve_net::IngressKind::Forwarded
    );
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_reload_public_bind_refused_old_config_kept() {
    let a = free_addr();
    let seen = Arc::new(std::sync::atomic::AtomicU32::new(0));
    struct Refused(Arc<std::sync::atomic::AtomicU32>);
    impl serve_net::Observer for Refused {
        fn reload_refused(&self, _errors: &serve_net::ConfigErrors) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    let server = Server::builder()
        .config(config(vec![ListenerSpec::tcp("web", a)]))
        .observer(Refused(Arc::clone(&seen)))
        .serve(site())
        .unwrap();
    let insecure = ListenerSpec::tcp("web", "0.0.0.0:0").ingress(Ingress::Proxy(
        ProxySource::forwarded_for(TrustedProxies::loopback()),
    ));
    let err = server.reload(config(vec![insecure])).unwrap_err();
    assert!(
        matches!(&err, ServeError::Reload(e) if e.has(RuleId::PublicBind)),
        "{err}"
    );
    assert_eq!(seen.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(exchange(&mut connect(a), GET).starts_with("HTTP/1.1 200"));
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_reload_workers_and_bind_fixed_settings_are_refused() {
    let a = free_addr();
    let server = Server::builder()
        .config(config(vec![ListenerSpec::tcp("web", a)]))
        .serve(site())
        .unwrap();
    let mut other = config(vec![ListenerSpec::tcp("web", a)]);
    other.workers = serve_net::Workers::blocking(8);
    let err = server.reload(other).unwrap_err();
    assert!(
        matches!(&err, ServeError::Reload(e) if e.has(RuleId::ReloadWorkers)),
        "{err}"
    );
    // PROXY is fixed when the socket is bound.
    let proxied = ListenerSpec::tcp("web", a).ingress(Ingress::Proxy(ProxySource::proxy_protocol(
        ProxyVersions::Both,
        TrustedProxies::loopback(),
    )));
    let err = server.reload(config(vec![proxied])).unwrap_err();
    assert!(
        matches!(&err, ServeError::Reload(e) if e.has(RuleId::ReloadListener)),
        "{err}"
    );
    assert!(exchange(&mut connect(a), GET).starts_with("HTTP/1.1 200"));
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_reload_bind_failure_changes_nothing() {
    let a = free_addr();
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = taken.local_addr().unwrap();
    let b = free_addr();
    let server = Server::builder()
        .config(config(vec![ListenerSpec::tcp("one", a)]))
        .serve(site())
        .unwrap();
    let err = server
        .reload(config(vec![
            ListenerSpec::tcp("one", a),
            ListenerSpec::tcp("two", b),
            ListenerSpec::tcp("three", busy),
        ]))
        .unwrap_err();
    assert!(
        matches!(&err, ServeError::ReloadBind { listener, .. } if listener == "three"),
        "{err}"
    );
    // "two" was bound first and closed again; the old listener serves.
    assert_eq!(server.listeners().len(), 1);
    assert!(
        TcpStream::connect(b).is_err(),
        "the new listener stayed open"
    );
    assert!(exchange(&mut connect(a), GET).starts_with("HTTP/1.1 200"));
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_reload_while_draining_is_refused() {
    let a = free_addr();
    let server = Server::builder()
        .config(config(vec![ListenerSpec::tcp("web", a)]))
        .serve(site())
        .unwrap();
    server.drain_handle().drain(Duration::from_secs(5));
    let err = server
        .reload(config(vec![ListenerSpec::tcp("web", a)]))
        .unwrap_err();
    assert!(matches!(err, ServeError::Draining), "{err}");
    let _ = server.wait();
}

#[test]
fn test_reload_notifies_and_reports() {
    #[derive(Default)]
    struct Log(std::sync::Mutex<Vec<String>>);
    struct Manager(Arc<Log>);
    impl serve_net::Notify for Manager {
        fn ready(&self) {
            self.0.0.lock().unwrap().push("ready".into());
        }
        fn reloading(&self, usec: u64) {
            assert!(usec > 0);
            self.0.0.lock().unwrap().push("reloading".into());
        }
    }
    struct Watch(Arc<Log>);
    impl serve_net::Observer for Watch {
        fn reloaded(&self, summary: &serve_net::ReloadSummary) {
            self.0
                .0
                .lock()
                .unwrap()
                .push(format!("reloaded {:?}", summary.added));
        }
    }
    let log = Arc::new(Log::default());
    let a = free_addr();
    let b = free_addr();
    let server = Server::builder()
        .config(config(vec![ListenerSpec::tcp("one", a)]))
        .notify(Manager(Arc::clone(&log)))
        .observer(Watch(Arc::clone(&log)))
        .serve(site())
        .unwrap();
    server
        .reload(config(vec![
            ListenerSpec::tcp("one", a),
            ListenerSpec::tcp("two", b),
        ]))
        .unwrap();
    assert_eq!(
        *log.0.lock().unwrap(),
        ["ready", "reloading", "reloaded [\"two\"]", "ready"]
    );
    let _ = server.drain(Duration::from_secs(2));
}
