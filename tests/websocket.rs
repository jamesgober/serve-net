//! WebSocket upgrades over real sockets: the handshake, messages both ways,
//! the close handshake, budgets, pings, drains, and T-21.

#![cfg(feature = "ws")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serve_net::{
    Checkpoint, CheckpointAt, Envelope, Finished, HeadAction, HttpHead, HttpReply, HttpRequest,
    HttpService, ListenerSpec, Outcome, RequestEnd, Server, UpgradedConn, Verdict, WebSocketConn,
    WebSocketService, WebSocketTarget, WsMessage, WsSendError,
};
use ws_net::{ClientHandshake, CloseCode, Connection, Event};

/// What the services saw.
#[derive(Default)]
struct Seen {
    opened: AtomicU32,
    upgraded: AtomicU32,
    closed: Mutex<Vec<Option<u16>>>,
    ends: Mutex<Vec<RequestEnd>>,
    refused_full: AtomicU32,
}

/// Echoes every message; a few texts do something else.
struct Echo(Arc<Seen>);

impl WebSocketService for Echo {
    fn opened(&self, _env: &Envelope, conn: &mut WebSocketConn<'_>) {
        self.0.opened.fetch_add(1, Ordering::SeqCst);
        conn.send_text("hello").unwrap();
    }

    fn message(&self, _env: &Envelope, message: WsMessage<'_>, conn: &mut WebSocketConn<'_>) {
        match message {
            WsMessage::Text("close") => conn.close(1000, "bye").unwrap(),
            WsMessage::Text("panic") => panic!("the service panicked on purpose"),
            WsMessage::Text("flood") => {
                // The client does not read: sends stop at the budget.
                let big = vec![b'x'; 16 * 1024];
                for _ in 0..1000 {
                    if conn.send_binary(&big) == Err(WsSendError::Full) {
                        self.0.refused_full.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                }
            }
            WsMessage::Text(t) => conn.send_text(t).unwrap(),
            WsMessage::Binary(b) => conn.send_binary(b).unwrap(),
            _ => {}
        }
    }

    fn closed(&self, _env: &Envelope, code: Option<u16>) {
        self.0.closed.lock().unwrap().push(code);
    }
}

struct App {
    seen: Arc<Seen>,
    echo: Arc<dyn WebSocketService>,
}

impl HttpService for App {
    fn head(&self, head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
        let target = WebSocketTarget::shared(Arc::clone(&self.echo));
        match head.path() {
            "/ws" => HeadAction::Upgrade(target.into()),
            "/ws-chat" => HeadAction::Upgrade(target.subprotocol("chat").into()),
            "/ws-quiet" => HeadAction::Upgrade(target.ping_interval(Duration::from_secs(1)).into()),
            "/ws-small" => HeadAction::Upgrade(
                target
                    .limits(
                        ws_net::Limits::new()
                            .with_max_frame(1024)
                            .with_max_message(1024),
                    )
                    .into(),
            ),
            "/ws-tight" => HeadAction::Upgrade(target.max_pending_output(64 * 1024).into()),
            "/ws-slowmsg" => {
                HeadAction::Upgrade(target.message_timeout(Duration::from_secs(1)).into())
            }
            // A service bug: an upgrade the request never offered.
            "/bug" => HeadAction::Upgrade(target.into()),
            _ => HeadAction::Proceed,
        }
    }

    fn call(&self, _req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
        reply.text(200, "plain");
        Outcome::Done
    }

    fn upgraded(&self, _env: &Envelope, conn: UpgradedConn) {
        assert_eq!(conn.protocol(), "websocket");
        self.seen.upgraded.fetch_add(1, Ordering::SeqCst);
    }

