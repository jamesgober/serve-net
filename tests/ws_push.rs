//! Messages a `WsSender` queues go out while the client is sending too: a
//! client sends 200 messages without reading while another thread sends
//! each one back, over ws and wss, on both drivers. Before 0.8.3 the
//! replies could wait for the next ping.

#![cfg(feature = "ws")]

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use serve_net::{
    ConnectionId, EndReason, Envelope, HeadAction, HttpHead, HttpReply, HttpRequest, HttpService,
    Ingress, ListenerSpec, Observer, Outcome, Server, WebSocketConn, WebSocketService,
    WebSocketTarget, Workers, WsMessage, WsSendError,
};
use tls_net::{
    ClientBuilder, ClientSession, Event as TlsEvent, KeySource, TlsServer, TrustAnchors,
};
use ws_net::{ClientHandshake, Connection, Event};

const SITE_PEM: &[u8] = include_bytes!("data/site.pem");
const SITE_KEY: &[u8] = include_bytes!("data/site.key");
const CA_PEM: &[u8] = include_bytes!("data/ca.pem");

/// What the client sends, and the server sends back.
const PUSHES: u32 = 200;
/// Far past how long the pushes take, and far short of the ping interval,
/// so a push that waited for the ping fails the test.
const PATIENCE: Duration = Duration::from_secs(20);

/// Sends every message back from another thread through a `WsSender`, as
/// a bridge does with its upstream's replies.
#[derive(Default)]
struct Pusher {
    received: AtomicU32,
    closed: AtomicU32,
    to_pusher: Mutex<Option<mpsc::Sender<String>>>,
}

impl WebSocketService for Pusher {
    fn opened(&self, _env: &Envelope, conn: &mut WebSocketConn<'_>) {
        let sender = conn.sender();
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            // "<i> <pad>": the reply is "message <i>" and `pad` bytes more.
            for text in rx {
                let (i, pad) = text.split_once(' ').unwrap();
                let pad: usize = pad.parse().unwrap();
                let reply = format!("message {i} {}", "x".repeat(pad));
                // A full queue is the client not keeping up: try again.
                loop {
                    match sender.send_text(&reply) {
                        Ok(()) => break,
                        Err(WsSendError::Full) => std::thread::sleep(Duration::from_millis(1)),
                        Err(_) => return,
                    }
                }
            }
        });
        *self.to_pusher.lock().unwrap() = Some(tx);
    }

    fn message(&self, _env: &Envelope, message: WsMessage<'_>, _conn: &mut WebSocketConn<'_>) {
        self.received.fetch_add(1, Ordering::Relaxed);
        if let WsMessage::Text(t) = message {
            if let Some(tx) = self.to_pusher.lock().unwrap().as_ref() {
                let _ = tx.send(t.to_owned());
            }
        }
    }

    fn closed(&self, _env: &Envelope, _code: Option<u16>) {
        self.closed.fetch_add(1, Ordering::AcqRel);
    }
}

/// How the server's connections ended, for a failure's message.
static ENDS: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Ends;

impl Observer for Ends {
    fn connection_ended(&self, connection: ConnectionId, end: EndReason, _requests: u32) {
        ENDS.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{connection:?}: {end:?}"));
    }
}

/// The connection broke: fails with how the server ended its connections.
fn broke(what: &dyn std::fmt::Display) -> ! {
    std::thread::sleep(Duration::from_millis(200));
    let ends = ENDS.lock().unwrap_or_else(PoisonError::into_inner).clone();
    panic!("{what}; the server's ends: {ends:?}");
}

struct App(Arc<Pusher>);

impl HttpService for App {
    fn head(&self, _head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
        let target = WebSocketTarget::shared(Arc::clone(&self.0) as Arc<dyn WebSocketService>)
            .ping_interval(Duration::from_secs(120));
        HeadAction::Upgrade(target.into())
    }

    fn call(&self, _req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
        reply.text(404, "");
        Outcome::Done
    }
}

fn serve(tls: bool, workers: Workers) -> (Arc<Pusher>, serve_net::Running, SocketAddr) {
    let pusher = Arc::new(Pusher::default());
    let app = App(Arc::clone(&pusher));
    let listener = ListenerSpec::tcp("web", "127.0.0.1:0");
    let builder = Server::builder().workers(workers).observer(Ends);
    let server = if tls {
        let acceptor = TlsServer::builder()
            .certificate(SITE_PEM, KeySource::Pem(SITE_KEY))
            .unwrap()
            .build()
            .unwrap();
        builder
            .listener(listener.ingress(Ingress::DirectTls))
            .tls(acceptor)
            .serve(app)
            .unwrap()
    } else {
        builder.listener(listener).serve(app).unwrap()
    };
    let addr = server.local_addr().unwrap();
    (pusher, server, addr)
}

