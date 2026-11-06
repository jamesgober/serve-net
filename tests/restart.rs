//! T-30: a restart through a shared listening socket refuses and resets
//! nothing. T-20 and
//! the head-read signal sit here too: both need a real server's view of a
//! request.

#![cfg(feature = "http1")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};

use serve_net::testing::TestConnection;
use serve_net::{Envelope, Finished, HttpReply, HttpRequest, HttpService, Outcome};

#[test]
fn test_upgrade_h2c_served_as_http11() {
    let site = serve_net::handler(|_req, reply| {
        reply.text(200, "plain");
    });
    let reply = serve_net::testing::respond(
        &site,
        b"GET / HTTP/1.1\r\nHost: x\r\nConnection: Upgrade, HTTP2-Settings\r\nUpgrade: h2c\r\nHTTP2-Settings: AAMAAABkAAQAAP__\r\n\r\n",
    );
    assert_eq!(reply.status, 200);
    assert_eq!(reply.text(), "plain");
}

struct Reads(Arc<AtomicU16>);

impl HttpService for Reads {
    fn call(&self, _r: &HttpRequest<'_>, _e: &Envelope, reply: &mut HttpReply) -> Outcome {
        reply.text(200, "ok");
        Outcome::Done
    }
    fn done(&self, _e: &Envelope, finished: &Finished) {
        self.0.store(finished.head_reads, Ordering::Relaxed);
    }
}

