//! Attack tests for threats in `dev/PLAN.md` section 12 that had none of their
//! own. Each test names its threat; the request-level ones run through the
//! test transport, the rest on real sockets.

#![cfg(feature = "http1")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serve_net::testing::{TestConnection, TestRequest};
use serve_net::{
    Envelope, ForwardedSource, HeadAction, HttpHead, HttpReply, HttpRequest, HttpService, Limits,
    ListenerSpec, Outcome, ProxySource, Scheme, Server, TrustedProxies,
};

/// Records the authority and scheme the service sees at `head` and at `call`,
/// and answers with the call's.
#[derive(Default)]
struct Seen {
    head: Mutex<Option<(String, Scheme)>>,
}

impl HttpService for Seen {
    fn head(&self, head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
        let seen = (
            head.authority().unwrap_or("").to_owned(),
            head.client().scheme(),
        );
        *self.head.lock().unwrap() = Some(seen);
        HeadAction::Proceed
    }

    fn call(&self, req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
        let scheme = match req.client().scheme() {
            Scheme::Https => "https",
            _ => "http",
        };
        let text = format!("{scheme} {}", req.head().authority().unwrap_or(""));
        reply.text(200, &text);
        Outcome::Done
    }
}

const UNTRUSTED: ([u8; 4], u16) = ([203, 0, 113, 5], 40000);
const TRUSTED: ([u8; 4], u16) = ([127, 0, 0, 1], 40000);

fn behind(source: ForwardedSource) -> ProxySource {
    ProxySource::headers(source, TrustedProxies::loopback())
        .trust_scheme(true)
        .trust_host(true)
}

/// T-17: forwarded host and scheme from a peer that is not a trusted proxy
/// change nothing, at `head` or at `call`, in either header kind. Before
/// 0.6.2 the view built for `call` took the host from an untrusted peer.
#[test]
fn test_t17_untrusted_peer_cannot_set_authority_or_scheme() {
    let cases: [(ForwardedSource, &[u8]); 2] = [
        (
            ForwardedSource::XForwardedFor,
            b"GET / HTTP/1.1\r\nHost: origin.example\r\nX-Forwarded-For: 198.51.100.7\r\nX-Forwarded-Proto: https\r\nX-Forwarded-Host: evil.example\r\n\r\n",
        ),
        (
            ForwardedSource::Forwarded,
            b"GET / HTTP/1.1\r\nHost: origin.example\r\nForwarded: for=198.51.100.7;proto=https;host=evil.example\r\n\r\n",
        ),
    ];
    for (source, request) in cases {
        let site = Seen::default();
        let reply = TestRequest::new(request)
            .client(UNTRUSTED.into())
            .behind(behind(source))
            .send(&site);
        assert_eq!(reply.status, 200, "{source:?}");
        assert_eq!(reply.text(), "http origin.example", "{source:?}: call");
        let head = site.head.lock().unwrap().clone().unwrap();
        assert_eq!(
            head,
            ("origin.example".to_owned(), Scheme::Http),
            "{source:?}: head"
        );
    }
}

/// T-17, the other side: from a trusted proxy the forwarded host and scheme
/// are used, and `head` and `call` see the same ones.
#[test]
fn test_t17_trusted_proxy_sets_authority_and_scheme_at_head_and_call() {
    let cases: [(ForwardedSource, &[u8]); 2] = [
        (
            ForwardedSource::XForwardedFor,
            b"GET / HTTP/1.1\r\nHost: origin.example\r\nX-Forwarded-For: 198.51.100.7\r\nX-Forwarded-Proto: https\r\nX-Forwarded-Host: site.example\r\n\r\n",
        ),
        (
            ForwardedSource::Forwarded,
            b"GET / HTTP/1.1\r\nHost: origin.example\r\nForwarded: for=198.51.100.7;proto=https;host=site.example\r\n\r\n",
        ),
    ];
    for (source, request) in cases {
        let site = Seen::default();
        let reply = TestRequest::new(request)
            .client(TRUSTED.into())
            .behind(behind(source))
            .send(&site);
        assert_eq!(reply.text(), "https site.example", "{source:?}: call");
        let head = site.head.lock().unwrap().clone().unwrap();
        assert_eq!(
            head,
            ("site.example".to_owned(), Scheme::Https),
            "{source:?}: head"
        );
    }
}

/// T-13: a direct listener reads no forwarded field of either kind: the
/// client, scheme, and authority come from the socket and `Host`.
#[test]
fn test_t13_direct_listener_ignores_every_forwarded_field() {
    let request = b"GET / HTTP/1.1\r\nHost: origin.example\r\nX-Forwarded-For: 198.51.100.7\r\nX-Forwarded-Proto: https\r\nX-Forwarded-Host: evil.example\r\nForwarded: for=198.51.100.8;proto=https;host=evil.example\r\n\r\n";
    let site = serve_net::handler(|req, reply| {
        let client = req
            .client()
            .addr()
            .map(|a| a.to_string())
            .unwrap_or_default();
        let scheme = matches!(req.client().scheme(), Scheme::Https);
        let text = format!("{client} {scheme} {}", req.head().authority().unwrap_or(""));
        reply.text(200, &text);
    });
    let reply = TestRequest::new(request)
        .client(UNTRUSTED.into())
        .send(&site);
    assert_eq!(reply.text(), "203.0.113.5 false origin.example");
}

