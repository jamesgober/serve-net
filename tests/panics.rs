//! Panics outside `call`: in `head`, in `done`, and in a WebSocket handler,
//! with `catch_service_panics` on and off. A panic costs its connection and
//! nothing else: admission comes back, `done` and `closed` run once, and the
//! worker is not left reported stalled.

#![cfg(feature = "http1")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serve_net::{
    Envelope, Finished, HeadAction, Health, HttpHead, HttpReply, HttpRequest, HttpService,
    InFlightCap, ListenerSpec, Outcome, Server, Workers,
};

/// `head` panics for `/head-boom`; `done` panics for `/boom`. Counts `done`.
#[derive(Clone, Default)]
struct Svc {
    done: Arc<AtomicU32>,
}

impl HttpService for Svc {
    fn head(&self, head: &HttpHead<'_>, env: &mut Envelope) -> HeadAction {
        if head.path() == "/head-boom" {
            panic!("head panicked on purpose");
        }
        env.set_user_word(u64::from(head.path() == "/boom"));
        HeadAction::Proceed
    }
    fn call(&self, req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
        reply.text(200, req.path());
        Outcome::Done
    }
    fn done(&self, env: &Envelope, _finished: &Finished) {
        self.done.fetch_add(1, Ordering::SeqCst);
        if env.user_word() == 1 {
            panic!("done panicked on purpose");
        }
    }
}

fn get(addr: SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: a\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut got = Vec::new();
    let _ = s.read_to_end(&mut got);
    String::from_utf8_lossy(&got).into_owned()
}

fn status(reply: &str) -> &str {
    reply.get(9..12).unwrap_or("<none>")
}

fn serve(catch: bool, svc: Svc) -> serve_net::Running {
    Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .workers(Workers::reactor(1))
        .catch_service_panics(catch)
        .admit(InFlightCap::new().per_worker(4))
        .serve(svc)
        .unwrap()
}

