//! `WsSender::close` from another thread, on both drivers: the Close goes
//! out after what was queued before it, the connection ends with the close
//! handshake, a repeat changes nothing, a close after the client closed is
//! harmless, and a client that never answers, or never reads, is closed
//! after the wait.

#![cfg(feature = "ws")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serve_net::{
    ConnectionId, EndReason, Envelope, HeadAction, HttpHead, HttpReply, HttpRequest, HttpService,
    ListenerSpec, Observer, Outcome, Server, WebSocketConn, WebSocketService, WebSocketTarget,
    Workers, WsMessage, WsSendError, WsSender,
};
use ws_net::{ClientHandshake, CloseCode, Connection, Event};

/// What the service saw.
#[derive(Default)]
struct Seen {
    opened: AtomicU32,
    sender: Mutex<Option<WsSender>>,
    closed: Mutex<Vec<Option<u16>>>,
    ends: Mutex<Vec<EndReason>>,
}

/// Echoes every text; keeps the sender of the connection it opened.
struct Echo(Arc<Seen>);

impl WebSocketService for Echo {
    fn opened(&self, _env: &Envelope, conn: &mut WebSocketConn<'_>) {
        *self.0.sender.lock().unwrap() = Some(conn.sender());
        self.0.opened.fetch_add(1, Ordering::SeqCst);
    }

    fn message(&self, _env: &Envelope, message: WsMessage<'_>, conn: &mut WebSocketConn<'_>) {
        if let WsMessage::Text(t) = message {
            let _ = conn.send_text(t);
        }
    }

    fn closed(&self, _env: &Envelope, code: Option<u16>) {
        self.0.closed.lock().unwrap().push(code);
    }
}

struct Ends(Arc<Seen>);

impl Observer for Ends {
    fn connection_ended(&self, _connection: ConnectionId, end: EndReason, _requests: u32) {
        self.0
            .ends
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(end);
    }
}

struct App(Arc<Seen>);

impl HttpService for App {
    fn head(&self, head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
        let echo: Arc<dyn WebSocketService> = Arc::new(Echo(Arc::clone(&self.0)));
        let target = WebSocketTarget::shared(echo).ping_interval(Duration::from_secs(120));
        match head.path() {
            "/ws-tight" => HeadAction::Upgrade(target.max_pending_output(64 * 1024).into()),
            _ => HeadAction::Upgrade(target.into()),
        }
    }

    fn call(&self, _req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
        reply.text(404, "");
        Outcome::Done
    }
}

fn start(workers: Workers) -> (Arc<Seen>, serve_net::Running, SocketAddr) {
    let seen = Arc::new(Seen::default());
    let server = Server::builder()
        .workers(workers)
        .observer(Ends(Arc::clone(&seen)))
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .serve(App(Arc::clone(&seen)))
        .unwrap();
    let addr = server.local_addr().unwrap();
    (seen, server, addr)
}

fn drivers() -> [Workers; 2] {
    [Workers::auto(), Workers::blocking(2)]
}

#[derive(Debug, PartialEq, Eq)]
enum Got {
    Text(String),
    Close(Option<u16>, String),
}

struct Client {
    s: TcpStream,
    conn: Connection,
}

