//! A connection that never sends a byte (a browser's spare preconnected
//! socket) closes on the head deadline, not on the inbound rate: a rate close
//! reads as a slow peer, and a connection gate counts slow peers against the
//! address. One byte and then nothing is still a slow peer. Before 0.8.4 the
//! rate ran from accept, and a silent socket closed as a slow peer.

#![cfg(feature = "http1")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serve_net::net_io::{Direction, Event};
use serve_net::{CloseReason, EventSink, Ingress, Limits, ListenerSpec, Server, Workers};
use tls_net::{ClientBuilder, ClientSession, KeySource, TlsServer, TrustAnchors};

const SITE_PEM: &[u8] = include_bytes!("data/site.pem");
const SITE_KEY: &[u8] = include_bytes!("data/site.key");
const CA_PEM: &[u8] = include_bytes!("data/ca.pem");

/// Past net-io's 5 s rate grace, with room for a loaded machine to close a
/// slow peer first.
const HEAD: Duration = Duration::from_secs(9);
/// How long a test waits for the close.
const PATIENCE: Duration = Duration::from_secs(40);

/// The close reasons net-io reports: what a connection gate sees.
#[derive(Clone, Default)]
struct Closes(Arc<Mutex<Vec<CloseReason>>>);

impl EventSink for Closes {
    fn event(&self, event: &Event) {
        if let Event::Closed { reason, .. } = event {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(*reason);
        }
    }
}

impl Closes {
    fn wait(&self) -> CloseReason {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(r) = self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .first()
            {
                return *r;
            }
            assert!(Instant::now() < deadline, "the connection never closed");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn serve(workers: Workers, tls: bool) -> (serve_net::Running, SocketAddr, Closes) {
    let closes = Closes::default();
    let listener = ListenerSpec::tcp("web", "127.0.0.1:0");
    let builder = Server::builder()
        .workers(workers)
        .limits(Limits::new().head_timeout(HEAD))
        .events(closes.clone());
    let handler = serve_net::handler(|_r, reply| {
        reply.text(200, "ok");
    });
    let server = if tls {
        let acceptor = TlsServer::builder()
            .certificate(SITE_PEM, KeySource::Pem(SITE_KEY))
            .unwrap()
            .build()
            .unwrap();
        builder
            .listener(listener.ingress(Ingress::DirectTls))
            .tls(acceptor)
            .serve(handler)
            .unwrap()
    } else {
        builder.listener(listener).serve(handler).unwrap()
    };
    let addr = server.local_addr().unwrap();
    (server, addr, closes)
}

/// A TLS client that finishes the handshake and then holds the socket.
fn handshaken(addr: SocketAddr) -> (TcpStream, ClientSession) {
    let mut session = ClientBuilder::new()
        .trust(TrustAnchors::from_pem(CA_PEM).unwrap())
        .build()
        .unwrap()
        .connect("example.test")
        .unwrap();
    let mut sock = TcpStream::connect(addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut inbuf = vec![0u8; 64 * 1024];
    let mut filled = 0;
    let mut out = vec![0u8; 64 * 1024];
    while session.info().is_none() {
        let p = session.step(&mut inbuf[..filled], &mut out).unwrap();
        let (consumed, written) = (p.consumed, p.written);
        let need = matches!(p.event, tls_net::Event::NeedInput);
        if written > 0 {
            sock.write_all(&out[..written]).unwrap();
        }
        inbuf.copy_within(consumed..filled, 0);
        filled -= consumed;
        if need && written == 0 && consumed == 0 {
            let n = sock.read(&mut inbuf[filled..]).unwrap();
            assert!(n > 0, "the server closed during the handshake");
            filled += n;
        }
    }
    (sock, session)
}

/// Opens a connection that sends `bytes` of a head (none for a preconnect)
/// and then nothing; returns net-io's close reason.
fn close_after(workers: Workers, tls: bool, bytes: &[u8]) -> CloseReason {
    let (server, addr, closes) = serve(workers, tls);
    let held = if tls {
        let (mut sock, mut session) = handshaken(addr);
        if !bytes.is_empty() {
            let mut out = vec![0u8; 1024];
            let n = session.encrypt(bytes, &mut out).unwrap();
            sock.write_all(&out[..n]).unwrap();
        }
        sock
    } else {
        let mut sock = TcpStream::connect(addr).unwrap();
        sock.write_all(bytes).unwrap();
        sock
    };
    let reason = closes.wait();
    drop(held);
    let _ = server.drain(Duration::from_secs(1));
    reason
}

fn assert_quiet_close(reason: CloseReason) {
    assert!(
        matches!(reason, CloseReason::ExchangeDeadline | CloseReason::Idle),
        "a silent connection closed as {reason:?}"
    );
}

fn assert_slow_peer(reason: CloseReason) {
    assert_eq!(
        reason,
        CloseReason::SlowPeer {
            direction: Direction::In
        }
    );
}

#[test]
fn test_silent_connection_closes_on_the_deadline_on_the_reactor() {
    assert_quiet_close(close_after(Workers::auto(), false, b""));
}

#[test]
fn test_silent_connection_closes_on_the_deadline_on_the_blocking_driver() {
    assert_quiet_close(close_after(Workers::blocking(2), false, b""));
}

#[test]
fn test_one_byte_then_nothing_is_a_slow_peer_on_the_reactor() {
    assert_slow_peer(close_after(Workers::auto(), false, b"G"));
}

#[test]
fn test_one_byte_then_nothing_is_a_slow_peer_on_the_blocking_driver() {
    assert_slow_peer(close_after(Workers::blocking(2), false, b"G"));
}

#[test]
fn test_silent_tls_connection_closes_on_the_deadline_on_the_reactor() {
    assert_quiet_close(close_after(Workers::auto(), true, b""));
}

#[test]
fn test_silent_tls_connection_closes_on_the_deadline_on_the_blocking_driver() {
    assert_quiet_close(close_after(Workers::blocking(2), true, b""));
}

#[test]
fn test_one_byte_over_tls_then_nothing_is_a_slow_peer_on_the_reactor() {
    assert_slow_peer(close_after(Workers::auto(), true, b"G"));
}

#[test]
fn test_one_byte_over_tls_then_nothing_is_a_slow_peer_on_the_blocking_driver() {
    assert_slow_peer(close_after(Workers::blocking(2), true, b"G"));
}