/// T-35: control bytes in a forwarded address are refused, never handed on
/// as text.
#[test]
fn test_t35_control_bytes_in_a_forwarded_address_are_refused() {
    let site = serve_net::handler(|_req, reply| {
        reply.text(200, "reached");
    });
    for bad in [
        &b"1.2.3.4\x1b[2J"[..],
        b"1.2.3.4\x00",
        b"1.2.3.4%0d%0aSet-Cookie: x",
        b"\x7f1.2.3.4",
    ] {
        let mut request = b"GET / HTTP/1.1\r\nHost: a\r\nX-Forwarded-For: ".to_vec();
        request.extend_from_slice(bad);
        request.extend_from_slice(b"\r\n\r\n");
        let reply = TestRequest::new(&request)
            .behind(ProxySource::forwarded_for(TrustedProxies::loopback()))
            .send(&site);
        assert_eq!(reply.status, 400, "{:?}", String::from_utf8_lossy(bad));
        assert!(!reply.text().contains("reached"));
    }
}

/// T-34: every refusal is a fixed short body with no `Server` field and no
/// byte of the request in it.
#[test]
fn test_t34_refusals_carry_no_request_bytes_and_no_server_field() {
    let site = serve_net::handler(|_req, reply| {
        reply.text(200, "ok");
    });
    let secret = "s3cr3t-token";
    let big = "x".repeat(64 * 1024);
    let cases: Vec<(u16, Vec<u8>)> = vec![
        (400, format!("GET /{secret} HTTP/1.1\r\n\r\n").into_bytes()),
        (
            400,
            format!("GET /{secret} HTTP/1.1\r\nHost: a\r\nBad Header: {secret}\r\n\r\n")
                .into_bytes(),
        ),
        (
            431,
            format!("GET /{secret} HTTP/1.1\r\nHost: a\r\nX-Big: {big}{secret}\r\n\r\n")
                .into_bytes(),
        ),
        (
            413,
            format!("POST /{secret} HTTP/1.1\r\nHost: a\r\nContent-Length: 99999999999\r\n\r\n")
                .into_bytes(),
        ),
    ];
    for (status, request) in cases {
        let reply = TestRequest::new(&request).send(&site);
        assert_eq!(
            reply.status,
            status,
            "{:?}",
            String::from_utf8_lossy(&request[..40])
        );
        assert!(
            reply.header("server").is_none(),
            "{status} has a Server field"
        );
        let mut raw = String::from_utf8_lossy(&reply.body).into_owned();
        for (name, value) in &reply.headers {
            raw.push_str(&format!("{name}: {}\n", String::from_utf8_lossy(value)));
        }
        assert!(!raw.contains(secret), "{status} echoes the request: {raw}");
        assert!(raw.len() < 256, "{status} refusal is {} bytes", raw.len());
        assert!(reply.closes, "{status} keeps the connection");
    }
}

/// T-8: a reply sent before the body was read closes the connection, so a
/// request smuggled inside the unread body is never answered.
#[test]
fn test_t8_request_in_an_unread_body_is_never_answered() {
    struct Early;
    impl HttpService for Early {
        fn head(&self, head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
            if head.path() == "/upload" {
                HeadAction::refuse(403)
            } else {
                HeadAction::Proceed
            }
        }
        fn call(&self, req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
            reply.text(200, req.path());
            Outcome::Done
        }
    }
    let smuggled = b"GET /smuggled HTTP/1.1\r\nHost: a\r\n\r\n";
    let mut input = format!(
        "POST /upload HTTP/1.1\r\nHost: a\r\nContent-Length: {}\r\n\r\n",
        smuggled.len()
    )
    .into_bytes();
    input.extend_from_slice(smuggled);
    let mut conn = TestConnection::new(Early);
    let replies = conn.send(&input);
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].status, 403);
    assert!(!conn.is_open());
}

fn connect(addr: SocketAddr) -> TcpStream {
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s
}

/// Reads until the peer closes or resets; true when it did within `limit`.
fn closed_within(stream: &mut TcpStream, limit: Duration) -> bool {
    let start = Instant::now();
    let mut buf = [0u8; 64 * 1024];
    // macOS refuses the option (EINVAL) on a socket the peer already reset.
    if stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .is_err()
    {
        return true;
    }
    while start.elapsed() < limit {
        match stream.read(&mut buf) {
            Ok(0) => return true,
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return true,
        }
    }
    false
}

