//! T-30: a restart through a shared listening socket refuses and resets
//! nothing. T-20 and the head-read signal sit here too: both need a real
//! server's view of a request.

#![cfg(feature = "http1")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::time::Duration;

use serve_net::testing::TestConnection;
use serve_net::{
    Envelope, Finished, HttpReply, HttpRequest, HttpService, ListenerSpec, Outcome, Server,
};

const GET: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";

/// One request on a new connection; `true` when a whole 200 came back.
fn one_request(addr: SocketAddr) -> bool {
    let Ok(mut s) = TcpStream::connect(addr) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    if s.write_all(GET).is_err() {
        return false;
    }
    let mut got = Vec::new();
    match s.read_to_end(&mut got) {
        Ok(_) => got.starts_with(b"HTTP/1.1 200"),
        Err(_) => false,
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

fn serve(handle: net_io::ListenerHandle, who: &'static str) -> serve_net::Running {
    Server::builder()
        .listener(ListenerSpec::handed_off("web", handle))
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
    let failed = Arc::new(AtomicU64::new(0));
    let clients: Vec<_> = (0..4)
        .map(|_| {
            let (stop, served, failed) =
                (Arc::clone(&stop), Arc::clone(&served), Arc::clone(&failed));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if one_request(addr) {
                        served.fetch_add(1, Ordering::Relaxed);
                    } else {
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
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
    assert_eq!(
        failed.load(Ordering::Relaxed),
        0,
        "requests failed in the restart"
    );
    let _ = new.drain(Duration::from_secs(2));
}

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