    fn done(&self, _env: &Envelope, finished: &Finished) {
        self.seen.ends.lock().unwrap().push(finished.end);
    }
}

fn start() -> (Arc<Seen>, serve_net::Running, SocketAddr) {
    let seen = Arc::new(Seen::default());
    let app = App {
        seen: Arc::clone(&seen),
        echo: Arc::new(Echo(Arc::clone(&seen))),
    };
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .catch_service_panics(true)
        .serve(app)
        .unwrap();
    let addr = server.local_addr().unwrap();
    (seen, server, addr)
}

/// A WebSocket client over a blocking socket.
struct Client {
    s: TcpStream,
    conn: Connection,
}

impl Client {
    /// Sends the opening request (plus `extra` in the same write) and reads
    /// the response head; returns the head text and the client, or the whole
    /// response when it is not a 101.
    fn open(
        addr: SocketAddr,
        path: &str,
        protocols: &[&str],
        extra: &[u8],
    ) -> Result<(String, Client), String> {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let hs = ClientHandshake::with_nonce(*b"serve-net-nonce!").with_protocols(protocols);
        let mut req = Vec::new();
        hs.write_request("127.0.0.1", path, &mut req).unwrap();
        req.extend_from_slice(extra);
        s.write_all(&req).unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        let end = loop {
            if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                break end + 4;
            }
            match s.read(&mut buf) {
                Ok(n @ 1..) => got.extend_from_slice(&buf[..n]),
                _ => return Err(String::from_utf8_lossy(&got).into_owned()),
            }
        };
        let head = String::from_utf8_lossy(&got[..end]).into_owned();
        if !head.starts_with("HTTP/1.1 101") {
            return Err(head);
        }
        // The accept key matches the nonce.
        assert!(hs.verify_head(&got[..end]).unwrap().is_some(), "{head}");
        let mut conn = Connection::client_with_seed([7; 32]);
        let _ = conn.feed(&got[end..]);
        Ok((head, Client { s, conn }))
    }

    fn flush(&mut self) {
        let out = self.conn.output().to_vec();
        self.s.write_all(&out).unwrap();
        self.conn.advance_output(out.len());
    }

    fn send_text(&mut self, text: &str) {
        self.conn.send_text(text).unwrap();
        self.flush();
    }

