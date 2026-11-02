//! The TLS seam over real sockets, with a toy engine in place of tls-net:
//! a `HELLO`/`WELCOME` handshake, then length-prefixed records.

#![cfg(feature = "http1")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serve_net::net_io::{
    ConnectionInfo, TlsAcceptor, TlsCloseKind, TlsError, TlsEvent, TlsFailure, TlsInfo,
    TlsProgress, TlsSession,
};
use serve_net::{Ingress, ListenerSpec, Scheme, Server};

struct Toy;

impl TlsAcceptor for Toy {
    type Session = ToySession;
    fn session(&self, _info: &ConnectionInfo) -> Result<ToySession, TlsError> {
        Ok(ToySession::default())
    }
}

#[derive(Default)]
struct ToySession {
    /// 0: waiting for the hello; 1: welcome sent; 2: handshake complete.
    state: u8,
    closed: bool,
}

impl TlsSession for ToySession {
    fn step<'i>(
        &mut self,
        input: &'i mut [u8],
        output: &mut [u8],
    ) -> Result<TlsProgress<'i>, TlsError> {
        let need = TlsProgress {
            consumed: 0,
            written: 0,
            event: TlsEvent::NeedInput,
        };
        match self.state {
            0 => {
                if input.len() < 5 {
                    return Ok(need);
                }
                output[..7].copy_from_slice(b"WELCOME");
                self.state = 1;
                Ok(TlsProgress {
                    consumed: 5,
                    written: 7,
                    event: TlsEvent::Transmit,
                })
            }
            1 => {
                self.state = 2;
                Ok(TlsProgress {
                    consumed: 0,
                    written: 0,
                    event: TlsEvent::HandshakeComplete,
                })
            }
            _ => {
                if input.len() < 2 {
                    return Ok(need);
                }
                let len = usize::from(u16::from_be_bytes([input[0], input[1]]));
                if len == 0 {
                    self.closed = true;
                    return Ok(TlsProgress {
                        consumed: 2,
                        written: 0,
                        event: TlsEvent::PeerClosed,
                    });
                }
                if input.len() < 2 + len {
                    return Ok(need);
                }
                Ok(TlsProgress {
                    consumed: 2 + len,
                    written: 0,
                    event: TlsEvent::Data(&input[2..2 + len]),
                })
            }
        }
    }

    fn encrypt(&mut self, plaintext: &[u8], output: &mut [u8]) -> Result<usize, TlsError> {
        let n = plaintext.len();
        if output.len() < n + 2 {
            return Err(TlsError::new(TlsFailure::OutputFull, 3, None));
        }
        output[..2].copy_from_slice(&u16::try_from(n).unwrap_or(u16::MAX).to_be_bytes());
        output[2..2 + n].copy_from_slice(plaintext);
        Ok(n + 2)
    }

    fn close(&mut self, output: &mut [u8]) -> Result<usize, TlsError> {
        output[..2].copy_from_slice(&[0, 0]);
        Ok(2)
    }

    fn on_transport_closed(&mut self) -> TlsCloseKind {
        if self.closed {
            TlsCloseKind::Clean
        } else {
            TlsCloseKind::Unclean
        }
    }

    fn info(&self) -> Option<TlsInfo> {
        (self.state == 2)
            .then(|| TlsInfo::new(Some("example.test"), None, 0x0304, 0x1301))
            .flatten()
    }

    fn input_capacity_required(&self) -> usize {
        2
    }
}

/// Runs the handshake and one request; returns the reply's text.
fn ask(addr: SocketAddr, request: &[u8]) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"HELLO").unwrap();
    let mut welcome = [0u8; 7];
    s.read_exact(&mut welcome).unwrap();
    assert_eq!(&welcome, b"WELCOME");
    let mut record = u16::try_from(request.len()).unwrap().to_be_bytes().to_vec();
    record.extend_from_slice(request);
    s.write_all(&record).unwrap();
    let mut got = Vec::new();
    loop {
        let mut h = [0u8; 2];
        s.read_exact(&mut h).unwrap();
        let len = usize::from(u16::from_be_bytes(h));
        if len == 0 {
            break;
        }
        let mut body = vec![0u8; len];
        s.read_exact(&mut body).unwrap();
        got.extend_from_slice(&body);
        let text = String::from_utf8_lossy(&got);
        if text.ends_with("https") {
            break;
        }
    }
    String::from_utf8_lossy(&got).into_owned()
}

#[test]
fn test_tls_listener_serves_https_through_the_seam() {
    let server = Server::builder()
        .listener(ListenerSpec::tcp("secure", "127.0.0.1:0").ingress(Ingress::DirectTls))
        .tls(Toy)
        .serve(serve_net::handler(|req, reply| {
            let c = req.client();
            assert_eq!(c.scheme(), Scheme::Https);
            let scheme = if c.scheme() == Scheme::Https {
                "https"
            } else {
                "http"
            };
            reply.text(200, &format!("{:?} {scheme}", c.ingress()));
        }))
        .unwrap();
    assert_eq!(
        server.listeners()[0].ingress,
        serve_net::IngressKind::DirectTls
    );
    let addr = server.local_addr().unwrap();
    let a = ask(addr, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(a.starts_with("HTTP/1.1 200"), "{a}");
    assert!(a.ends_with("DirectTls https"), "{a}");
    let report = server.drain(Duration::from_secs(2));
    assert_eq!(report.killed, 0, "{report:?}");
}

#[test]
fn test_tls_listener_without_acceptor_is_refused() {
    let err = Server::builder()
        .listener(ListenerSpec::tcp("secure", "127.0.0.1:0").ingress(Ingress::DirectTls))
        .build()
        .unwrap_err();
    assert!(err.has(serve_net::RuleId::TlsWithoutAcceptor), "{err}");
}
