//! HTTP/2 end to end: TLS with ALPN `h2` under `EngineChoice::Http1OrH2ByAlpn`,
//! each stream an `HttpRequest` through the same `head`, admission, and
//! service as HTTP/1.1. The client writes frames with h2-net's own codec.

#![cfg(all(feature = "http1", feature = "h2"))]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use h2_net::ErrorCode;
use h2_net::frame::{self, Decoded, Frame, StreamId};
use h2_net::hpack::{Decoder, Encoder, HeaderField, HeaderList};
use serve_net::{
    ConnectionId, EndReason, EngineChoice, Envelope, HeadAction, HttpHead, HttpReply, HttpRequest,
    HttpService, Ingress, ListenerSpec, Observer, Outcome, Profile, ProfileId, ReplyToken, Scheme,
    Server,
};
use tls_net::{Alpn, ClientBuilder, ClientSession, Event, KeySource, TlsServer, TrustAnchors};

const SITE_PEM: &[u8] = include_bytes!("data/site.pem");
const SITE_KEY: &[u8] = include_bytes!("data/site.key");
const CA_PEM: &[u8] = include_bytes!("data/ca.pem");

/// Counts how connections ended.
#[derive(Clone, Default)]
struct Ends(Arc<Mutex<Vec<EndReason>>>);

impl Observer for Ends {
    fn connection_ended(&self, _c: ConnectionId, end: EndReason, _requests: u32) {
        self.0.lock().unwrap().push(end);
    }
}

impl Ends {
    fn wait_for(&self, end: EndReason) -> bool {
        for _ in 0..300 {
            if self.0.lock().unwrap().contains(&end) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
}

#[derive(Clone, Default)]
struct Site {
    tokens: Arc<Mutex<Vec<ReplyToken>>>,
    done: Arc<AtomicU32>,
}

impl HttpService for Site {
    fn head(&self, head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
        if head.path() == "/refuse" {
            return HeadAction::Refuse {
                status: 429,
                retry_after: Some(Duration::from_secs(2)),
            };
        }
        HeadAction::Proceed
    }

    fn call(&self, req: &HttpRequest<'_>, env: &Envelope, reply: &mut HttpReply) -> Outcome {
        match req.path() {
            "/echo" => {
                reply.text(200, &format!("{} bytes", req.body_bytes().len()));
                Outcome::Done
            }
            "/defer" => {
                let mut token = reply.defer();
                token.reply().text(200, "late");
                self.tokens.lock().unwrap().push(token);
                Outcome::Deferred
            }
            path => {
                let https = req.client().scheme() == Scheme::Https;
                reply.text(
                    200,
                    &format!("https={https} seq={} {path}", env.seq().get()),
                );
                Outcome::Done
            }
        }
    }

    fn done(&self, _env: &Envelope, _finished: &serve_net::Finished) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }
}

fn serve(site: Site, ends: Ends) -> serve_net::Running {
    let acceptor = TlsServer::builder()
        .certificate(SITE_PEM, KeySource::Pem(SITE_KEY))
        .unwrap()
        .alpn(&[Alpn::H2, Alpn::HTTP_1_1])
        .build()
        .unwrap();
    Server::builder()
        .listener(ListenerSpec::tcp("secure", "127.0.0.1:0").ingress(Ingress::DirectTls))
        .profile(Profile::new(ProfileId(0)).engine(EngineChoice::Http1OrH2ByAlpn))
        .tls(acceptor)
        .observer(ends)
        .serve(site)
        .unwrap()
}

/// One stream's answer, as the client saw it.
#[derive(Debug, Default)]
struct Answer {
    status: Option<u16>,
    fields: Vec<(String, String)>,
    body: Vec<u8>,
    ended: bool,
    reset: Option<ErrorCode>,
}

impl Answer {
    fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A blocking HTTP/2 client over TLS: enough for the tests.
struct H2 {
    sock: TcpStream,
    session: ClientSession,
    inbuf: Vec<u8>,
    filled: usize,
    out: Vec<u8>,
    enc: Encoder,
    dec: Decoder,
    /// Decrypted bytes not yet read as frames.
    frames: Vec<u8>,
    block: Vec<u8>,
    block_stream: u32,
    block_end: bool,
    answers: HashMap<u32, Answer>,
    goaway: Option<ErrorCode>,
    closed: bool,
}

impl H2 {
    fn connect(addr: SocketAddr) -> H2 {
        let tls = ClientBuilder::new()
            .trust(TrustAnchors::from_pem(CA_PEM).unwrap())
            .alpn(&[Alpn::H2])
            .build()
            .unwrap();
        let sock = TcpStream::connect(addr).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut c = H2 {
            sock,
            session: tls.connect("example.test").unwrap(),
            inbuf: vec![0; 64 * 1024],
            filled: 0,
            out: vec![0; 256 * 1024],
            enc: Encoder::new(4096),
            dec: Decoder::new(4096),
            frames: Vec::new(),
            block: Vec::new(),
            block_stream: 0,
            block_end: false,
            answers: HashMap::new(),
            goaway: None,
            closed: false,
        };
        while c.session.info().is_none() {
            assert!(c.turn(), "the handshake failed");
        }
        let alpn = c.session.info().and_then(|i| i.alpn.map(<[u8]>::to_vec));
        assert_eq!(alpn.as_deref(), Some(&b"h2"[..]), "ALPN did not pick h2");
        let mut hello = frame::PREFACE.to_vec();
        frame::settings_frame(&[], 16_384, &mut hello).unwrap();
        c.send(&hello);
        c
    }