impl Client {
    fn open(addr: SocketAddr, path: &str) -> Client {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        let hs = ClientHandshake::with_nonce(*b"serve-net-nonce!");
        let mut req = Vec::new();
        hs.write_request("127.0.0.1", path, &mut req).unwrap();
        s.write_all(&req).unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        let end = loop {
            if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                break end + 4;
            }
            let n = s.read(&mut buf).unwrap();
            assert!(n > 0, "the server closed before the 101");
            got.extend_from_slice(&buf[..n]);
        };
        assert!(got.starts_with(b"HTTP/1.1 101"));
        let mut conn = Connection::client_with_seed([7; 32]);
        let _ = conn.feed(&got[end..]);
        Client { s, conn }
    }

    fn flush(&mut self) {
        let out = self.conn.output().to_vec();
        self.s.write_all(&out).unwrap();
        self.conn.advance_output(out.len());
    }

    /// The next message or Close, owned; `None` once the socket closed.
    fn next(&mut self) -> Option<Got> {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match self.conn.poll() {
                Ok(Event::Text(t)) => return Some(Got::Text(t.to_owned())),
                Ok(Event::Close(f)) => {
                    return Some(match f {
                        Some(f) => Got::Close(Some(f.code.as_u16()), f.reason.to_owned()),
                        None => Got::Close(None, String::new()),
                    });
                }
                Ok(Event::Closed) => return None,
                Ok(Event::NeedInput) => {}
                Ok(_) => continue,
                Err(e) => panic!("the server sent something invalid: {e:?}"),
            }
            match self.s.read(&mut buf) {
                Ok(n @ 1..) => {
                    let _ = self.conn.feed(&buf[..n]);
                }
                _ => return None,
            }
        }
    }

    /// Whether the server closed the socket: reads until end of stream or
    /// reset, dropping what comes.
    fn sees_end(&mut self) -> bool {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match self.s.read(&mut buf) {
                Ok(0) | Err(_) => return true,
                Ok(_) => {}
            }
        }
    }
}

fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !cond() {
        assert!(Instant::now() < deadline, "{what} never happened");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn sender(seen: &Seen) -> WsSender {
    wait_for("opened", || seen.opened.load(Ordering::SeqCst) == 1);
    seen.sender.lock().unwrap().clone().unwrap()
}

#[test]
fn test_ws_sender_close_ends_with_the_handshake() {
    for workers in drivers() {
        let (seen, _server, addr) = start(workers);
        let mut c = Client::open(addr, "/ws");
        let sender = sender(&seen);
        // From another thread: two messages, then the close, then a repeat
        // and a send, both too late to change anything.
        let closer = std::thread::spawn(move || {
            sender.send_text("one").unwrap();
            sender.send_text("two").unwrap();
            sender.close(4001, "revoked").unwrap();
            assert_eq!(sender.close(4002, "again"), Ok(()));
            assert_eq!(sender.send_text("late"), Err(WsSendError::Closed));
            assert!(sender.is_closed());
            sender
        });
        let sender = closer.join().unwrap();
        assert_eq!(c.next(), Some(Got::Text("one".into())));
        assert_eq!(c.next(), Some(Got::Text("two".into())));
        assert_eq!(c.next(), Some(Got::Close(Some(4001), "revoked".into())));
        // The client's answer completes the handshake and the server closes.
        c.flush();
        assert_eq!(c.next(), None);
        wait_for("closed", || !seen.closed.lock().unwrap().is_empty());
        assert_eq!(*seen.closed.lock().unwrap(), vec![Some(4001)]);
        wait_for("ended", || !seen.ends.lock().unwrap().is_empty());
        assert_eq!(*seen.ends.lock().unwrap(), vec![EndReason::ServiceClosed]);
        assert_eq!(sender.close(1000, ""), Ok(()));
    }
}

#[test]
fn test_ws_sender_close_from_many_threads_sends_one_close() {
    for workers in drivers() {
        let (seen, _server, addr) = start(workers);
        let mut c = Client::open(addr, "/ws");
        let sender = sender(&seen);
        let threads: Vec<_> = (0..8u16)
            .map(|i| {
                let sender = sender.clone();
                std::thread::spawn(move || sender.close(4000 + i, "bye"))
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), Ok(()));
        }
        let Some(Got::Close(Some(code), reason)) = c.next() else {
            panic!("no Close");
        };
        assert!((4000..4008).contains(&code), "{code}");
        assert_eq!(reason, "bye");
        c.flush();
        // Exactly one Close: the socket ends next.
        assert_eq!(c.next(), None);
        wait_for("closed", || !seen.closed.lock().unwrap().is_empty());
        assert_eq!(*seen.closed.lock().unwrap(), vec![Some(code)]);
    }
}

