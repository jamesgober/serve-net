//! Streamed request bodies and streamed replies, over real sockets.

#![cfg(feature = "http1")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serve_net::{
    BodyPolicy, Checkpoint, CheckpointAt, Envelope, HeadAction, HttpBody, HttpHead, HttpReply,
    HttpRequest, HttpService, ListenerSpec, Outcome, Server, Verdict,
};

fn connect(addr: SocketAddr) -> TcpStream {
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s
}

/// Everything the server sends until it closes; a reset ends it too.
fn read_all(s: &mut TcpStream) -> Vec<u8> {
    let mut got = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    while let Ok(n @ 1..) = s.read(&mut buf) {
        got.extend_from_slice(&buf[..n]);
    }
    got
}

/// Reads one chunked reply and decodes its body; `None` when it is cut short.
fn read_chunked(s: &mut TcpStream) -> (String, Option<Vec<u8>>) {
    let mut got = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&got[..end]).into_owned();
            let mut at = end + 4;
            let mut body = Vec::new();
            loop {
                let Some(eol) = got[at..].windows(2).position(|w| w == b"\r\n") else {
                    match s.read(&mut buf) {
                        Ok(n @ 1..) => {
                            got.extend_from_slice(&buf[..n]);
                            continue;
                        }
                        _ => return (head, None),
                    }
                };
                let size_text = std::str::from_utf8(&got[at..at + eol]).unwrap();
                let size = usize::from_str_radix(size_text, 16).unwrap();
                let start = at + eol + 2;
                while got.len() < start + size + 2 {
                    match s.read(&mut buf) {
                        Ok(n @ 1..) => got.extend_from_slice(&buf[..n]),
                        _ => return (head, None),
                    }
                }
                if size == 0 {
                    return (head, Some(body));
                }
                body.extend_from_slice(&got[start..start + size]);
                at = start + size + 2;
            }
        }
        match s.read(&mut buf) {
            Ok(n @ 1..) => got.extend_from_slice(&buf[..n]),
            _ => return (String::from_utf8_lossy(&got).into_owned(), None),
        }
    }
}

/// A service the test keeps a handle on.
struct Held<T>(Arc<T>);

impl<T> std::ops::Deref for Held<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// Keeps what each call saw, and answers the way the path says.
#[derive(Default)]
struct Upload {
    pieces: Mutex<Vec<(Vec<u8>, bool)>>,
}

impl HttpService for Held<Upload> {
    fn head(&self, _head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
        HeadAction::ProceedWith(BodyPolicy::Stream)
    }

    fn call(&self, req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
        let HttpBody::Chunk { bytes, last } = req.body() else {
            panic!("not a piece: {:?}", req.body());
        };
        self.pieces.lock().unwrap().push((bytes.to_vec(), last));
        match req.path() {
            "/early" if !last => {
                reply.text(200, "enough");
                Outcome::Done
            }
            "/lied" if last => Outcome::More,
            _ if last => {
                let total: usize = self.pieces.lock().unwrap().iter().map(|p| p.0.len()).sum();
                reply.text(200, &total.to_string());
                Outcome::Done
            }
            _ => Outcome::More,
        }
    }
}

fn upload_server() -> (Arc<Upload>, serve_net::Running, SocketAddr) {
    let service = Arc::new(Upload::default());
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .serve(Held(Arc::clone(&service)))
        .unwrap();
    let addr = server.local_addr().unwrap();
    (service, server, addr)
}