    fn send(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(16 * 1024) {
            let n = self.session.encrypt(chunk, &mut self.out).unwrap();
            if self.sock.write_all(&self.out[..n]).is_err() {
                self.closed = true;
                return;
            }
        }
    }

    fn frame(&mut self, f: &Frame<'_>) {
        let mut wire = Vec::new();
        frame::encode(f, 1 << 24, &mut wire).unwrap();
        self.send(&wire);
    }

    fn request(
        &mut self,
        stream: u32,
        method: &str,
        path: &str,
        extra: &[(&str, &str)],
        end: bool,
    ) {
        let mut wire = Vec::new();
        self.request_into(&mut wire, stream, method, path, extra, end);
        self.send(&wire);
    }

    /// Appends a request's HEADERS frame to `wire` without sending it.
    fn request_into(
        &mut self,
        wire: &mut Vec<u8>,
        stream: u32,
        method: &str,
        path: &str,
        extra: &[(&str, &str)],
        end: bool,
    ) {
        let mut fields = vec![
            HeaderField::new(b":method", method.as_bytes()),
            HeaderField::new(b":scheme", b"https"),
            HeaderField::new(b":authority", b"example.test"),
            HeaderField::new(b":path", path.as_bytes()),
        ];
        for (n, v) in extra {
            fields.push(HeaderField::new(n.as_bytes(), v.as_bytes()));
        }
        let mut block = Vec::new();
        self.enc.encode(fields, &mut block);
        frame::encode(
            &Frame::Headers {
                stream: StreamId::new(stream).unwrap(),
                end_stream: end,
                end_headers: true,
                priority: None,
                fragment: &block,
                padding: None,
            },
            1 << 24,
            wire,
        )
        .unwrap();
    }

    /// One step of the TLS session; false once the connection is gone.
    fn turn(&mut self) -> bool {
        let Ok(p) = self
            .session
            .step(&mut self.inbuf[..self.filled], &mut self.out)
        else {
            return false;
        };
        let (consumed, written) = (p.consumed, p.written);
        let mut need = false;
        let mut alive = true;
        match p.event {
            Event::Data(d) => self.frames.extend_from_slice(d),
            Event::NeedInput => need = true,
            Event::PeerClosed | Event::Closed => alive = false,
            _ => {}
        }
        if written > 0 && self.sock.write_all(&self.out[..written]).is_err() {
            return false;
        }
        self.inbuf.copy_within(consumed..self.filled, 0);
        self.filled -= consumed;
        if need && written == 0 && consumed == 0 {
            if self.filled == self.inbuf.len() {
                let want = self.session.input_capacity_required().max(self.filled + 1);
                self.inbuf.resize(want, 0);
            }
            match self.sock.read(&mut self.inbuf[self.filled..]) {
                Ok(0) | Err(_) => return false,
                Ok(n) => self.filled += n,
            }
        }
        alive
    }