/// A socket with a short read timeout, plain or TLS: `recv` returns what
/// arrived, or nothing when nothing did in a millisecond or so.
enum Wire {
    Plain(TcpStream),
    Tls {
        sock: TcpStream,
        session: Box<ClientSession>,
        inbuf: Vec<u8>,
        filled: usize,
        out: Vec<u8>,
    },
}

fn quiet(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

impl Wire {
    fn connect(addr: SocketAddr, tls: bool) -> Wire {
        let sock = TcpStream::connect(addr).unwrap();
        sock.set_nodelay(true).unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(2)))
            .unwrap();
        if !tls {
            return Wire::Plain(sock);
        }
        let client = ClientBuilder::new()
            .trust(TrustAnchors::from_pem(CA_PEM).unwrap())
            .build()
            .unwrap();
        let mut wire = Wire::Tls {
            sock,
            session: Box::new(client.connect("example.test").unwrap()),
            inbuf: vec![0; 64 * 1024],
            filled: 0,
            out: vec![0; 64 * 1024],
        };
        let deadline = Instant::now() + PATIENCE;
        while let Wire::Tls { session, .. } = &wire {
            if session.info().is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the TLS handshake never finished"
            );
            let _ = wire.recv();
        }
        wire
    }

    fn send(&mut self, bytes: &[u8]) {
        match self {
            Wire::Plain(s) => s.write_all(bytes).unwrap_or_else(|e| broke(&e)),
            Wire::Tls {
                sock, session, out, ..
            } => {
                for chunk in bytes.chunks(8 * 1024) {
                    let n = session.encrypt(chunk, out).unwrap_or_else(|e| broke(&e));
                    sock.write_all(&out[..n]).unwrap_or_else(|e| broke(&e));
                }
            }
        }
    }

    /// Plaintext that arrived, possibly none.
    fn recv(&mut self) -> Vec<u8> {
        match self {
            Wire::Plain(s) => {
                let mut buf = vec![0; 64 * 1024];
                match s.read(&mut buf) {
                    Ok(0) => broke(&"the server closed the connection"),
                    Ok(n) => {
                        buf.truncate(n);
                        buf
                    }
                    Err(e) if quiet(&e) => Vec::new(),
                    Err(e) => broke(&e),
                }
            }
            Wire::Tls {
                sock,
                session,
                inbuf,
                filled,
                out,
            } => {
                let mut plain = Vec::new();
                loop {
                    let p = session
                        .step(&mut inbuf[..*filled], out)
                        .unwrap_or_else(|e| broke(&format!("{e:?}")));
                    let (consumed, written) = (p.consumed, p.written);
                    let mut need = false;
                    match p.event {
                        TlsEvent::Data(d) => plain.extend_from_slice(d),
                        TlsEvent::NeedInput => need = true,
                        TlsEvent::PeerClosed | TlsEvent::Closed => {
                            broke(&"the server closed the connection")
                        }
                        _ => {}
                    }
                    if written > 0 {
                        sock.write_all(&out[..written])
                            .unwrap_or_else(|e| broke(&e));
                    }
                    inbuf.copy_within(consumed..*filled, 0);
                    *filled -= consumed;
                    if !(need && written == 0 && consumed == 0) {
                        continue;
                    }
                    if !plain.is_empty() {
                        return plain;
                    }
                    if *filled == inbuf.len() {
                        let want = session.input_capacity_required().max(*filled + 1);
                        inbuf.resize(want, 0);
                    }
                    match sock.read(&mut inbuf[*filled..]) {
                        Ok(0) => broke(&"the server closed the connection"),
                        Ok(n) => *filled += n,
                        Err(e) if quiet(&e) => return plain,
                        Err(e) => broke(&e),
                    }
                }
            }
        }
    }
}