    /// The next event, owned; `None` once the server closed the socket.
    fn next(&mut self) -> Option<Got> {
        let mut buf = [0u8; 16 * 1024];
        loop {
            let got = match self.conn.poll() {
                Ok(Event::Text(t)) => Some(Got::Text(t.to_owned())),
                Ok(Event::Binary(b)) => Some(Got::Binary(b.to_vec())),
                Ok(Event::Ping(_)) => Some(Got::Ping),
                Ok(Event::Close(f)) => Some(Got::Close(f.map(|f| f.code.as_u16()))),
                Ok(Event::NeedInput) => None,
                Ok(Event::Closed) => return None,
                Ok(_) => continue,
                Err(e) => panic!("the server sent something invalid: {e:?}"),
            };
            if let Some(g) = got {
                return Some(g);
            }
            match self.s.read(&mut buf) {
                Ok(n @ 1..) => {
                    let _ = self.conn.feed(&buf[..n]);
                }
                _ => {
                    self.conn.feed_eof();
                    return match self.conn.poll() {
                        Ok(Event::Close(f)) => Some(Got::Close(f.map(|f| f.code.as_u16()))),
                        _ => None,
                    };
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Got {
    Text(String),
    Binary(Vec<u8>),
    Ping,
    Close(Option<u16>),
}

fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "{what} never happened");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn test_ws_upgrade_echoes_text_and_binary() {
    let (seen, _server, addr) = start();
    let (head, mut c) = Client::open(addr, "/ws", &[], b"").unwrap();
    assert!(
        head.to_ascii_lowercase().contains("upgrade: websocket"),
        "{head}"
    );
    assert_eq!(c.next(), Some(Got::Text("hello".into())));
    c.send_text("one");
    assert_eq!(c.next(), Some(Got::Text("one".into())));
    c.conn.send_binary(&[0, 1, 2, 255]).unwrap();
    c.flush();
    assert_eq!(c.next(), Some(Got::Binary(vec![0, 1, 2, 255])));
    // A message in two fragments arrives whole.
    c.conn.send_text_fragment("fr", false).unwrap();
    c.conn.send_text_fragment("ag", true).unwrap();
    c.flush();
    assert_eq!(c.next(), Some(Got::Text("frag".into())));
    assert_eq!(seen.upgraded.load(Ordering::SeqCst), 1);
    assert_eq!(seen.opened.load(Ordering::SeqCst), 1);
    assert_eq!(*seen.ends.lock().unwrap(), vec![RequestEnd::Upgraded]);
}

#[test]
fn test_ws_close_handshake_from_the_client() {
    let (seen, _server, addr) = start();
    let (_, mut c) = Client::open(addr, "/ws", &[], b"").unwrap();
    assert_eq!(c.next(), Some(Got::Text("hello".into())));
    c.conn.close(CloseCode::NORMAL, "done").unwrap();
    c.flush();
    assert_eq!(c.next(), Some(Got::Close(Some(1000))));
    assert_eq!(
        c.next(),
        None,
        "the server closes the socket after the handshake"
    );
    wait_for("closed", || !seen.closed.lock().unwrap().is_empty());
    assert_eq!(*seen.closed.lock().unwrap(), vec![Some(1000)]);
}

#[test]
fn test_ws_close_handshake_from_the_service() {
    let (seen, _server, addr) = start();
    let (_, mut c) = Client::open(addr, "/ws", &[], b"").unwrap();
    assert_eq!(c.next(), Some(Got::Text("hello".into())));
    c.send_text("close");
    assert_eq!(c.next(), Some(Got::Close(Some(1000))));
    // The client's answer ends it.
    c.flush();
    assert_eq!(c.next(), None);
    wait_for("closed", || !seen.closed.lock().unwrap().is_empty());
    assert_eq!(*seen.closed.lock().unwrap(), vec![Some(1000)]);
}

#[test]
fn test_ws_bytes_after_the_head_belong_to_the_websocket() {
    // A frame sent in the same write as the opening request: never parsed
    // as HTTP, delivered as the first message.
    let (_seen, _server, addr) = start();
    let mut pre = Connection::client_with_seed([9; 32]);
    pre.send_text("early").unwrap();
    let frame = pre.output().to_vec();
    let (_, mut c) = Client::open(addr, "/ws", &[], &frame).unwrap();
    assert_eq!(c.next(), Some(Got::Text("hello".into())));
    assert_eq!(c.next(), Some(Got::Text("early".into())));
}

#[test]
fn test_ws_bad_key_refused_400() {
    // T-21: an invalid handshake never gets a 101.
    let (seen, _server, addr) = start();
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(
        b"GET /ws HTTP/1.1\r\nHost: a\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
          Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: not-base64\r\n\r\n",
    )
    .unwrap();
    let mut got = Vec::new();
    let _ = s.read_to_end(&mut got);
    let text = String::from_utf8_lossy(&got);
    assert!(text.starts_with("HTTP/1.1 400"), "{text}");
    assert_eq!(seen.upgraded.load(Ordering::SeqCst), 0);
}

#[test]
fn test_ws_wrong_version_refused() {
    let (seen, _server, addr) = start();
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(
        b"GET /ws HTTP/1.1\r\nHost: a\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
          Sec-WebSocket-Version: 8\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
    )
    .unwrap();
    let mut got = Vec::new();
    let _ = s.read_to_end(&mut got);
    assert!(String::from_utf8_lossy(&got).starts_with("HTTP/1.1 400"));
    assert_eq!(seen.upgraded.load(Ordering::SeqCst), 0);
}

#[test]
fn test_ws_upgrade_the_request_did_not_offer_is_500() {
    // T-21: the service asked for a protocol the client never offered.
    let (seen, _server, addr) = start();
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"GET /bug HTTP/1.1\r\nHost: a\r\n\r\n")
        .unwrap();
    let mut got = Vec::new();
    let _ = s.read_to_end(&mut got);
    assert!(String::from_utf8_lossy(&got).starts_with("HTTP/1.1 500"));
    assert_eq!(seen.upgraded.load(Ordering::SeqCst), 0);
}

#[test]
fn test_ws_h2c_offer_is_served_as_http11() {
    let (_seen, _server, addr) = start();
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(
        b"GET /plain HTTP/1.1\r\nHost: a\r\nConnection: Upgrade, HTTP2-Settings\r\n\
          Upgrade: h2c\r\nHTTP2-Settings: AAMAAABkAAQAoAAA\r\n\r\n",
    )
    .unwrap();
    let mut buf = [0u8; 1024];
    let n = s.read(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf[..n]);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.ends_with("plain"), "{text}");
}

#[test]
fn test_ws_subprotocol_named_in_the_101() {
    let (_seen, _server, addr) = start();
    let (head, _c) = Client::open(addr, "/ws-chat", &["other", "chat"], b"").unwrap();
    assert!(
        head.to_ascii_lowercase()
            .contains("sec-websocket-protocol: chat"),
        "{head}"
    );
    // Not offered: a service bug, answered 500.
    let refused = Client::open(addr, "/ws-chat", &["other"], b"")
        .err()
        .unwrap();
    assert!(refused.starts_with("HTTP/1.1 500"), "{refused}");
}

#[test]
fn test_ws_protocol_violation_closes_1002() {
    let (seen, _server, addr) = start();
    let (_, mut c) = Client::open(addr, "/ws", &[], b"").unwrap();
    assert_eq!(c.next(), Some(Got::Text("hello".into())));
    // An unmasked text frame from a client.
    c.s.write_all(&[0x81, 0x02, b'h', b'i']).unwrap();
    assert_eq!(c.next(), Some(Got::Close(Some(1002))));
    assert_eq!(c.next(), None);
    wait_for("closed", || !seen.closed.lock().unwrap().is_empty());
    assert_eq!(*seen.closed.lock().unwrap(), vec![Some(1002)]);
}

#[test]
fn test_ws_message_over_the_budget_closes_1009() {
    let (_seen, _server, addr) = start();
    let (_, mut c) = Client::open(addr, "/ws-small", &[], b"").unwrap();
    assert_eq!(c.next(), Some(Got::Text("hello".into())));
    c.send_text(&"x".repeat(2000));
    assert_eq!(c.next(), Some(Got::Close(Some(1009))));
}

#[test]
fn test_ws_panic_closes_1011_and_the_server_lives() {
    let (seen, _server, addr) = start();
    let (_, mut c) = Client::open(addr, "/ws", &[], b"").unwrap();
    assert_eq!(c.next(), Some(Got::Text("hello".into())));
    c.send_text("panic");
    assert_eq!(c.next(), Some(Got::Close(Some(1011))));
    c.flush();
    wait_for("closed", || !seen.closed.lock().unwrap().is_empty());
    let (_, mut again) = Client::open(addr, "/ws", &[], b"").unwrap();
    assert_eq!(again.next(), Some(Got::Text("hello".into())));
}

#[test]
fn test_ws_quiet_connection_is_pinged_then_closed() {
    let (seen, _server, addr) = start();
    let (_, mut c) = Client::open(addr, "/ws-quiet", &[], b"").unwrap();
    assert_eq!(c.next(), Some(Got::Text("hello".into())));
    let started = Instant::now();
    // Our client does not answer pings on its own (it only queues the Pong).
    assert_eq!(c.next(), Some(Got::Ping));
    assert!(started.elapsed() >= Duration::from_millis(800));
    let queued = c.conn.output().len();
    c.conn.advance_output(queued);
    assert_eq!(c.next(), None, "a second quiet interval closes it");
    wait_for("closed", || !seen.closed.lock().unwrap().is_empty());
    assert_eq!(*seen.closed.lock().unwrap(), vec![None]);
}

#[test]
fn test_ws_pong_keeps_a_quiet_connection_open() {
    let (_seen, _server, addr) = start();
    let (_, mut c) = Client::open(addr, "/ws-quiet", &[], b"").unwrap();
    assert_eq!(c.next(), Some(Got::Text("hello".into())));
    for _ in 0..2 {
        assert_eq!(c.next(), Some(Got::Ping));
        c.flush(); // the queued Pong
    }
    c.send_text("still here");
    assert_eq!(c.next(), Some(Got::Text("still here".into())));
}

#[test]
fn test_ws_client_that_does_not_read_is_held_to_the_budget() {
    let (seen, _server, addr) = start();
    let (_, mut c) = Client::open(addr, "/ws-tight", &[], b"").unwrap();
    assert_eq!(c.next(), Some(Got::Text("hello".into())));
    c.send_text("flood");
    wait_for("a refused send", || {
        seen.refused_full.load(Ordering::SeqCst) > 0
    });
}

#[test]
fn test_ws_drain_closes_the_connection() {
    let (seen, server, addr) = start();
    let (_, mut c) = Client::open(addr, "/ws", &[], b"").unwrap();
    assert_eq!(c.next(), Some(Got::Text("hello".into())));
    let report = server.drain(Duration::from_secs(5));
    assert_eq!(c.next(), None);
    assert_eq!(report.killed, 0, "{report:?}");
    assert_eq!(*seen.closed.lock().unwrap(), vec![None]);
}

#[test]
fn test_ws_plain_requests_still_served() {
    let (_seen, _server, addr) = start();
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"GET /plain HTTP/1.1\r\nHost: a\r\n\r\n")
        .unwrap();
    let mut buf = [0u8; 1024];
    let n = s.read(&mut buf).unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).ends_with("plain"));
}