/// Waits until `f` holds, up to 10 s.
fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..1_000 {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// a panic in `done` gave its admission back to nobody, so four of them
/// under a cap of four shed every later request. Either way the option is
/// set, the permit comes back.
#[test]
fn test_a_panic_in_done_gives_its_admission_back() {
    for catch in [true, false] {
        let svc = Svc::default();
        let server = serve(catch, svc.clone());
        let addr = server.local_addr().unwrap();
        assert_eq!(status(&get(addr, "/ok")), "200");
        for _ in 0..8 {
            let _ = get(addr, "/boom");
        }
        for _ in 0..4 {
            let reply = get(addr, "/ok");
            assert_eq!(status(&reply), "200", "catch {catch}: {reply}");
        }
        assert_eq!(svc.done.load(Ordering::SeqCst), 13, "catch {catch}");
        let _ = server.drain(Duration::from_secs(2));
    }
}

/// under `catch_service_panics`, a panic in `head` is answered with the
/// 500 refusal instead of a reset; on or off, `done` still runs once for the
/// request, and the worker goes on serving.
#[test]
fn test_a_panic_in_head_is_answered_and_done_still_runs() {
    let svc = Svc::default();
    let server = serve(true, svc.clone());
    let addr = server.local_addr().unwrap();
    let reply = get(addr, "/head-boom");
    assert_eq!(status(&reply), "500", "{reply}");
    assert!(eventually(|| svc.done.load(Ordering::SeqCst) == 1));
    assert_eq!(status(&get(addr, "/ok")), "200");
    let _ = server.drain(Duration::from_secs(2));

    let svc = Svc::default();
    let server = serve(false, svc.clone());
    let addr = server.local_addr().unwrap();
    // Off: the connection is dropped, as for a panic in `call`.
    assert_eq!(get(addr, "/head-boom"), "");
    assert!(eventually(|| svc.done.load(Ordering::SeqCst) == 1));
    assert_eq!(status(&get(addr, "/ok")), "200");
    let _ = server.drain(Duration::from_secs(2));
}

/// a panic in `call` with the option off left the worker marked inside
/// a call, so an idle server reported it stalled and, with one worker,
/// `Health::Failing`.
#[test]
fn test_a_panic_in_call_does_not_leave_the_worker_reported_stalled() {
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .workers(Workers::reactor(1))
        .serve(serve_net::handler(|req, reply| {
            if req.path() == "/panic" {
                panic!("call panicked on purpose");
            }
            reply.text(200, "ok");
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    assert_eq!(status(&get(addr, "/ok")), "200");
    let _ = get(addr, "/panic");
    let _ = server.health();
    std::thread::sleep(Duration::from_millis(200));
    let health = server.health();
    assert!(matches!(health, Health::Ok), "{health:?}");
    let _ = server.drain(Duration::from_secs(2));
}

#[cfg(feature = "ws")]
mod websocket {
    use super::*;
    use serve_net::{WebSocketConn, WebSocketService, WebSocketTarget, WsMessage, WsSender};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Seen {
        opened: AtomicU32,
        closed: AtomicU32,
        sender: Mutex<Option<WsSender>>,
    }

    struct Chat(Arc<Seen>);

    impl WebSocketService for Chat {
        fn opened(&self, _env: &Envelope, conn: &mut WebSocketConn<'_>) {
            *self.0.sender.lock().unwrap() = Some(conn.sender());
            self.0.opened.fetch_add(1, Ordering::SeqCst);
        }
        fn message(&self, _env: &Envelope, message: WsMessage<'_>, conn: &mut WebSocketConn<'_>) {
            if message == WsMessage::Text("panic") {
                panic!("message handler panicked on purpose");
            }
            let _ = conn.send_text("ok");
        }
        fn closed(&self, _env: &Envelope, _code: Option<u16>) {
            self.0.closed.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct Site(Arc<Seen>);

    impl HttpService for Site {
        fn head(&self, head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
            if head.is_upgrade() {
                return HeadAction::Upgrade(
                    WebSocketTarget::shared(Arc::new(Chat(self.0.clone()))).into(),
                );
            }
            HeadAction::Proceed
        }
        fn call(&self, _req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
            reply.text(200, "page");
            Outcome::Done
        }
    }

    fn upgrade(addr: SocketAddr) -> TcpStream {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s.write_all(
            b"GET /chat HTTP/1.1\r\nHost: site.example\r\nUpgrade: websocket\r\n\
              Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
              Sec-WebSocket-Version: 13\r\nOrigin: http://site.example\r\n\r\n",
        )
        .unwrap();
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match s.read(&mut b) {
                Ok(1) => head.push(b[0]),
                _ => break,
            }
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        s
    }

    /// with the option off, a panic in `message` skipped `closed`,
    /// left every `WsSender` accepting sends into a dead queue, and kept the
    /// connection in the drain registry. With it on, the socket closes with
    /// 1011 and the same cleanup runs.
    #[test]
    fn test_a_panicking_websocket_handler_still_closes_its_senders() {
        for catch in [false, true] {
            let seen = Arc::new(Seen::default());
            let server = Server::builder()
                .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
                .workers(Workers::reactor(1))
                .catch_service_panics(catch)
                .serve(Site(seen.clone()))
                .unwrap();
            let addr = server.local_addr().unwrap();
            let mut s = upgrade(addr);
            assert!(eventually(|| seen.opened.load(Ordering::SeqCst) == 1));
            // A masked text frame "panic" with a zero masking key.
            s.write_all(&[0x81, 0x85, 0, 0, 0, 0, b'p', b'a', b'n', b'i', b'c'])
                .unwrap();
            let mut buf = [0u8; 64];
            let _ = s.read(&mut buf);
            drop(s);
            assert!(
                eventually(|| seen.closed.load(Ordering::SeqCst) == 1),
                "catch {catch}: closed() did not run"
            );
            let sender = seen.sender.lock().unwrap().clone().unwrap();
            assert!(sender.is_closed(), "catch {catch}");
            assert!(sender.send_text("into nothing").is_err(), "catch {catch}");
            // A drain does not wait on the dead connection's goodbye.
            let report = server.drain(Duration::from_secs(2));
            assert!(
                report.elapsed < Duration::from_millis(1_500),
                "catch {catch}: {report:?}"
            );
        }
    }
}
