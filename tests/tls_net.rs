//! HTTPS end to end with a real TLS engine (tls-net, on ring), and the
//! engine choice by ALPN (`EngineChoice::Http1OrH2ByAlpn`).

#![cfg(feature = "http1")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serve_net::{
    ConnectionId, EndReason, EngineChoice, Ingress, ListenerSpec, Observer, Profile, ProfileId,
    RuleId, Scheme, Server,
};
use tls_net::{Alpn, ClientBuilder, ClientSession, Event, KeySource, TlsServer, TrustAnchors};

const SITE_PEM: &[u8] = include_bytes!("data/site.pem");
const SITE_KEY: &[u8] = include_bytes!("data/site.key");
const CA_PEM: &[u8] = include_bytes!("data/ca.pem");

fn acceptor(alpn: &[Alpn]) -> TlsServer {
    TlsServer::builder()
        .certificate(SITE_PEM, KeySource::Pem(SITE_KEY))
        .unwrap()
        .alpn(alpn)
        .build()
        .unwrap()
}

/// Counts connections that ended with `EndReason::NoEngine`.
#[derive(Clone, Default)]
struct NoEngine(Arc<AtomicU32>);

impl NoEngine {
    fn count(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }
}

impl Observer for NoEngine {
    fn connection_ended(&self, _c: ConnectionId, end: EndReason, _requests: u32) {
        if end == EndReason::NoEngine {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn serve(alpn: &[Alpn], engine: EngineChoice, seen: NoEngine) -> serve_net::Running {
    Server::builder()
        .listener(ListenerSpec::tcp("secure", "127.0.0.1:0").ingress(Ingress::DirectTls))
        .profile(Profile::new(ProfileId(0)).engine(engine))
        .tls(acceptor(alpn))
        .observer(seen)
        .serve(serve_net::handler(|req, reply| {
            let https = req.client().scheme() == Scheme::Https;
            reply.text(200, &format!("https={https} {}", req.path()));
        }))
        .unwrap()
}

/// A blocking TLS client over a socket: enough for one exchange.
struct Client {
    sock: TcpStream,
    session: ClientSession,
    inbuf: Vec<u8>,
    filled: usize,
    out: Vec<u8>,
}

enum Turn {
    Data(Vec<u8>),
    Closed,
    More,
}

impl Client {
    fn connect(addr: SocketAddr, alpn: &[Alpn]) -> Client {
        Client::connect_as(addr, alpn, "example.test")
    }

    /// As `connect`, asking for `sni` in the handshake.
    fn connect_as(addr: SocketAddr, alpn: &[Alpn], sni: &str) -> Client {
        let tls = ClientBuilder::new()
            .trust(TrustAnchors::from_pem(CA_PEM).unwrap())
            .alpn(alpn)
            .build()
            .unwrap();
        let sock = TcpStream::connect(addr).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        Client {
            sock,
            session: tls.connect(sni).unwrap(),
            inbuf: vec![0; 64 * 1024],
            filled: 0,
            out: vec![0; 64 * 1024],
        }
    }

    fn turn(&mut self) -> Turn {
        let Ok(p) = self
            .session
            .step(&mut self.inbuf[..self.filled], &mut self.out)
        else {
            return Turn::Closed;
        };
        let (consumed, written) = (p.consumed, p.written);
        let mut turn = Turn::More;
        let mut need = false;
        match p.event {
            Event::Data(d) => turn = Turn::Data(d.to_vec()),
            Event::NeedInput => need = true,
            Event::PeerClosed | Event::Closed => turn = Turn::Closed,
            _ => {}
        }
        if written > 0 && self.sock.write_all(&self.out[..written]).is_err() {
            return Turn::Closed;
        }
        self.inbuf.copy_within(consumed..self.filled, 0);
        self.filled -= consumed;
        if need && written == 0 && consumed == 0 {
            if self.filled == self.inbuf.len() {
                let want = self.session.input_capacity_required().max(self.filled + 1);
                self.inbuf.resize(want, 0);
            }
            match self.sock.read(&mut self.inbuf[self.filled..]) {
                Ok(0) | Err(_) => return Turn::Closed,
                Ok(n) => self.filled += n,
            }
        }
        turn
    }

    /// Completes the handshake; returns the negotiated ALPN id, or `None`
    /// when the server closed first.
    fn handshake(&mut self) -> Option<Option<Vec<u8>>> {
        while self.session.info().is_none() {
            if matches!(self.turn(), Turn::Closed) {
                return None;
            }
        }
        Some(self.session.info().and_then(|i| i.alpn.map(<[u8]>::to_vec)))
    }

    /// Sends a request and reads until the server closes, or `None` when the
    /// connection ended before any reply byte.
    fn exchange(&mut self, request: &[u8]) -> Option<String> {
        let n = self.session.encrypt(request, &mut self.out).ok()?;
        self.sock.write_all(&self.out[..n]).ok()?;
        let mut got = Vec::new();
        for _ in 0..1_000 {
            match self.turn() {
                Turn::Data(d) => got.extend(d),
                Turn::Closed => break,
                Turn::More => {}
            }
        }
        (!got.is_empty()).then(|| String::from_utf8_lossy(&got).into_owned())
    }
}

#[test]
fn test_tls_net_serves_https_end_to_end() {
    let seen = NoEngine::default();
    let server = serve(&[Alpn::HTTP_1_1], EngineChoice::Http1, seen.clone());
    let addr = server.local_addr().unwrap();
    let mut c = Client::connect(addr, &[Alpn::HTTP_1_1]);
    assert_eq!(c.handshake(), Some(Some(b"http/1.1".to_vec())));
    let reply = c
        .exchange(b"GET /secure HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n\r\n")
        .expect("a reply");
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(reply.ends_with("https=true /secure"), "{reply}");
    let report = server.drain(Duration::from_secs(2));
    assert_eq!(report.killed, 0, "{report:?}");
    assert_eq!(seen.count(), 0);
}

#[test]
fn test_alpn_http11_and_no_alpn_run_http1_under_by_alpn() {
    let seen = NoEngine::default();
    let server = serve(
        &[Alpn::H2, Alpn::HTTP_1_1],
        EngineChoice::Http1OrH2ByAlpn,
        seen.clone(),
    );
    let addr = server.local_addr().unwrap();
    for offer in [&[Alpn::HTTP_1_1][..], &[]] {
        let mut c = Client::connect(addr, offer);
        assert!(c.handshake().is_some());
        let reply = c
            .exchange(b"GET /a HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n\r\n")
            .expect("a reply");
        assert!(reply.ends_with("https=true /a"), "{reply}");
    }
    assert_eq!(seen.count(), 0);
    let _ = server.drain(Duration::from_secs(2));
}

/// No HTTP/2 engine is built yet: a connection that negotiated `h2` is
/// closed with `NoEngine` before any byte reaches the HTTP/1 parser.
#[test]
fn test_alpn_h2_without_an_engine_is_closed_with_no_engine() {
    for engine in [EngineChoice::Http1OrH2ByAlpn, EngineChoice::Http1] {
        let seen = NoEngine::default();
        let server = serve(&[Alpn::H2, Alpn::HTTP_1_1], engine, seen.clone());
        let addr = server.local_addr().unwrap();
        let mut c = Client::connect(addr, &[Alpn::H2]);
        if c.handshake().is_some() {
            let preface = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
            assert_eq!(
                c.exchange(preface),
                None,
                "{engine:?}: an h2 connection got a reply"
            );
        }
        for _ in 0..100 {
            if seen.count() > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(seen.count(), 1, "{engine:?}");
        let _ = server.drain(Duration::from_secs(2));
    }
}

#[test]
fn test_by_alpn_on_a_listener_without_tls_is_refused() {
    let err = Server::builder()
        .listener(ListenerSpec::tcp("plain", "127.0.0.1:0"))
        .profile(Profile::new(ProfileId(0)).engine(EngineChoice::Http1OrH2ByAlpn))
        .build()
        .unwrap_err();
    assert!(err.has(RuleId::AlpnWithoutTls), "{err}");
}

/// A server whose service names the TLS server name it saw.
fn serve_names(spec: ListenerSpec) -> serve_net::Running {
    Server::builder()
        .listener(spec)
        .tls(acceptor(&[Alpn::HTTP_1_1]))
        .serve(Names)
        .unwrap()
}

/// Answers with the TLS server name and the request's authority.
struct Names;

impl serve_net::HttpService for Names {
    fn call(
        &self,
        req: &serve_net::HttpRequest<'_>,
        env: &serve_net::Envelope,
        reply: &mut serve_net::HttpReply,
    ) -> serve_net::Outcome {
        let sni = env.tls().and_then(|t| t.sni()).unwrap_or("-");
        let host = req.head().authority().unwrap_or("-");
        reply.text(200, &format!("sni={sni} host={host}"));
        serve_net::Outcome::Done
    }
}

fn tls_listener() -> ListenerSpec {
    ListenerSpec::tcp("secure", "127.0.0.1:0").ingress(Ingress::DirectTls)
}

fn get(addr: SocketAddr, sni: &str, host: &str) -> String {
    let mut c = Client::connect_as(addr, &[Alpn::HTTP_1_1], sni);
    assert!(c.handshake().is_some(), "no handshake for {sni}");
    let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    c.exchange(request.as_bytes()).expect("a reply")
}

/// T-42: a client that asked the TLS layer for one name and the HTTP layer
/// for another is answered 421, so a policy the TLS layer chose by name
/// (client certificates per host) cannot be stepped around through `Host`.
#[test]
fn test_t42_host_that_differs_from_the_tls_server_name_is_misdirected() {
    let server = serve_names(tls_listener());
    let addr = server.local_addr().unwrap();
    let reply = get(addr, "www.example.test", "admin.example.test");
    assert!(reply.starts_with("HTTP/1.1 421 "), "{reply}");
    assert!(!reply.contains("sni="), "the service ran: {reply}");
    // The names match: served, and the service sees the server name.
    let reply = get(addr, "admin.example.test", "admin.example.test");
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(
        reply.ends_with("sni=admin.example.test host=admin.example.test"),
        "{reply}"
    );
    // A port, a trailing dot, and letter case do not make a mismatch.
    let reply = get(addr, "admin.example.test", "ADMIN.example.test.:443");
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_t42_allow_sni_mismatch_serves_and_shows_both_names() {
    let server = serve_names(tls_listener().allow_sni_mismatch("a test of the opt-out"));
    let addr = server.local_addr().unwrap();
    let reply = get(addr, "www.example.test", "admin.example.test");
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(
        reply.ends_with("sni=www.example.test host=admin.example.test"),
        "{reply}"
    );
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_plain_listener_has_no_tls_facts() {
    let server = serve_names(ListenerSpec::tcp("plain", "127.0.0.1:0"));
    let addr = server.local_addr().unwrap();
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"GET / HTTP/1.1\r\nHost: a\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut got = String::new();
    let _ = s.read_to_string(&mut got);
    assert!(got.ends_with("sni=- host=a"), "{got}");
    let _ = server.drain(Duration::from_secs(2));
}