#[test]
fn test_head_reads_counts_the_reads_of_a_head() {
    let reads = Arc::new(AtomicU16::new(0));
    let mut conn = TestConnection::new(Reads(Arc::clone(&reads)));
    let _ = conn.send(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    // The next head of the connection starts counting again.
    let _ = conn.send(b"GET / HT");
    let _ = conn.send(b"TP/1.1\r\nHo");
    let _ = conn.send(b"st: x\r\n\r\n");
    assert_eq!(reads.load(Ordering::Relaxed), 3);
}

struct Ends(Arc<std::sync::Mutex<Vec<serve_net::RequestEnd>>>);

impl HttpService for Ends {
    fn call(&self, req: &HttpRequest<'_>, _e: &Envelope, reply: &mut HttpReply) -> Outcome {
        assert_ne!(req.path(), "/boom", "a service bug");
        reply.text(200, "ok");
        Outcome::Done
    }
    fn done(&self, _e: &Envelope, finished: &Finished) {
        self.0.lock().unwrap().push(finished.end);
    }
}

#[test]
fn test_panic_caught_500_and_done_called() {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use serve_net::{ListenerSpec, RequestEnd, Server};
    let ends = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .catch_service_panics(true)
        .serve(Ends(Arc::clone(&ends)))
        .unwrap();
    let addr = server.local_addr().unwrap();
    let ask = |path: &str| {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        write!(
            s,
            "GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut got = String::new();
        let _ = s.read_to_string(&mut got);
        got
    };
    assert!(ask("/boom").starts_with("HTTP/1.1 500"));
    assert!(ask("/ok").starts_with("HTTP/1.1 200"));
    let _ = server.drain(std::time::Duration::from_secs(2));
    let ends = ends.lock().unwrap();
    assert_eq!(ends[0], RequestEnd::ServicePanic);
    assert_eq!(ends[1], RequestEnd::Replied);
}

mod restart {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serve_net::{ListenerSpec, Server};

    const GET: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";

    /// One request on a new connection; a whole 200, or what went wrong.
    fn one_request(addr: SocketAddr) -> Result<(), String> {
        let mut s = TcpStream::connect(addr).map_err(|e| format!("connect: {e}"))?;
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        s.write_all(GET).map_err(|e| format!("write: {e}"))?;
        let mut got = Vec::new();
        match s.read_to_end(&mut got) {
            Ok(_) if got.starts_with(b"HTTP/1.1 200") => Ok(()),
            Ok(_) => Err(format!("reply: {}", String::from_utf8_lossy(&got))),
            Err(e) => Err(format!("read after {} bytes: {e}", got.len())),
        }
    }

    /// Two handles on one listening socket, as a supervisor holds it.
    fn shared_socket() -> (SocketAddr, net_io::ListenerHandle, net_io::ListenerHandle) {
        let holder = net_io::ServerBuilder::new()
            .listener(net_io::ListenerSpec::tcp("127.0.0.1:0"))
            .handler(|_c: &mut net_io::TcpConnection| Ok(()))
            .gate(net_io::AllowAll)
            .build()
            .unwrap();
        let addr = holder.listeners()[0].local.socket_addr().unwrap();
        let first = holder.release_listeners().unwrap().pop().unwrap();
        let second = holder.release_listeners().unwrap().pop().unwrap();
        let _ = holder.shutdown(std::time::Instant::now());
        (addr, first, second)
    }

    /// A server on one handle of the shared socket. Every client here comes
    /// from 127.0.0.1, and in a hand-off the incoming server takes the old
    /// one's clients while it starts, so its per-source cap needs room above
    /// the load: at the default 16 it refuses at its own cap on a slow host,
    /// which looks like a hand-off reset (D-100).
    fn serve(handle: net_io::ListenerHandle, who: &'static str) -> serve_net::Running {
        let room = net_io::Limits::builder()
            .max_per_source(64, net_io::PrefixPolicy::DEFAULT)
            .build()
            .unwrap();
        Server::builder()
            .listener(ListenerSpec::handed_off("web", handle).transport_limits(room))
            .serve(serve_net::handler(move |_req, reply| {
                reply.text(200, who);
            }))
            .unwrap()
    }

    #[test]
    fn test_restart_under_load_refuses_and_resets_nothing() {
        let (addr, first, second) = shared_socket();
        let old = serve(first, "old");

        let stop = Arc::new(AtomicBool::new(false));
        let served = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(Mutex::new(Vec::new()));
        let clients: Vec<_> = (0..4)
            .map(|_| {
                let (stop, served, failed) =
                    (Arc::clone(&stop), Arc::clone(&served), Arc::clone(&failed));
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match one_request(addr) {
                            Ok(()) => served.fetch_add(1, Ordering::Relaxed),
                            Err(e) => {
                                failed.lock().unwrap().push(e);
                                0
                            }
                        };
                    }
                })
            })
            .collect();

        std::thread::sleep(Duration::from_millis(100));
        // The new process starts on the same socket, then the old one leaves.
        let new = serve(second, "new");
        std::thread::sleep(Duration::from_millis(50));
        old.close_door();
        let report = old.drain(Duration::from_secs(5));
        assert_eq!(report.killed, 0, "{report:?}");
        std::thread::sleep(Duration::from_millis(100));

        stop.store(true, Ordering::Relaxed);
        for c in clients {
            c.join().unwrap();
        }
        assert!(served.load(Ordering::Relaxed) > 0);
        let failed = failed.lock().unwrap();
        assert!(failed.is_empty(), "failed in the restart: {failed:?}");
        let _ = new.drain(Duration::from_secs(2));
    }

    /// One request: when it began, and how it ended.
    type Attempt = (std::time::Instant, Result<(), String>);

    /// Where idea I-9 kills the old server in a hand-off.
    #[derive(Clone, Copy, Debug)]
    enum KillAt {
        /// Before the new server exists: only the supervisor's handle holds
        /// the socket, and connections wait in its backlog.
        BeforeNew,
        /// Right after the new server started.
        AfterNew,
        /// After the old server closed its door.
        AfterDoor,
        /// While the old server drains.
        MidDrain,
    }

    /// Idea I-9: the old server dies at a step of the hand-off instead of
    /// draining. The guarantee that holds then: no connection is refused
    /// (the socket stays open through the new server's handle), only requests
    /// already begun on the old server can fail, and every request begun after
    /// the kill is served.
    fn kill_during_handoff(at: KillAt) {
        let (addr, first, second) = shared_socket();
        let old = serve(first, "old");
        let stop = Arc::new(AtomicBool::new(false));
        let log: Arc<Mutex<Vec<Attempt>>> = Arc::new(Mutex::new(Vec::new()));
        let clients: Vec<_> = (0..4)
            .map(|_| {
                let (stop, log) = (Arc::clone(&stop), Arc::clone(&log));
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let begun = std::time::Instant::now();
                        let r = one_request(addr);
                        log.lock().unwrap().push((begun, r));
                    }
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(100));
        let (killed_at, new) = match at {
            KillAt::BeforeNew => {
                drop(old);
                let killed = std::time::Instant::now();
                std::thread::sleep(Duration::from_millis(50));
                (killed, serve(second, "new"))
            }
            KillAt::AfterNew => {
                let new = serve(second, "new");
                drop(old);
                (std::time::Instant::now(), new)
            }
            KillAt::AfterDoor => {
                let new = serve(second, "new");
                std::thread::sleep(Duration::from_millis(50));
                old.close_door();
                drop(old);
                (std::time::Instant::now(), new)
            }
            KillAt::MidDrain => {
                let new = serve(second, "new");
                std::thread::sleep(Duration::from_millis(50));
                old.close_door();
                old.drain_handle().drain(Duration::from_secs(5));
                std::thread::sleep(Duration::from_millis(20));
                drop(old);
                (std::time::Instant::now(), new)
            }
        };
        // Run until a request begun after the kill is served, not for a fixed
        // time: under heavy load the clients' first requests after the kill
        // can take longer than any short window.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        std::thread::sleep(Duration::from_millis(300));
        while std::time::Instant::now() < deadline
            && !log
                .lock()
                .unwrap()
                .iter()
                .any(|(begun, r)| *begun > killed_at && r.is_ok())
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        stop.store(true, Ordering::Relaxed);
        for c in clients {
            c.join().unwrap();
        }
        let log = log.lock().unwrap();
        let refused: Vec<_> = log
            .iter()
            .filter_map(|(_, r)| r.as_ref().err())
            // Refused: nobody held the socket. A reset of a connection the
            // old server had taken when it died is an in-flight loss, which
            // the check below allows only before the kill.
            .filter(|e| e.starts_with("connect") && e.contains("refused"))
            .collect();
        assert!(
            refused.is_empty(),
            "{at:?}: connections refused: {refused:?}"
        );
        let late: Vec<_> = log
            .iter()
            .filter(|(begun, r)| *begun > killed_at && r.is_err())
            .collect();
        assert!(late.is_empty(), "{at:?}: failed after the kill: {late:?}");
        let served_after = log
            .iter()
            .filter(|(begun, r)| *begun > killed_at && r.is_ok())
            .count();
        assert!(served_after > 0, "{at:?}: nothing served after the kill");
        let _ = new.drain(Duration::from_secs(2));
    }

    #[test]
    #[cfg_attr(
        all(windows, serve_net_blocking_tests),
        ignore = "net-io: on the Windows blocking pool a request begun after the old server's drop can hang (dev/ROADMAP.md)"
    )]
    fn test_i9_kill_before_the_new_server_starts() {
        kill_during_handoff(KillAt::BeforeNew);
    }

    #[test]
    fn test_i9_kill_right_after_the_new_server_starts() {
        kill_during_handoff(KillAt::AfterNew);
    }

    #[test]
    fn test_i9_kill_after_the_door_closed() {
        kill_during_handoff(KillAt::AfterDoor);
    }

    #[test]
    fn test_i9_kill_mid_drain() {
        kill_during_handoff(KillAt::MidDrain);
    }
}
