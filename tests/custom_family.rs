//! A custom protocol family over real sockets, built from the public API
//! alone, as a database wire protocol would be: a small key-value protocol
//! served through `bind_family`.
//!
//! The protocol, one request per line:
//!
//! ```text
//! GET <key>\n              -> +<len>\n<value>   or  -404\n
//! PUT <key> <len>\n<value> -> +0\n
//! QUIT\n                   -> +0\n, then the server closes
//! ```
//!
//! Refusals are `-<status>\n` and close the connection.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serve_net::engine::{
    Engine, EngineError, EngineEvent, EngineProgress, Family, HeadContext, OutBuf, Refusal,
    RefusalKind, Service,
};
use serve_net::{
    ClientInfo, EngineChoice, Envelope, Limits, ListenerSpec, Outcome, Profile, ProfileId, Reply,
    Running, Server,
};

/// The longest request line the engine holds.
const MAX_LINE: usize = 256;
/// The input the engine holds at once: a line and the start of a value.
const MAX_INPUT: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verb {
    Get,
    Put,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KvState {
    /// Waiting for a request line.
    Line,
    /// The line is parsed; this many value bytes are outstanding.
    Body(u64),
    /// The request is complete; its reply is not.
    Reply,
    Closed,
}

/// The engine: a line, then a value of the declared length.
struct KvEngine {
    input: Vec<u8>,
    /// Bytes at the front of `input` handed out as body; dropped at the next call.
    handed: usize,
    line: Vec<u8>,
    verb: Verb,
    key: (usize, usize),
    declared: Option<u64>,
    state: KvState,
    eof: bool,
    keep_alive: bool,
    served: u64,
    wipe: bool,
}

/// The head the engine shows: borrowed from its line.
#[derive(Clone, Copy, Debug)]
struct KvRaw<'a> {
    verb: Verb,
    key: &'a [u8],
}

fn zero(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        *b = 0;
    }
    std::hint::black_box(bytes);
}

impl KvEngine {
    fn new(wipe: bool) -> Self {
        KvEngine {
            input: Vec::new(),
            handed: 0,
            line: Vec::new(),
            verb: Verb::Get,
            key: (0, 0),
            declared: None,
            state: KvState::Line,
            eof: false,
            keep_alive: true,
            served: 0,
            wipe,
        }
    }

    fn drop_handed(&mut self) {
        if self.handed > 0 {
            if self.wipe {
                zero(&mut self.input[..self.handed]);
            }
            self.input.drain(..self.handed);
            self.handed = 0;
        }
    }

    fn malformed() -> EngineError {
        EngineError {
            kind: RefusalKind::Malformed,
            status: 400,
        }
    }

    /// Parses `self.line` into the verb, key, and declared length.
    fn parse(&mut self) -> Result<(), EngineError> {
        let text = core::str::from_utf8(&self.line).map_err(|_| Self::malformed())?;
        let mut words = text.split(' ');
        let verb = match words.next() {
            Some("GET") => Verb::Get,
            Some("PUT") => Verb::Put,
            Some("QUIT") => Verb::Quit,
            _ => return Err(Self::malformed()),
        };
        let key = words.next().unwrap_or("");
        let start = text.find(' ').map_or(text.len(), |i| i + 1);
        let declared = match (verb, words.next()) {
            (Verb::Put, Some(len)) => Some(len.parse::<u64>().map_err(|_| Self::malformed())?),
            (Verb::Get, None) if !key.is_empty() => None,
            (Verb::Quit, None) if key.is_empty() => None,
            _ => return Err(Self::malformed()),
        };
        if words.next().is_some() || (verb != Verb::Quit && key.is_empty()) {
            return Err(Self::malformed());
        }
        self.verb = verb;
        self.key = (start.min(text.len()), (start + key.len()).min(text.len()));
        self.declared = declared;
        Ok(())
    }
}

impl Engine for KvEngine {
    type Head<'a> = KvRaw<'a>;

    fn feed(&mut self, bytes: &[u8]) -> usize {
        self.drop_handed();
        let room = MAX_INPUT.saturating_sub(self.input.len());
        let n = room.min(bytes.len());
        self.input.extend_from_slice(&bytes[..n]);
        n
    }

    fn feed_eof(&mut self) {
        self.eof = true;
    }