    /// Reads the frames that arrived.
    fn read_frames(&mut self) {
        let mut acks = Vec::new();
        while let Ok(Decoded::Frame { frame, len }) = frame::decode(&self.frames, 1 << 24) {
            match frame {
                Frame::Settings(s) if !s.is_ack() => {
                    frame::encode(&Frame::Settings(frame::Settings::ack()), 16_384, &mut acks)
                        .unwrap();
                }
                Frame::Ping {
                    ack: false,
                    payload,
                } => {
                    frame::encode(&Frame::Ping { ack: true, payload }, 16_384, &mut acks).unwrap();
                }
                Frame::Headers {
                    stream,
                    end_stream,
                    end_headers,
                    fragment,
                    ..
                } => {
                    self.block.clear();
                    self.block.extend_from_slice(fragment);
                    self.block_stream = stream.value();
                    self.block_end = end_stream;
                    if end_headers {
                        self.headers_done();
                    }
                }
                Frame::Continuation {
                    end_headers,
                    fragment,
                    ..
                } => {
                    self.block.extend_from_slice(fragment);
                    if end_headers {
                        self.headers_done();
                    }
                }
                Frame::Data {
                    stream,
                    end_stream,
                    data,
                    ..
                } => {
                    let a = self.answers.entry(stream.value()).or_default();
                    a.body.extend_from_slice(data);
                    a.ended |= end_stream;
                }
                Frame::RstStream { stream, code } => {
                    self.answers.entry(stream.value()).or_default().reset = Some(code);
                }
                Frame::GoAway { code, .. } => self.goaway = Some(code),
                _ => {}
            }
            self.frames.drain(..len);
        }
        if !acks.is_empty() {
            self.send(&acks);
        }
    }

    fn headers_done(&mut self) {
        let mut list = HeaderList::new(1 << 20);
        self.dec.decode(&self.block, &mut list).unwrap();
        let a = self.answers.entry(self.block_stream).or_default();
        for f in list.iter() {
            let (n, v) = (
                String::from_utf8_lossy(f.name).into_owned(),
                String::from_utf8_lossy(f.value).into_owned(),
            );
            if n == ":status" {
                a.status = v.parse().ok();
            } else {
                a.fields.push((n, v));
            }
        }
        a.ended |= self.block_end;
    }

    /// Reads until `done` holds or the connection ends.
    fn until(&mut self, done: impl Fn(&H2) -> bool) -> bool {
        for _ in 0..10_000 {
            self.read_frames();
            if done(self) {
                return true;
            }
            if self.closed || !self.turn() {
                self.closed = true;
                self.read_frames();
                return done(self);
            }
        }
        false
    }