#[test]
fn test_stream_body_arrives_in_pieces_and_the_connection_lives() {
    let (service, server, addr) = upload_server();
    let mut s = connect(addr);
    s.write_all(b"POST /x HTTP/1.1\r\nHost: x\r\nContent-Length: 6\r\n\r\nabc")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while service.pieces.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the first piece never came");
        std::thread::sleep(Duration::from_millis(2));
    }
    s.write_all(b"def").unwrap();
    let mut buf = [0u8; 512];
    let n = s.read(&mut buf).unwrap();
    let got = String::from_utf8_lossy(&buf[..n]);
    assert!(
        got.starts_with("HTTP/1.1 200") && got.ends_with('6'),
        "{got}"
    );
    {
        let pieces = service.pieces.lock().unwrap();
        assert!(pieces.len() >= 2, "{pieces:?}");
        assert_eq!(pieces.iter().filter(|p| p.1).count(), 1, "{pieces:?}");
        assert!(pieces.last().unwrap().1);
        let whole: Vec<u8> = pieces.iter().flat_map(|p| p.0.clone()).collect();
        assert_eq!(whole, b"abcdef");
        drop(pieces);
    }
    // The same connection takes the next request.
    s.write_all(b"POST /x HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n")
        .unwrap();
    let n = s.read(&mut buf).unwrap();
    assert!(buf[..n].starts_with(b"HTTP/1.1 200"));
    drop(s);
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_stream_body_empty_is_one_last_piece() {
    let (service, server, addr) = upload_server();
    let mut s = connect(addr);
    s.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .unwrap();
    assert!(read_all(&mut s).starts_with(b"HTTP/1.1 200"));
    assert_eq!(*service.pieces.lock().unwrap(), vec![(Vec::new(), true)]);
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_stream_body_answered_early_closes() {
    let (service, server, addr) = upload_server();
    let mut s = connect(addr);
    s.write_all(b"POST /early HTTP/1.1\r\nHost: x\r\nContent-Length: 100\r\n\r\nabc")
        .unwrap();
    let got = read_all(&mut s);
    assert!(got.starts_with(b"HTTP/1.1 200") && got.ends_with(b"enough"));
    assert_eq!(service.pieces.lock().unwrap().len(), 1);
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_stream_body_more_after_the_last_piece_is_a_500() {
    let (_service, server, addr) = upload_server();
    let mut s = connect(addr);
    s.write_all(b"POST /lied HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n")
        .unwrap();
    assert!(read_all(&mut s).starts_with(b"HTTP/1.1 500"));
    let _ = server.drain(Duration::from_secs(2));
}

/// Streams `parts` parts of `size` bytes; the part number comes from `sent`.
struct Download {
    parts: u64,
    size: usize,
    cut_at: Option<u64>,
    calls: Mutex<u64>,
}

impl Download {
    fn new(parts: u64, size: usize) -> Self {
        Download {
            parts,
            size,
            cut_at: None,
            calls: Mutex::new(0),
        }
    }
}

impl HttpService for Held<Download> {
    fn call(&self, req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
        *self.calls.lock().unwrap() += 1;
        reply.status(201).header("x-part", "kept");
        let block = vec![b'x'; self.size];
        let mut stream = reply.stream();
        let part = stream.sent() / self.size as u64;
        if self.cut_at == Some(part) {
            assert_ne!(req.path(), "/", "a service bug");
        }
        stream.send(&block);
        if part + 1 < self.parts {
            Outcome::More
        } else if req.path() == "/close" {
            Outcome::Close
        } else {
            Outcome::Done
        }
    }
}

fn serve_download(service: &Arc<Download>) -> (serve_net::Running, SocketAddr) {
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .catch_service_panics(true)
        .serve(Held(Arc::clone(service)))
        .unwrap();
    let addr = server.local_addr().unwrap();
    (server, addr)
}

const GET: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";

#[test]
fn test_stream_reply_parts_and_keep_alive() {
    let service = Arc::new(Download::new(3, 5));
    let (server, addr) = serve_download(&service);
    let mut s = connect(addr);
    for round in 1..=2 {
        s.write_all(GET).unwrap();
        let (head, body) = read_chunked(&mut s);
        let lower = head.to_ascii_lowercase();
        assert!(head.starts_with("HTTP/1.1 201"), "{head}");
        assert!(lower.contains("x-part: kept"), "{head}");
        assert!(lower.contains("transfer-encoding: chunked"), "{head}");
        assert_eq!(body.unwrap(), vec![b'x'; 15]);
        assert_eq!(*service.calls.lock().unwrap(), 3 * round);
    }
    drop(s);
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_stream_reply_close_outcome_ends_the_connection() {
    let service = Arc::new(Download::new(2, 4));
    let (server, addr) = serve_download(&service);
    let mut s = connect(addr);
    s.write_all(b"GET /close HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    let (_, body) = read_chunked(&mut s);
    assert_eq!(body.unwrap().len(), 8);
    assert!(matches!(s.read(&mut [0u8; 8]), Ok(0) | Err(_)));
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_stream_reply_to_head_sends_the_head_only() {
    let service = Arc::new(Download::new(3, 5));
    let (server, addr) = serve_download(&service);
    let mut s = connect(addr);
    s.write_all(b"HEAD / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .unwrap();
    let got = read_all(&mut s);
    assert!(got.starts_with(b"HTTP/1.1 201"));
    assert!(got.ends_with(b"\r\n\r\n"), "no body");
    let _ = server.drain(Duration::from_secs(2));
}

/// More than the socket buffers hold: a reader that waits holds the service
/// back, and every byte arrives once it reads.
#[test]
fn test_stream_reply_slow_reader_holds_the_service_back() {
    const PART: usize = 64 * 1024;
    const PARTS: u64 = 400;
    let service = Arc::new(Download::new(PARTS, PART));
    let (server, addr) = serve_download(&service);
    let mut s = connect(addr);
    s.write_all(GET).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        *service.calls.lock().unwrap() < PARTS,
        "the service ran ahead of the reader"
    );
    let (_, body) = read_chunked(&mut s);
    assert_eq!(body.unwrap().len(), PART * PARTS as usize);
    drop(s);
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_stream_reply_panic_cuts_the_connection_and_the_server_lives() {
    let mut service = Download::new(4, 5);
    service.cut_at = Some(2);
    let service = Arc::new(service);
    let (server, addr) = serve_download(&service);
    let mut s = connect(addr);
    s.write_all(GET).unwrap();
    let (head, body) = read_chunked(&mut s);
    assert!(head.starts_with("HTTP/1.1 201"), "{head}");
    assert_eq!(body, None, "a cut body never looks complete");
    let mut s = connect(addr);
    s.write_all(b"GET /fine HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    assert_eq!(read_chunked(&mut s).1.unwrap().len(), 20);
    drop(s);
    let _ = server.drain(Duration::from_secs(2));
}

struct Revoke(Arc<AtomicBool>);

impl Checkpoint for Revoke {
    fn check(&self, _env: &Envelope, at: CheckpointAt) -> Verdict {
        if at == CheckpointAt::BeforeReply && self.0.load(Ordering::Acquire) {
            Verdict::Deny { status: 403 }
        } else {
            Verdict::Pass
        }
    }
}

/// Revoked while the reply runs: the rest never leaves.
#[test]
fn test_stream_reply_checkpoint_denies_a_later_part() {
    struct Gated(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>);
    impl HttpService for Gated {
        fn call(&self, _req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
            let mut stream = reply.stream();
            if stream.sent() == 0 {
                stream.send(b"first");
                return Outcome::More;
            }
            self.0.send(()).unwrap();
            let _ = self.1.lock().unwrap().recv();
            stream.send(b"secret");
            Outcome::More
        }
    }
    let revoked = Arc::new(AtomicBool::new(false));
    let (entered, in_call) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .checkpoint(Revoke(Arc::clone(&revoked)))
        .serve(Gated(entered, Mutex::new(released)))
        .unwrap();
    let mut s = connect(server.local_addr().unwrap());
    s.write_all(GET).unwrap();
    in_call.recv_timeout(Duration::from_secs(5)).unwrap();
    revoked.store(true, Ordering::Release);
    release.send(()).unwrap();
    let got = read_all(&mut s);
    let text = String::from_utf8_lossy(&got);
    assert!(text.contains("first"), "{text}");
    assert!(!text.contains("secret"), "{text}");
    assert!(!text.contains("403"), "no refusal after the head: {text}");
    let _ = server.drain(Duration::from_secs(2));
}

/// Both directions at once: each piece comes back as a part.
#[test]
fn test_stream_echo_body_to_reply() {
    struct Echo;
    impl HttpService for Echo {
        fn head(&self, _head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
            HeadAction::ProceedWith(BodyPolicy::Stream)
        }
        fn call(&self, req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
            let HttpBody::Chunk { bytes, last } = req.body() else {
                panic!("not a piece");
            };
            reply.stream().send(bytes);
            if last { Outcome::Done } else { Outcome::More }
        }
    }
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .serve(Echo)
        .unwrap();
    let mut s = connect(server.local_addr().unwrap());
    s.write_all(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 8\r\n\r\nabcd")
        .unwrap();
    std::thread::sleep(Duration::from_millis(50));
    s.write_all(b"efgh").unwrap();
    let (_, body) = read_chunked(&mut s);
    assert_eq!(body.unwrap(), b"abcdefgh");
    drop(s);
    let _ = server.drain(Duration::from_secs(2));
}

/// A drain lets a streamed reply finish, and closes after it.
#[test]
fn test_stream_reply_finishes_through_a_drain() {
    const PART: usize = 64 * 1024;
    let service = Arc::new(Download::new(100, PART));
    let (server, addr) = serve_download(&service);
    let mut s = connect(addr);
    s.write_all(GET).unwrap();
    let mut first = [0u8; 512];
    let n = s.read(&mut first).unwrap();
    assert!(first[..n].starts_with(b"HTTP/1.1 201"));
    let drained = std::thread::spawn(move || server.drain(Duration::from_secs(10)));
    let rest = read_all(&mut s);
    assert!(
        rest.ends_with(b"0\r\n\r\n"),
        "cut after {} bytes",
        rest.len()
    );
    let report = drained.join().unwrap();
    assert_eq!(report.killed, 0, "{report:?}");
}