#[test]
fn test_ws_sender_close_after_the_client_closed_is_harmless() {
    for workers in drivers() {
        let (seen, _server, addr) = start(workers);
        let mut c = Client::open(addr, "/ws");
        let sender = sender(&seen);
        c.conn.close(CloseCode::NORMAL, "done").unwrap();
        c.flush();
        // The server answered the client's Close; a close from a sender
        // after that changes nothing.
        assert_eq!(c.next(), Some(Got::Close(Some(1000), String::new())));
        assert_eq!(sender.close(4001, "revoked"), Ok(()));
        assert_eq!(c.next(), None);
        wait_for("closed", || !seen.closed.lock().unwrap().is_empty());
        assert_eq!(sender.close(4001, "revoked"), Ok(()));
        assert!(sender.is_closed());
        // The client's close stands.
        assert_eq!(*seen.closed.lock().unwrap(), vec![Some(1000)]);
    }
}

#[test]
fn test_ws_sender_close_refuses_a_bad_code_or_reason() {
    for workers in drivers() {
        let (seen, _server, addr) = start(workers);
        let mut c = Client::open(addr, "/ws");
        let sender = sender(&seen);
        for code in [0, 999, 1004, 1005, 1006, 1015, 2999, 5000] {
            assert_eq!(sender.close(code, ""), Err(WsSendError::Invalid), "{code}");
        }
        assert_eq!(
            sender.close(1000, &"x".repeat(124)),
            Err(WsSendError::Invalid)
        );
        // Nothing was queued: the connection is open as before.
        assert!(!sender.is_closed());
        c.conn.send_text("still open").unwrap();
        c.flush();
        assert_eq!(c.next(), Some(Got::Text("still open".into())));
        assert_eq!(sender.close(1000, &"x".repeat(123)), Ok(()));
        assert_eq!(c.next(), Some(Got::Close(Some(1000), "x".repeat(123))));
    }
}

#[test]
fn test_ws_sender_close_ends_a_client_that_does_not_answer() {
    for workers in drivers() {
        let (seen, _server, addr) = start(workers);
        let mut c = Client::open(addr, "/ws");
        let sender = sender(&seen);
        let started = Instant::now();
        sender.close(1008, "revoked").unwrap();
        assert_eq!(c.next(), Some(Got::Close(Some(1008), "revoked".into())));
        // No answer: the server waits out its 5 s and closes the socket.
        let queued = c.conn.output().len();
        c.conn.advance_output(queued);
        assert!(c.sees_end());
        let waited = started.elapsed();
        assert!(waited >= Duration::from_secs(4), "{waited:?}");
        wait_for("closed", || !seen.closed.lock().unwrap().is_empty());
        assert_eq!(*seen.closed.lock().unwrap(), vec![Some(1008)]);
    }
}

#[test]
fn test_ws_sender_close_ends_a_client_that_does_not_read() {
    for workers in drivers() {
        let (seen, _server, addr) = start(workers);
        let mut c = Client::open(addr, "/ws-tight");
        let sender = sender(&seen);
        // The client reads nothing: fill the socket buffers, ws-net's
        // output, and the queue, so messages wait ahead of the close.
        let big = vec![b'x'; 16 * 1024];
        let mut full = false;
        for _ in 0..10_000 {
            match sender.send_binary(&big) {
                Ok(()) => {}
                Err(WsSendError::Full) => {
                    full = true;
                    break;
                }
                Err(e) => panic!("{e:?}"),
            }
        }
        assert!(full, "the queue never filled");
        sender.close(1008, "revoked").unwrap();
        // The close cannot go out; the wait ends the connection anyway.
        wait_for("closed", || !seen.closed.lock().unwrap().is_empty());
        wait_for("ended", || !seen.ends.lock().unwrap().is_empty());
        assert_eq!(*seen.ends.lock().unwrap(), vec![EndReason::ServiceClosed]);
        assert!(c.sees_end());
    }
}