    fn finished(&self, stream: u32) -> bool {
        self.answers
            .get(&stream)
            .is_some_and(|a| a.ended || a.reset.is_some())
    }
}

#[test]
fn test_h2_get_is_served_over_alpn() {
    let ends = Ends::default();
    let server = serve(Site::default(), ends.clone());
    let mut c = H2::connect(server.local_addr().unwrap());
    c.request(1, "GET", "/hello?x=1", &[], true);
    assert!(c.until(|c| c.finished(1)), "no answer");
    let a = &c.answers[&1];
    assert_eq!(a.status, Some(200));
    assert_eq!(String::from_utf8_lossy(&a.body), "https=true seq=1 /hello");
    assert_eq!(a.field("content-length"), Some("23"));
    assert!(a.field("date").is_some(), "no date");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_h2_many_streams_on_one_connection() {
    let site = Site::default();
    let server = serve(site.clone(), Ends::default());
    let mut c = H2::connect(server.local_addr().unwrap());
    let ids: Vec<u32> = (0..20).map(|i| 2 * i + 1).collect();
    for id in &ids {
        c.request(*id, "GET", &format!("/s{id}"), &[], true);
    }
    assert!(c.until(|c| ids.iter().all(|id| c.finished(*id))));
    for id in &ids {
        let a = &c.answers[id];
        assert_eq!(a.status, Some(200), "{id}");
        assert_eq!(
            String::from_utf8_lossy(&a.body),
            format!("https=true seq={id} /s{id}"),
            "each stream's envelope carries its stream id"
        );
    }
    assert!(site.done.load(Ordering::Relaxed) >= 20);
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_h2_request_body_reaches_the_service() {
    let server = serve(Site::default(), Ends::default());
    let mut c = H2::connect(server.local_addr().unwrap());
    let body = vec![b'x'; 40_000];
    c.request(1, "POST", "/echo", &[("content-length", "40000")], false);
    for (i, chunk) in body.chunks(16_000).enumerate() {
        let last = (i + 1) * 16_000 >= body.len();
        c.frame(&Frame::Data {
            stream: StreamId::new(1).unwrap(),
            end_stream: last,
            data: chunk,
            padding: None,
        });
    }
    assert!(c.until(|c| c.finished(1)));
    assert_eq!(String::from_utf8_lossy(&c.answers[&1].body), "40000 bytes");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_h2_refusal_in_head_answers_and_the_connection_goes_on() {
    let server = serve(Site::default(), Ends::default());
    let mut c = H2::connect(server.local_addr().unwrap());
    c.request(1, "GET", "/refuse", &[], true);
    c.request(3, "GET", "/after", &[], true);
    assert!(c.until(|c| c.finished(1) && c.finished(3)));
    assert_eq!(c.answers[&1].status, Some(429));
    assert_eq!(c.answers[&1].field("retry-after"), Some("2"));
    assert_eq!(c.answers[&3].status, Some(200));
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_h2_deferred_reply_arrives_while_other_streams_are_served() {
    let site = Site::default();
    let server = serve(site.clone(), Ends::default());
    let mut c = H2::connect(server.local_addr().unwrap());
    c.request(1, "GET", "/defer", &[], true);
    c.request(3, "GET", "/now", &[], true);
    assert!(
        c.until(|c| c.finished(3)),
        "the second stream waited for the first"
    );
    assert!(!c.finished(1));
    let tokens: Vec<ReplyToken> = site.tokens.lock().unwrap().drain(..).collect();
    assert_eq!(tokens.len(), 1);
    for t in tokens {
        t.complete().unwrap();
    }
    assert!(c.until(|c| c.finished(1)));
    assert_eq!(String::from_utf8_lossy(&c.answers[&1].body), "late");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_h2_rapid_reset_ends_the_connection_as_abuse() {
    let ends = Ends::default();
    let server = serve(Site::default(), ends.clone());
    let mut c = H2::connect(server.local_addr().unwrap());
    // Each stream's reply is pending when the reset arrives: work the reset
    // abandons, which is what makes the attack cost the server. (A reset of
    // a stream already answered abandons nothing and is not counted as one.)
    // The frames go out in full TLS records, as a real client sends them.
    let mut wire = Vec::new();
    for i in 0..1000u32 {
        let id = 2 * i + 1;
        c.request_into(&mut wire, id, "GET", "/defer", &[], true);
        frame::encode(
            &Frame::RstStream {
                stream: StreamId::new(id).unwrap(),
                code: ErrorCode::CANCEL,
            },
            1 << 24,
            &mut wire,
        )
        .unwrap();
    }
    c.send(&wire);
    let _ = c.until(|c| c.goaway.is_some());
    assert_eq!(c.goaway, Some(ErrorCode::ENHANCE_YOUR_CALM));
    assert!(
        ends.wait_for(EndReason::Abuse),
        "{:?}",
        ends.0.lock().unwrap()
    );
    let _ = server.drain(Duration::from_secs(2));
}

// ---- Cleartext HTTP/2 with prior knowledge, behind a trusted proxy.

fn serve_cleartext() -> serve_net::Running {
    use serve_net::{ProxySource, TrustedProxies};
    Server::builder()
        .listener(
            ListenerSpec::tcp("edge", "127.0.0.1:0").ingress(Ingress::Proxy(
                ProxySource::forwarded_for(TrustedProxies::loopback()),
            )),
        )
        .profile(Profile::new(ProfileId(0)).engine(EngineChoice::Http1OrH2ByPreface))
        .serve(serve_net::handler(|req, reply| {
            let client = req
                .client()
                .addr()
                .map(|a| a.to_string())
                .unwrap_or_default();
            reply.text(200, &format!("{} from {client}", req.path()));
        }))
        .unwrap()
}

/// Sends `wire` over plain TCP and reads frames until stream 1 ends or the
/// connection closes; returns stream 1's status and body.
fn cleartext_exchange(addr: SocketAddr, wire: &[u8]) -> (Option<u16>, Vec<u8>) {
    let mut sock = TcpStream::connect(addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    sock.write_all(wire).unwrap();
    let mut buf = Vec::new();
    let mut dec = Decoder::new(4096);
    let (mut status, mut body, mut ended) = (None, Vec::new(), false);
    let mut chunk = [0u8; 16 * 1024];
    while !ended {
        let n = match sock.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        while let Ok(Decoded::Frame { frame, len }) = frame::decode(&buf, 1 << 24) {
            match frame {
                Frame::Settings(s) if !s.is_ack() => {
                    let mut ack = Vec::new();
                    frame::encode(&Frame::Settings(frame::Settings::ack()), 16_384, &mut ack)
                        .unwrap();
                    let _ = sock.write_all(&ack);
                }
                Frame::Headers {
                    stream,
                    fragment,
                    end_stream,
                    ..
                } if stream.value() == 1 => {
                    let mut list = HeaderList::new(1 << 20);
                    dec.decode(fragment, &mut list).unwrap();
                    status = list
                        .get(":status")
                        .and_then(|v| std::str::from_utf8(v).ok()?.parse().ok());
                    ended |= end_stream;
                }
                Frame::Data {
                    stream,
                    data,
                    end_stream,
                    ..
                } if stream.value() == 1 => {
                    body.extend_from_slice(data);
                    ended |= end_stream;
                }
                _ => {}
            }
            buf.drain(..len);
        }
    }
    (status, body)
}

#[test]
fn test_h2c_prior_knowledge_behind_a_trusted_proxy() {
    let server = serve_cleartext();
    let mut wire = frame::PREFACE.to_vec();
    frame::settings_frame(&[], 16_384, &mut wire).unwrap();
    let mut block = Vec::new();
    Encoder::new(4096).encode(
        [
            HeaderField::new(b":method", b"GET"),
            HeaderField::new(b":scheme", b"http"),
            HeaderField::new(b":authority", b"example.test"),
            HeaderField::new(b":path", b"/clear"),
            HeaderField::new(b"x-forwarded-for", b"203.0.113.9"),
        ],
        &mut block,
    );
    frame::encode(
        &Frame::Headers {
            stream: StreamId::new(1).unwrap(),
            end_stream: true,
            end_headers: true,
            priority: None,
            fragment: &block,
            padding: None,
        },
        16_384,
        &mut wire,
    )
    .unwrap();
    let (status, body) = cleartext_exchange(server.local_addr().unwrap(), &wire);
    assert_eq!(status, Some(200));
    // The forwarded client, as for HTTP/1.1 behind the same proxy.
    assert_eq!(String::from_utf8_lossy(&body), "/clear from 203.0.113.9");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_h2c_listener_still_serves_http1() {
    let server = serve_cleartext();
    let mut sock = TcpStream::connect(server.local_addr().unwrap()).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    sock.write_all(
        b"GET /old HTTP/1.1\r\nHost: example.test\r\nX-Forwarded-For: 203.0.113.9\r\nConnection: close\r\n\r\n",
    )
    .unwrap();
    let mut got = String::new();
    let _ = sock.read_to_string(&mut got);
    assert!(got.starts_with("HTTP/1.1 200"), "{got}");
    assert!(got.ends_with("/old from 203.0.113.9"), "{got}");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_prior_knowledge_needs_a_trusted_proxy() {
    let err = Server::builder()
        .listener(ListenerSpec::tcp("open", "127.0.0.1:0"))
        .profile(Profile::new(ProfileId(0)).engine(EngineChoice::Http1OrH2ByPreface))
        .build()
        .unwrap_err();
    assert!(
        err.has(serve_net::RuleId::PriorKnowledgeWithoutProxy),
        "{err}"
    );
}