    fn poll(&mut self) -> Result<EngineEvent<'_>, EngineError> {
        self.drop_handed();
        match self.state {
            KvState::Closed => Ok(EngineEvent::Closed),
            KvState::Reply => Ok(EngineEvent::AwaitingReply),
            KvState::Line => {
                let Some(end) = self.input.iter().position(|&b| b == b'\n') else {
                    if self.input.len() >= MAX_LINE {
                        return Err(EngineError {
                            kind: RefusalKind::TooLarge,
                            status: 431,
                        });
                    }
                    if self.eof {
                        self.state = KvState::Closed;
                        return Ok(EngineEvent::Closed);
                    }
                    return Ok(EngineEvent::NeedInput);
                };
                if end >= MAX_LINE {
                    return Err(EngineError {
                        kind: RefusalKind::TooLarge,
                        status: 431,
                    });
                }
                if self.wipe {
                    zero(&mut self.line);
                }
                self.line.clear();
                self.line.extend_from_slice(&self.input[..end]);
                self.handed = end + 1;
                self.parse()?;
                self.state = KvState::Body(self.declared.unwrap_or(0));
                Ok(EngineEvent::Head)
            }
            KvState::Body(0) => {
                self.state = KvState::Reply;
                Ok(EngineEvent::BodyEnd)
            }
            KvState::Body(left) => {
                if self.input.is_empty() {
                    if self.eof {
                        self.state = KvState::Closed;
                        return Ok(EngineEvent::Closed);
                    }
                    return Ok(EngineEvent::NeedInput);
                }
                let n = usize::try_from(left)
                    .unwrap_or(usize::MAX)
                    .min(self.input.len());
                self.state = KvState::Body(left - n as u64);
                self.handed = n;
                Ok(EngineEvent::Body(&self.input[..n]))
            }
        }
    }

    fn head(&self) -> Option<KvRaw<'_>> {
        match self.state {
            KvState::Body(_) | KvState::Reply => Some(KvRaw {
                verb: self.verb,
                key: &self.line[self.key.0..self.key.1],
            }),
            _ => None,
        }
    }

    fn declared_length(&self) -> Option<u64> {
        self.declared
    }

    fn head_len(&self) -> usize {
        self.line.len() + 1
    }

    fn progress(&self) -> EngineProgress {
        match self.state {
            KvState::Line if self.input.len() == self.handed => EngineProgress::Idle,
            KvState::Line => EngineProgress::Head,
            KvState::Body(_) => EngineProgress::Body,
            KvState::Reply => EngineProgress::AwaitingReply,
            KvState::Closed => EngineProgress::Closed,
        }
    }

    fn refuse(&mut self, refusal: &Refusal, out: &mut OutBuf) {
        out.push(format!("-{}\n", refusal.status).as_bytes());
        self.keep_alive = false;
        self.state = KvState::Closed;
    }

    fn close_after_reply(&mut self) {
        self.keep_alive = false;
    }

    fn keep_alive(&self) -> bool {
        self.keep_alive
    }

    fn requests_served(&self) -> u64 {
        self.served
    }

    fn remaining_input(&self) -> &[u8] {
        &[]
    }

    fn reset(&mut self) {
        if self.wipe {
            zero(&mut self.input);
            zero(&mut self.line);
        }
        let wipe = self.wipe;
        let input = core::mem::take(&mut self.input);
        let line = core::mem::take(&mut self.line);
        *self = KvEngine::new(wipe);
        self.input = input;
        self.input.clear();
        self.line = line;
        self.line.clear();
    }

    fn shrink_idle(&mut self) -> bool {
        let had = self.input.capacity() + self.line.capacity();
        self.drop_handed();
        self.input.shrink_to_fit();
        self.line.shrink_to_fit();
        had > self.input.capacity() + self.line.capacity()
    }
}

/// The family: the head and request views and the reply data.
struct Kv;

#[derive(Clone, Copy, Debug)]
struct KvHead<'a> {
    verb: Verb,
    key: &'a [u8],
}

struct KvRequest<'a> {
    head: KvHead<'a>,
    value: &'a [u8],
}

#[derive(Default)]
struct KvParts {
    /// 0 or 200: `+<len>\n<value>`; anything else `-<status>\n`.
    status: u16,
    value: Vec<u8>,
}