/// Opens the WebSocket, sends `PUSHES` messages without reading, then
/// reads until every reply, `pad` bytes past its number, came back; returns
/// how many did.
fn stream_and_count(addr: SocketAddr, tls: bool, pad: usize) -> u32 {
    let mut wire = Wire::connect(addr, tls);
    let hs = ClientHandshake::with_nonce(*b"serve-net-nonce!");
    let mut req = Vec::new();
    hs.write_request("example.test", "/ws", &mut req).unwrap();
    wire.send(&req);
    let deadline = Instant::now() + PATIENCE;
    let mut got = Vec::new();
    let end = loop {
        if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
        assert!(Instant::now() < deadline, "no 101");
        got.extend(wire.recv());
    };
    assert!(
        got.starts_with(b"HTTP/1.1 101"),
        "{}",
        String::from_utf8_lossy(&got)
    );
    let mut conn = Connection::client_with_seed([7; 32]);
    // What arrived and ws-net has not taken yet.
    let mut held = got[end..].to_vec();

    // Every message goes out before any reply is read, so replies are
    // queued for the client while its own messages still arrive.
    for i in 0..PUSHES {
        conn.send_text(&format!("{i} {pad}")).unwrap();
        let out = conn.output().to_vec();
        conn.advance_output(out.len());
        wire.send(&out);
    }
    let mut back = 0u32;
    let deadline = Instant::now() + PATIENCE;
    while back < PUSHES && Instant::now() < deadline {
        let n = conn.feed(&held);
        held.drain(..n);
        loop {
            match conn.poll() {
                Ok(Event::Text(t)) => {
                    let want = format!("message {back} ");
                    assert!(t.starts_with(&want), "replies arrive in order");
                    assert_eq!(t.len(), want.len() + pad, "a whole reply");
                    back += 1;
                }
                Ok(Event::NeedInput) => break,
                Ok(Event::Closed | Event::Close(_)) => broke(&"the server closed"),
                Ok(_) => {}
                Err(e) => panic!("{e:?}"),
            }
        }
        if back < PUSHES && held.is_empty() {
            held = wire.recv();
        }
    }
    back
}

fn check(tls: bool, workers: Workers) {
    check_with(tls, workers, 0);
}

/// Replies big enough that the server waits for the client to take them,
/// so wakes arrive while it waits to write.
fn check_big(tls: bool, workers: Workers) {
    check_with(tls, workers, 32 * 1024);
}

fn check_with(tls: bool, workers: Workers, pad: usize) {
    let (pusher, server, addr) = serve(tls, workers);
    let got = stream_and_count(addr, tls, pad);
    assert_eq!(
        got,
        PUSHES,
        "only {got} of {PUSHES} replies arrived within {PATIENCE:?} (the server read {} messages)",
        pusher.received.load(Ordering::Relaxed)
    );
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_ws_pushes_go_out_while_the_client_sends() {
    check(false, Workers::auto());
}

#[test]
#[ignore = "waits on a net-io fix for TLS on the reactor; run by hand"]
fn test_wss_pushes_go_out_while_the_client_sends() {
    check(true, Workers::auto());
}

#[test]
fn test_ws_pushes_go_out_while_the_client_sends_blocking() {
    check(false, Workers::blocking(2));
}

#[test]
fn test_wss_pushes_go_out_while_the_client_sends_blocking() {
    check(true, Workers::blocking(2));
}

#[test]
fn test_ws_big_pushes_go_out_while_the_client_sends() {
    check_big(false, Workers::auto());
}

#[test]
#[ignore = "waits on a net-io fix for TLS on the reactor; run by hand"]
fn test_wss_big_pushes_go_out_while_the_client_sends() {
    check_big(true, Workers::auto());
}

#[test]
fn test_ws_big_pushes_go_out_while_the_client_sends_blocking() {
    check_big(false, Workers::blocking(2));
}

#[test]
fn test_wss_big_pushes_go_out_while_the_client_sends_blocking() {
    check_big(true, Workers::blocking(2));
}

/// One blocking worker serves two WebSockets in turn: the second waits for
/// posts while it reads too, though the worker made its waker for the first.
fn check_second_connection(tls: bool) {
    let (pusher, server, addr) = serve(tls, Workers::blocking(1));
    for round in 1..=2 {
        let got = stream_and_count(addr, tls, 0);
        assert_eq!(
            got,
            PUSHES,
            "connection {round}: only {got} of {PUSHES} replies arrived within {PATIENCE:?} (the server read {} messages)",
            pusher.received.load(Ordering::Relaxed)
        );
        // The one worker's seat comes back once the connection is gone;
        // a connect before then is refused busy, which is not this test.
        let deadline = Instant::now() + PATIENCE;
        while pusher.closed.load(Ordering::Acquire) < round {
            assert!(Instant::now() < deadline, "connection {round} never closed");
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_ws_pushes_reach_a_second_connection_on_one_worker() {
    check_second_connection(false);
}

#[test]
fn test_wss_pushes_reach_a_second_connection_on_one_worker() {
    check_second_connection(true);
}