/// Denies every service call once revoked.
struct Revoke(Arc<AtomicBool>);

impl Checkpoint for Revoke {
    fn check(&self, _env: &Envelope, at: CheckpointAt) -> Verdict {
        if at == CheckpointAt::BeforeDispatch && self.0.load(Ordering::Acquire) {
            Verdict::Deny { status: 403 }
        } else {
            Verdict::Pass
        }
    }
}

#[test]
fn test_ws_checkpoint_revokes_an_open_connection_1008() {
    // Each message passes the checkpoint a request would: a session revoked
    // while the socket is open stops at the next message.
    let seen = Arc::new(Seen::default());
    let revoked = Arc::new(AtomicBool::new(false));
    let app = App {
        seen: Arc::clone(&seen),
        echo: Arc::new(Echo(Arc::clone(&seen))),
    };
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .checkpoint(Revoke(Arc::clone(&revoked)))
        .serve(app)
        .unwrap();
    let (_, mut c) = Client::open(server.local_addr().unwrap(), "/ws", &[], b"").unwrap();
    assert_eq!(c.next(), Some(Got::Text("hello".into())));
    c.send_text("allowed");
    assert_eq!(c.next(), Some(Got::Text("allowed".into())));
    revoked.store(true, Ordering::Release);
    c.send_text("not any more");
    assert_eq!(c.next(), Some(Got::Close(Some(1008))));
    c.flush();
    assert_eq!(c.next(), None);
    wait_for("closed", || !seen.closed.lock().unwrap().is_empty());
    assert_eq!(*seen.closed.lock().unwrap(), vec![Some(1008)]);
}