impl Family for Kv {
    type Engine = KvEngine;
    type Head<'a> = KvHead<'a>;
    type Request<'a> = KvRequest<'a>;
    type Parts = KvParts;

    fn engine(&self, profile: &Profile, _limits: &Limits) -> KvEngine {
        KvEngine::new(profile.wipes_buffers())
    }

    fn head<'a>(
        &self,
        raw: KvRaw<'a>,
        cx: &HeadContext<'a>,
    ) -> Result<(KvHead<'a>, ClientInfo), EngineError> {
        Ok((
            KvHead {
                verb: raw.verb,
                key: raw.key,
            },
            *cx.client(),
        ))
    }

    fn request<'a>(&self, head: KvHead<'a>, body: &'a [u8]) -> KvRequest<'a> {
        KvRequest { head, value: body }
    }

    fn new_parts(&self) -> KvParts {
        KvParts::default()
    }

    fn clear_parts(&self, parts: &mut KvParts, wipe: bool) {
        if wipe {
            zero(&mut parts.value);
        }
        parts.value.clear();
        parts.status = 0;
    }

    fn respond(
        &self,
        engine: &mut KvEngine,
        parts: &mut KvParts,
        _unix_secs: u64,
        out: &mut OutBuf,
    ) -> Result<u16, EngineError> {
        if engine.state != KvState::Reply {
            return Err(EngineError {
                kind: RefusalKind::Internal,
                status: 500,
            });
        }
        let status = if parts.status == 0 { 200 } else { parts.status };
        if status == 200 {
            out.push(format!("+{}\n", parts.value.len()).as_bytes());
            out.push(&parts.value);
        } else {
            out.push(format!("-{status}\n").as_bytes());
        }
        engine.served += 1;
        engine.state = if engine.keep_alive {
            KvState::Line
        } else {
            KvState::Closed
        };
        Ok(status)
    }
}

/// The service: a map behind a lock.
#[derive(Default)]
struct Store {
    map: Mutex<HashMap<Vec<u8>, Vec<u8>>>,
}

impl Service<Kv> for Store {
    fn call(&self, request: &KvRequest<'_>, _env: &Envelope, reply: &mut Reply<Kv>) -> Outcome {
        let mut map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        let parts = reply.parts_mut();
        match request.head.verb {
            Verb::Get => match map.get(request.head.key) {
                Some(v) => parts.value.extend_from_slice(v),
                None => parts.status = 404,
            },
            Verb::Put => {
                map.insert(request.head.key.to_vec(), request.value.to_vec());
            }
            Verb::Quit => return Outcome::Close,
        }
        Outcome::Done
    }
}

fn kv_profile() -> Profile {
    Profile::new(ProfileId(0)).engine(EngineChoice::Custom { alpn: &[] })
}

fn start(limits: Limits) -> (Running, SocketAddr) {
    let server = Server::builder()
        .listener(ListenerSpec::tcp("kv", "127.0.0.1:0"))
        .profile(kv_profile())
        .limits(limits)
        .build()
        .unwrap()
        .bind_family(Kv, Store::default())
        .unwrap()
        .open()
        .unwrap();
    let addr = server.local_addr().unwrap();
    (server, addr)
}

fn connect(addr: SocketAddr) -> TcpStream {
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s
}

/// Reads until `want` bytes arrived or the server closed.
fn read_n(s: &mut TcpStream, want: usize) -> Vec<u8> {
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    while got.len() < want {
        let n = s.read(&mut buf).expect("read timed out");
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    got
}

/// Reads until the server closes.
fn read_to_close(s: &mut TcpStream) -> Vec<u8> {
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf) {
            Ok(0) => return got,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            // A reset after the reply counts as the close.
            Err(e)
                if e.kind() != std::io::ErrorKind::WouldBlock
                    && e.kind() != std::io::ErrorKind::TimedOut =>
            {
                return got;
            }
            Err(e) => panic!("no close: {e}, got {:?}", String::from_utf8_lossy(&got)),
        }
    }
}