/// T-29: a client that keeps its request open does not hold up shutdown: the
/// drain ends at its deadline and kills the connection.
#[test]
fn test_t29_drain_deadline_kills_a_request_kept_open() {
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .limits(Limits::new().request_timeout(Duration::from_secs(120)))
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    let mut c = connect(addr);
    c.write_all(b"POST / HTTP/1.1\r\nHost: a\r\nContent-Length: 1000000\r\n\r\nabc")
        .unwrap();
    // The head is in: the request is open and waiting for its body.
    std::thread::sleep(Duration::from_millis(200));
    let start = Instant::now();
    let report = server.drain(Duration::from_millis(300));
    let took = start.elapsed();
    assert!(
        took < Duration::from_secs(3),
        "drain took {took:?}: {report:?}"
    );
    assert_eq!(report.killed, 1, "{report:?}");
    assert!(
        closed_within(&mut c, Duration::from_secs(3)),
        "the connection stayed open"
    );
}

/// T-3: a client that sends requests and never reads holds at most the
/// output budget, and is closed once its writes stall.
#[test]
fn test_t3_client_that_never_reads_is_closed_after_the_write_stall() {
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .limits(
            Limits::new()
                .write_stall(Duration::from_millis(500))
                .max_pending_output(64 * 1024),
        )
        .serve(serve_net::handler(|_req, reply| {
            reply.body_static(&[b'x'; 16 * 1024]);
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    let mut c = TcpStream::connect(addr).unwrap();
    c.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
    // Far more replies than socket buffers and the budget can hold.
    let one = b"GET / HTTP/1.1\r\nHost: a\r\n\r\n";
    let batch: Vec<u8> = one
        .iter()
        .copied()
        .cycle()
        .take(one.len() * 2_000)
        .collect();
    let start = Instant::now();
    let mut gone = false;
    while start.elapsed() < Duration::from_secs(10) {
        if c.write_all(&batch).is_err() {
            gone = true;
            break;
        }
        // Still accepting writes: the server may have stopped reading; wait
        // and probe again rather than spin.
        std::thread::sleep(Duration::from_millis(100));
        let mut probe = [0u8; 1];
        c.set_nonblocking(true).unwrap();
        let peeked = c.peek(&mut probe);
        c.set_nonblocking(false).unwrap();
        if matches!(peeked, Ok(0))
            || matches!(peeked, Err(ref e) if e.kind() != std::io::ErrorKind::WouldBlock)
        {
            gone = true;
            break;
        }
    }
    assert!(
        gone,
        "a client that never read was still connected after 10 s"
    );
    // Another client is still served.
    let mut d = connect(addr);
    d.write_all(b"GET / HTTP/1.1\r\nHost: a\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut head = [0u8; 12];
    d.read_exact(&mut head).unwrap();
    assert_eq!(&head, b"HTTP/1.1 200");
    let _ = server.drain(Duration::from_secs(1));
}

/// T-6: ten thousand pipelined requests in one write are answered in turn
/// while another connection is served promptly. The per-connection request
/// limit (1,000 by default) is lifted here so the flood itself is measured.
#[test]
fn test_t6_ten_thousand_pipelined_requests_are_fair_to_others() {
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .keep_alive(serve_net::KeepAlive::new().max_requests(u32::MAX))
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    let one = b"GET / HTTP/1.1\r\nHost: a\r\n\r\n";
    let n = 10_000;
    let flood: Vec<u8> = one.iter().copied().cycle().take(one.len() * n).collect();
    let mut a = connect(addr);
    let writer = {
        let mut w = a.try_clone().unwrap();
        std::thread::spawn(move || w.write_all(&flood))
    };
    // While the flood is being answered, a second client gets its reply fast.
    std::thread::sleep(Duration::from_millis(20));
    let start = Instant::now();
    let mut b = connect(addr);
    b.write_all(b"GET / HTTP/1.1\r\nHost: a\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut head = [0u8; 12];
    b.read_exact(&mut head).unwrap();
    let waited = start.elapsed();
    assert_eq!(&head, b"HTTP/1.1 200");
    assert!(
        waited < Duration::from_secs(1),
        "the second client waited {waited:?}"
    );
    // Every pipelined request is answered, in order, on the first connection.
    let mut got = 0usize;
    let mut buf = vec![0u8; 256 * 1024];
    let mut tail = Vec::new();
    let marker = b"HTTP/1.1 200";
    while got < n {
        let read = a.read(&mut buf).expect("the flood's replies stopped");
        assert!(read > 0, "closed after {got} replies");
        tail.extend_from_slice(&buf[..read]);
        let mut at = 0;
        while let Some(i) = tail[at..].windows(marker.len()).position(|w| w == marker) {
            got += 1;
            at += i + marker.len();
        }
        tail.drain(..at);
    }
    assert_eq!(got, n);
    writer.join().unwrap().unwrap();
    let _ = server.drain(Duration::from_secs(1));
}