/// T-21, found in the 0.7 review: input past the HTTP engine's buffer in the
/// read that carries the upgrade goes to ws-net, never back to the HTTP
/// parser. Before, a request smuggled after 64 KiB of frames was parsed and
/// answered as HTTP, with the client's own `X-Forwarded-For` believed.
#[test]
fn test_ws_input_past_the_engine_buffer_is_never_parsed_as_http() {
    use serve_net::testing::TestConnection;
    use serve_net::{ProxySource, TrustedProxies};
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Quiet;
    impl WebSocketService for Quiet {
        fn message(&self, _e: &Envelope, _m: WsMessage<'_>, _c: &mut WebSocketConn<'_>) {}
    }
    struct Site(Arc<AtomicU32>);
    impl HttpService for Site {
        fn head(&self, head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
            if head.path() == "/ws" {
                HeadAction::Upgrade(WebSocketTarget::new(Quiet).into())
            } else {
                HeadAction::Proceed
            }
        }
        fn call(&self, req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
            self.0.fetch_add(1, Ordering::SeqCst);
            reply.text(200, &format!("served {}", req.path()));
            Outcome::Done
        }
    }
    fn frame(len: usize) -> Vec<u8> {
        let mut f = vec![0x82, 0x80 | len as u8, 0, 0, 0, 0];
        f.extend(std::iter::repeat_n(b'p', len));
        f
    }
    for behind in [false, true] {
        let calls = Arc::new(AtomicU32::new(0));
        let site = Site(Arc::clone(&calls));
        let mut conn = if behind {
            TestConnection::behind(
                site,
                ProxySource::forwarded_for(TrustedProxies::loopback()),
                serve_net::Profile::new(serve_net::ProfileId(0)),
            )
        } else {
            TestConnection::new(site)
        };
        let mut bytes = b"GET /ws HTTP/1.1\r\nHost: a\r\nX-Forwarded-For: 198.51.100.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n".to_vec();
        // Frames that fill the engine's 64 KiB buffer exactly.
        let cap = 64 * 1024;
        while cap - bytes.len() > 131 + 6 {
            bytes.extend(frame(125));
        }
        let rest = cap - bytes.len();
        bytes.extend(frame(rest - 6));
        bytes.extend_from_slice(
            b"GET /smuggled HTTP/1.1\r\nHost: a\r\nX-Forwarded-For: 10.9.9.9, 127.0.0.1\r\n\r\n",
        );
        let mut out = Vec::new();
        conn.send_into(&bytes, &mut out);
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.starts_with("HTTP/1.1 101"),
            "behind={behind}: {text:.80}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "behind={behind}: an HTTP call after the 101"
        );
        assert!(!text.contains("served"), "behind={behind}: {text}");
    }
}