#[test]
fn test_custom_family_serves_pipelined_requests_on_one_connection() {
    let (server, addr) = start(Limits::new());
    let mut s = connect(addr);
    s.write_all(b"PUT colour 4\nblueGET colour\nGET shape\n")
        .unwrap();
    let want = b"+0\n+4\nblue-404\n";
    assert_eq!(read_n(&mut s, want.len()), want);
    // The same connection carries more.
    s.write_all(b"PUT shape 6\nsquareGET shape\n").unwrap();
    let want = b"+0\n+6\nsquare";
    assert_eq!(read_n(&mut s, want.len()), want);
    s.write_all(b"QUIT\n").unwrap();
    assert_eq!(read_to_close(&mut s), b"+0\n");
    let stats = server.stats();
    assert_eq!(stats.requests, 6, "{stats:?}");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_custom_family_value_split_across_writes() {
    let (server, addr) = start(Limits::new());
    let mut s = connect(addr);
    s.write_all(b"PUT k 10\nhello").unwrap();
    std::thread::sleep(Duration::from_millis(50));
    s.write_all(b" kv!\nGET k\n").unwrap();
    let want = b"+0\n+10\nhello kv!\n";
    assert_eq!(read_n(&mut s, want.len()), want);
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_custom_family_refusals_use_the_protocol() {
    let (server, addr) = start(Limits::new().max_body(8));
    // A declared value over max_body: refused at the head, in the family's terms.
    let mut s = connect(addr);
    s.write_all(b"PUT big 9\n").unwrap();
    assert_eq!(read_to_close(&mut s), b"-413\n");
    // A malformed line.
    let mut s = connect(addr);
    s.write_all(b"DROP everything\n").unwrap();
    assert_eq!(read_to_close(&mut s), b"-400\n");
    // A line that never ends.
    let mut s = connect(addr);
    s.write_all(&[b'x'; MAX_LINE + 1]).unwrap();
    assert_eq!(read_to_close(&mut s), b"-431\n");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_custom_family_drain_closes_an_idle_connection() {
    let (server, addr) = start(Limits::new());
    let mut s = connect(addr);
    s.write_all(b"GET nothing\n").unwrap();
    assert_eq!(read_n(&mut s, 5), b"-404\n");
    let report = server.drain(Duration::from_secs(2));
    assert_eq!(read_to_close(&mut s), b"");
    assert_eq!(report.killed, 0, "{report:?}");
}

#[test]
fn test_bind_family_refuses_a_profile_that_serves_http() {
    let built = Server::builder()
        .listener(ListenerSpec::tcp("kv", "127.0.0.1:0"))
        .build()
        .unwrap();
    let err = built
        .bind_family(Kv, Store::default())
        .map(|_| ())
        .expect_err("bound");
    let text = err.to_string();
    assert!(text.contains("engine_family"), "{text}");
}

#[cfg(feature = "http1")]
#[test]
fn test_bind_refuses_a_profile_that_serves_a_custom_family() {
    let built = Server::builder()
        .listener(ListenerSpec::tcp("kv", "127.0.0.1:0"))
        .profile(kv_profile())
        .build()
        .unwrap();
    let err = built
        .bind(serve_net::handler(|_req, reply| {
            reply.text(200, "http");
        }))
        .map(|_| ())
        .expect_err("bound");
    assert!(err.to_string().contains("engine_family"), "{err}");
}

#[test]
fn test_reload_keeps_each_listener_in_the_family() {
    let (server, addr) = start(Limits::new());
    // Profile 0 back to HTTP under a custom family: refused, nothing changes.
    let mut next = serve_net::ServerConfig::new();
    next.listeners = vec![ListenerSpec::tcp("kv", "127.0.0.1:0")];
    next.profiles = vec![Profile::new(ProfileId(0))];
    let err = server.reload(next).map(|_| ()).expect_err("reloaded");
    assert!(err.to_string().contains("engine_family"), "{err}");
    // No profile at all means the default, which is HTTP: refused too.
    let mut next = serve_net::ServerConfig::new();
    next.listeners = vec![ListenerSpec::tcp("kv", "127.0.0.1:0")];
    let err = server.reload(next).map(|_| ()).expect_err("reloaded");
    assert!(err.to_string().contains("engine_family"), "{err}");
    // The same family reloads.
    let mut next = serve_net::ServerConfig::new();
    next.listeners = vec![ListenerSpec::tcp("kv", "127.0.0.1:0")];
    next.profiles = vec![kv_profile()];
    server.reload(next).unwrap();
    let mut s = connect(addr);
    s.write_all(b"GET k\n").unwrap();
    assert_eq!(read_n(&mut s, 5), b"-404\n");
    let _ = server.drain(Duration::from_secs(2));
}