/// Found in the 0.7 review: any byte refreshed the quiet timer, so a client
/// could keep one message part-way for ever, holding up to the message limit
/// of buffer. A message now has `message_timeout` from its first byte to its
/// last; past it the connection closes with 1008.
#[test]
fn test_ws_message_trickled_past_its_time_closes_1008() {
    let (_seen, _server, addr) = start();
    let (_, mut c) = Client::open(addr, "/ws-slowmsg", &[], b"").unwrap();
    assert_eq!(c.next(), Some(Got::Text("hello".into())));
    // A text frame without FIN, then one byte of continuation every 300 ms:
    // the connection is never quiet, the message never ends.
    let mut w = c.s.try_clone().unwrap();
    let started = Instant::now();
    let trickle = std::thread::spawn(move || {
        if w.write_all(&[0x01, 0x81, 0, 0, 0, 0, b'a']).is_err() {
            return;
        }
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(300));
            if w.write_all(&[0x00, 0x81, 0, 0, 0, 0, b'b']).is_err() {
                return;
            }
        }
    });
    let mut got = None;
    while let Some(g) = c.next() {
        if matches!(g, Got::Close(_)) {
            got = Some(g);
            break;
        }
    }
    let took = started.elapsed();
    assert_eq!(got, Some(Got::Close(Some(1008))));
    assert!(took < Duration::from_secs(5), "closed after {took:?}");
    let _ = trickle.join();
}
