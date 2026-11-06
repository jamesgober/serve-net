//! Plaintext HTTP/1.1 throughput of serve-net against hyper 1.x with
//! hyper-util on tokio (NFR-5: P-2 keep-alive, P-3 pipelined, P-4 churn).
//!
//! Each server runs in a process of its own and meets the same load
//! generator: a few threads, each driving its share of the connections
//! closed-loop on one `mio` poll, so the generator costs the same in every
//! row. Both servers answer `200` with `content-type: text/plain` and the
//! 11-byte body `hello world`, keep connections alive, and time out a slow
//! head (10 s), so neither skips work the other does.
//!
//! ```text
//! cargo run --release -- table [server cpus] [load threads]   every row, a markdown table
//! cargo run --release -- server serve-net 127.0.0.1:9000 [workers]
//! cargo run --release -- load 127.0.0.1:9000 256 1 5          connections, depth (0: churn), seconds
//! ```
//!
//! On Linux, `table 0-15 8` pins each server to CPUs 0-15 with `taskset`;
//! run the tool itself under `taskset -c 16-31` so the generator stays off
//! the server's cores.

use std::io::{ErrorKind, Read, Write};
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use mio::net::TcpStream;
use mio::{Events, Interest, Poll, Token};

const STACKS: &[&str] = &["serve-net", "hyper"];

/// `(row, connections, pipelining depth; 0 is a new connection per request)`.
const SHAPES: &[(&str, usize, usize)] = &[
    ("P-2 keep-alive, 256 connections", 256, 1),
    ("P-2 keep-alive, 1,000 connections", 1_000, 1),
    ("P-3 pipelined 16, 256 connections", 256, 16),
    ("P-4 churn, 64 connections", 64, 0),
];

const ROUNDS: usize = 5;
const SECONDS: u64 = 10;
const WARM: Duration = Duration::from_secs(2);
const BODY: &[u8] = b"hello world";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("server") => {
            let addr = args[2].parse().expect("address");
            let workers = args.get(3).and_then(|w| w.parse().ok()).unwrap_or(0);
            server(&args[1], addr, workers);
        }
        Some("load") => {
            let addr = args[1].parse().expect("address");
            let conns = args[2].parse().expect("connections");
            let depth = args[3].parse().expect("depth");
            let secs = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(SECONDS);
            let m = load(addr, conns, depth, threads(), Duration::from_secs(secs));
            println!("{:.0} requests/s, {} errors", m.rate(), m.errors);
        }
        Some("soak") => {
            let hours: f64 = args.get(1).and_then(|h| h.parse().ok()).unwrap_or(24.0);
            soak(Duration::from_secs_f64(hours * 3600.0));
        }
        Some("table") | None => {
            let cpus = args.get(1).cloned();
            let load_threads = args.get(2).and_then(|t| t.parse().ok()).unwrap_or_else(threads);
            table(cpus.as_deref(), load_threads);
        }
        Some(other) => panic!("unknown command {other}"),
    }
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).max(1))
}

/// How many CPUs a `taskset` list such as `0-15` or `0-3,8-11` names.
fn cpu_count(list: &str) -> usize {
    list.split(',')
        .map(|part| match part.split_once('-') {
            Some((a, b)) => b.parse::<usize>().unwrap_or(0) + 1 - a.parse::<usize>().unwrap_or(0),
            None => 1,
        })
        .sum()
}

fn start(stack: &str, port: u16, cpus: Option<&str>) -> Child {
    let exe = std::env::current_exe().expect("own path");
    let addr = format!("127.0.0.1:{port}");
    let workers = cpus.map_or(0, cpu_count).to_string();
    let mut cmd = match cpus {
        Some(list) => {
            let mut c = Command::new("taskset");
            c.arg("-c").arg(list).arg(&exe);
            c
        }
        None => Command::new(&exe),
    };
    let child = cmd
        .args(["server", stack, &addr, &workers])
        .stdout(Stdio::null())
        .spawn()
        .expect("start a server");
    let target: SocketAddr = addr.parse().unwrap();
    let start = Instant::now();
    while std::net::TcpStream::connect(target).is_err() {
        assert!(start.elapsed() < Duration::from_secs(10), "{stack} did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

/// The server's user plus system CPU time so far, in clock ticks (Linux).
fn cpu_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some(fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?)
}

fn table(cpus: Option<&str>, load_threads: usize) {
    println!(
        "server CPUs: {}; load threads: {load_threads}; {ROUNDS} rounds of {SECONDS} s after {} s warm-up; median (min to max)\n",
        cpus.unwrap_or("unpinned"),
        WARM.as_secs()
    );
    println!("| Row | Stack | Requests/s | Requests per server CPU-second | Errors |");
    println!("|---|---|---|---|---|");
    let mut port = 9_400;
    for &(row, conns, depth) in SHAPES {
        for stack in STACKS {
            port += 1;
            let mut child = start(stack, port, cpus);
            let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
            let _ = load(addr, conns, depth, load_threads, WARM);
            let mut rates = Vec::new();
            let mut per_cpu = Vec::new();
            let mut errors = 0;
            for _ in 0..ROUNDS {
                let before = cpu_ticks(child.id());
                let m = load(addr, conns, depth, load_threads, Duration::from_secs(SECONDS));
                if let (Some(a), Some(b)) = (before, cpu_ticks(child.id())) {
                    // 100 ticks per second on Linux.
                    let secs = (b.saturating_sub(a)) as f64 / 100.0;
                    if secs > 0.0 {
                        per_cpu.push(m.done as f64 / secs);
                    }
                }
                rates.push(m.rate());
                errors += m.errors;
            }
            let _ = child.kill();
            let _ = child.wait();
            let cpu = if per_cpu.is_empty() { "n/a".into() } else { spread(&mut per_cpu) };
            println!("| {row} | {stack} | {} | {cpu} | {errors} |", spread(&mut rates));
        }
    }
}

fn spread(v: &mut [f64]) -> String {
    v.sort_by(f64::total_cmp);
    format!("{:.0} ({:.0} to {:.0})", v[v.len() / 2], v[0], v[v.len() - 1])
}

struct Measured {
    done: u64,
    errors: u64,
    took: Duration,
}

impl Measured {
    fn rate(&self) -> f64 {
        self.done as f64 / self.took.as_secs_f64()
    }
}

fn load(addr: SocketAddr, conns: usize, depth: usize, threads: usize, time: Duration) -> Measured {
    let threads = threads.min(conns).max(1);
    let start = Instant::now();
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let share = conns / threads + usize::from(t < conns % threads);
            std::thread::spawn(move || drive(addr, share, depth, start + time))
        })
        .collect();
    let mut m = Measured {
        done: 0,
        errors: 0,
        took: Duration::ZERO,
    };
    for h in handles {
        let (done, errors) = h.join().expect("a load thread panicked");
        m.done += done;
        m.errors += errors;
    }
    m.took = start.elapsed();
    m
}

struct Conn {
    stream: TcpStream,
    pending: Vec<u8>,
    sent: usize,
    inbuf: Vec<u8>,
    outstanding: usize,
}

const GET: &[u8] = b"GET / HTTP/1.1\r\nHost: bench\r\n\r\n";
const GET_CLOSE: &[u8] = b"GET / HTTP/1.1\r\nHost: bench\r\nConnection: close\r\n\r\n";

fn open(addr: SocketAddr, depth: usize) -> std::io::Result<Conn> {
    let stream = TcpStream::connect(addr)?;
    stream.set_nodelay(true)?;
    let mut c = Conn {
        stream,
        pending: Vec::with_capacity(GET.len() * depth.max(1)),
        sent: 0,
        inbuf: Vec::with_capacity(16 * 1024),
        outstanding: 0,
    };
    queue(&mut c, depth);
    Ok(c)
}

fn queue(c: &mut Conn, depth: usize) {
    if depth == 0 {
        c.pending.extend_from_slice(GET_CLOSE);
        c.outstanding += 1;
    } else {
        while c.outstanding < depth {
            c.pending.extend_from_slice(GET);
            c.outstanding += 1;
        }
    }
}

/// The length of one complete response at the start of `buf`, if there is one.
fn response_len(buf: &[u8]) -> Option<usize> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let head = std::str::from_utf8(&buf[..end]).ok()?;
    let len = head
        .lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    (buf.len() >= end + len).then_some(end + len)
}

fn drive(addr: SocketAddr, conns: usize, depth: usize, until: Instant) -> (u64, u64) {
    let mut poll = Poll::new().expect("poll");
    let mut slots: Vec<Option<Conn>> = Vec::with_capacity(conns);
    let mut errors = 0u64;
    for i in 0..conns {
        match open(addr, depth) {
            Ok(mut c) => {
                poll.registry()
                    .register(&mut c.stream, Token(i), Interest::READABLE | Interest::WRITABLE)
                    .expect("register");
                slots.push(Some(c));
            }
            Err(_) => {
                errors += 1;
                slots.push(None);
            }
        }
    }
    let mut events = Events::with_capacity(1024);
    let mut done = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    while Instant::now() < until {
        poll.poll(&mut events, Some(Duration::from_millis(50)))
            .expect("poll");
        for ev in &events {
            let i = ev.token().0;
            let Some(c) = slots[i].as_mut() else { continue };
            let mut dead = false;
            // Write what is queued.
            while c.sent < c.pending.len() {
                match c.stream.write(&c.pending[c.sent..]) {
                    Ok(n) => c.sent += n,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(_) => {
                        dead = true;
                        break;
                    }
                }
            }
            if c.sent == c.pending.len() {
                c.pending.clear();
                c.sent = 0;
            }
            // Read and count complete responses.
            loop {
                match c.stream.read(&mut buf) {
                    Ok(0) => {
                        dead = true;
                        break;
                    }
                    Ok(n) => c.inbuf.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(_) => {
                        dead = true;
                        break;
                    }
                }
            }
            while let Some(len) = response_len(&c.inbuf) {
                if !c.inbuf.starts_with(b"HTTP/1.1 200") {
                    errors += 1;
                }
                c.inbuf.drain(..len);
                c.outstanding -= 1;
                done += 1;
            }
            if depth == 0 {
                if c.outstanding == 0 {
                    // Churn: the server closes; open the next connection.
                    dead = true;
                } else if dead {
                    errors += 1;
                }
            } else if dead {
                errors += 1;
            } else if c.outstanding < depth {
                queue(c, depth);
                // Push the new requests now; a writable event may not come.
                while c.sent < c.pending.len() {
                    match c.stream.write(&c.pending[c.sent..]) {
                        Ok(n) => c.sent += n,
                        Err(_) => break,
                    }
                }
                if c.sent == c.pending.len() {
                    c.pending.clear();
                    c.sent = 0;
                }
            }
            if dead {
                if let Some(mut old) = slots[i].take() {
                    let _ = poll.registry().deregister(&mut old.stream);
                }
                if Instant::now() < until {
                    match open(addr, depth) {
                        Ok(mut c) => {
                            poll.registry()
                                .register(&mut c.stream, Token(i), Interest::READABLE | Interest::WRITABLE)
                                .expect("register");
                            slots[i] = Some(c);
                        }
                        Err(_) => errors += 1,
                    }
                }
            }
        }
    }
    (done, errors)
}

fn server(stack: &str, addr: SocketAddr, workers: u16) {
    match stack {
        "serve-net" => serve_net_server(addr, workers),
        "hyper" => hyper_server(addr, workers),
        "soak" => soak_server(addr),
        other => panic!("unknown stack {other}"),
    }
}

fn serve_net_server(addr: SocketAddr, workers: u16) {
    use serve_net::{KeepAlive, ListenerSpec, Server, Workers};
    let running = Server::builder()
        .listener(ListenerSpec::tcp("bench", &addr.to_string()))
        .workers(Workers::reactor(workers))
        .keep_alive(KeepAlive::new().max_requests(u32::MAX))
        // Every benchmark connection comes from 127.0.0.1, and net-io caps a
        // source at 256 connections by default, a defence hyper does not
        // have: lifted here (to the server's own cap) so both serve the same
        // connections.
        .limits(serve_net::Limits::new().max_connections(4_000))
        .transport_limits(
            serve_net::net_io::Limits::builder()
                .max_per_source(4_000, serve_net::net_io::PrefixPolicy::DEFAULT)
                .build()
                .expect("transport limits"),
        )
        .serve(serve_net::handler(|_req, reply| {
            reply
                .status(200)
                .header("content-type", "text/plain")
                .body_static(BODY);
        }))
        .expect("serve-net server");
    running.wait();
}

fn hyper_server(addr: SocketAddr, workers: u16) {
    let mut rt = tokio::runtime::Builder::new_multi_thread();
    if workers > 0 {
        rt.worker_threads(usize::from(workers));
    }
    rt.enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(async move {
            use bytes::Bytes;
            use http_body_util::Full;
            use hyper::server::conn::http1;
            use hyper::service::service_fn;
            use hyper::{Request, Response, body::Incoming};
            use hyper_util::rt::{TokioIo, TokioTimer};

            let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                let _ = stream.set_nodelay(true);
                tokio::spawn(async move {
                    let service = service_fn(|_req: Request<Incoming>| async {
                        Ok::<_, std::convert::Infallible>(
                            Response::builder()
                                .header("content-type", "text/plain")
                                .body(Full::new(Bytes::from_static(BODY)))
                                .expect("response"),
                        )
                    });
                    let _ = http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(Duration::from_secs(10))
                        .keep_alive(true)
                        .pipeline_flush(true)
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
}

/// NFR-18: the soak server. Small and large replies, a buffered POST body,
/// and the defaults for everything else.
fn soak_server(addr: SocketAddr) {
    use serve_net::{ListenerSpec, Server};
    static BIG: [u8; 256 * 1024] = [b'x'; 256 * 1024];
    static RUNNING: std::sync::OnceLock<std::sync::Mutex<serve_net::Running>> = std::sync::OnceLock::new();
    let running = Server::builder()
        .listener(ListenerSpec::tcp("soak", &addr.to_string()))
        .serve(serve_net::handler(|req, reply| match req.path() {
            "/big" => {
                reply.status(200).body_static(&BIG);
            }
            "/stats" => {
                let t = RUNNING
                    .get()
                    .map(|r| format!("{:?}", r.lock().unwrap().stats().transport))
                    .unwrap_or_default();
                reply.text(200, &t);
            }
            "/post" => {
                let n = req.body_bytes().len();
                reply.text(200, &n.to_string());
            }
            _ => {
                reply.text(200, "ok");
            }
        }))
        .expect("soak server");
    let _ = RUNNING.set(std::sync::Mutex::new(running));
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// The server's resident memory in KiB and its open descriptors (Linux).
fn footprint(pid: u32) -> Option<(u64, usize)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    let fds = std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?.count();
    Some((rss, fds))
}

/// One round of the mix on fresh and kept connections; returns the
/// requests made, the answers that were not what each case expects, and the
/// fresh connections that failed once and then succeeded (transient).
fn soak_round(addr: SocketAddr, kept: &mut Option<(std::net::TcpStream, u32)>) -> (u64, u64, u64) {
    use std::io::{Read, Write};
    let mut done = 0;
    let mut bad = 0;
    let mut transient = 0;
    let ask = |s: &mut std::net::TcpStream, req: &[u8], want: &[u8]| -> bool {
        let debug = std::env::var_os("SOAK_DEBUG").is_some();
        if let Err(e) = s.write_all(req) {
            if debug {
                eprintln!("write failed: {:?}", e.kind());
            }
            return false;
        }
        let mut got = Vec::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            match s.read(&mut buf) {
                Ok(0) => {
                    if debug && !got.starts_with(want) {
                        eprintln!("closed after {} bytes", got.len());
                    }
                    return got.starts_with(want);
                }
                Err(e) => {
                    if debug && !got.starts_with(want) {
                        eprintln!("read failed after {} bytes: {:?}", got.len(), e.kind());
                    }
                    return got.starts_with(want);
                }
                Ok(n) => got.extend_from_slice(&buf[..n]),
            }
            if response_len(&got).is_some() {
                return got.starts_with(want);
            }
        }
    };
    let fresh = || {
        let s = match std::net::TcpStream::connect(addr) {
            Ok(s) => s,
            Err(e) => {
                if std::env::var_os("SOAK_DEBUG").is_some() {
                    eprintln!("connect failed: {:?}", e.kind());
                }
                return None;
            }
        };
        s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
        Some(s)
    };
    // Keep-alive GETs on a kept connection, replaced before the server's
    // 1,000-request limit closes it.
    if kept.as_ref().is_none_or(|(_, n)| *n >= 980) {
        *kept = fresh().map(|s| (s, 0));
    }
    if let Some((s, n)) = kept.as_mut() {
        for _ in 0..20 {
            done += 1;
            *n += 1;
            if !ask(s, GET, b"HTTP/1.1 200") {
                bad += 1;
                if std::env::var_os("SOAK_DEBUG").is_some() {
                    eprintln!("unexpected: a kept-alive GET");
                }
                *kept = None;
                break;
            }
        }
    }
    // A large reply, a POST, a churned connection, and a malformed request.
    let cases: [(&[u8], &[u8]); 4] = [
        (b"GET /big HTTP/1.1\r\nHost: s\r\nConnection: close\r\n\r\n", b"HTTP/1.1 200"),
        (
            b"POST /post HTTP/1.1\r\nHost: s\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789",
            b"HTTP/1.1 200",
        ),
        (GET_CLOSE, b"HTTP/1.1 200"),
        (b"GET / HTTP/1.1\r\nBad Header\r\n\r\n", b"HTTP/1.1 400"),
    ];
    for (req, want) in cases {
        done += 1;
        let answered = fresh().is_some_and(|mut s| ask(&mut s, req, want));
        // A fresh connection that fails once and then succeeds is the host's
        // (resets under load), counted apart; failing twice is a failure.
        let retried = !answered && fresh().is_some_and(|mut s| ask(&mut s, req, want));
        if retried {
            transient += 1;
        } else if !answered {
            bad += 1;
            if std::env::var_os("SOAK_DEBUG").is_some() {
                let mut s = fresh();
                let got = s.as_mut().map(|s| {
                    let _ = s.write_all(req);
                    let mut b = vec![0u8; 512];
                    let n = s.read(&mut b).unwrap_or(0);
                    String::from_utf8_lossy(&b[..n.min(120)]).into_owned()
                });
                eprintln!("unexpected: {:?} -> {got:?}", String::from_utf8_lossy(&req[..req.len().min(40)]));
            }
        }
    }
    (done, bad, transient)
}

fn soak(time: Duration) {
    let port = 9_390;
    let mut child = start("soak", port, None);
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let bad = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let transient = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let (stop, done, bad, transient) = (stop.clone(), done.clone(), bad.clone(), transient.clone());
            std::thread::spawn(move || {
                use std::sync::atomic::Ordering::Relaxed;
                let mut kept = None;
                while !stop.load(Relaxed) {
                    let (d, b, t) = soak_round(addr, &mut kept);
                    done.fetch_add(d, Relaxed);
                    bad.fetch_add(b, Relaxed);
                    transient.fetch_add(t, Relaxed);
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        })
        .collect();
    let start_at = Instant::now();
    // The baseline after ten minutes: pools and caches have filled by then.
    let mut baseline: Option<(u64, usize)> = None;
    let mut worst = (0u64, 0usize);
    let mut failed = None;
    while start_at.elapsed() < time {
        std::thread::sleep(Duration::from_secs(60));
        use std::sync::atomic::Ordering::Relaxed;
        let Some((rss, fds)) = footprint(child.id()) else {
            failed = Some("the server is gone".to_owned());
            break;
        };
        let minutes = start_at.elapsed().as_secs() / 60;
        if minutes >= 10 && baseline.is_none() {
            baseline = Some((rss, fds));
        }
        worst = (worst.0.max(rss), worst.1.max(fds));
        println!(
            "{minutes:>5} min: {} requests, {} unexpected, {} transient, rss {rss} KiB, {fds} descriptors",
            done.load(Relaxed),
            bad.load(Relaxed),
            transient.load(Relaxed)
        );
        // About one request in six opens a fresh connection; more than 0.1%
        // of those failing once is not noise.
        if transient.load(Relaxed) * 6 * 1000 > done.load(Relaxed).max(60_000) {
            failed = Some(format!("{} transient failures", transient.load(Relaxed)));
            break;
        }
        if let Some((b_rss, b_fds)) = baseline {
            // Bounded means flat: more than 25% over the warm baseline, or 64
            // descriptors more, is growth.
            if rss > b_rss + b_rss / 4 || fds > b_fds + 64 {
                failed = Some(format!("growth: rss {b_rss} to {rss} KiB, descriptors {b_fds} to {fds}"));
                break;
            }
        }
        if bad.load(Relaxed) > 0 {
            if let Ok(mut s) = std::net::TcpStream::connect(addr) {
                let _ = s.write_all(b"GET /stats HTTP/1.1
Host: s
Connection: close

");
                let mut t = String::new();
                let _ = s.read_to_string(&mut t);
                println!("server stats: {}", t.split("

").nth(1).unwrap_or(""));
            }
            failed = Some(format!("{} unexpected answers", bad.load(Relaxed)));
            break;
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for w in workers {
        let _ = w.join();
    }
    let _ = child.kill();
    let _ = child.wait();
    use std::sync::atomic::Ordering::Relaxed;
    println!(
        "soak: {:.1} h, {} requests, {} transient, baseline {baseline:?}, worst rss {} KiB and {} descriptors: {}",
        start_at.elapsed().as_secs_f64() / 3600.0,
        done.load(Relaxed),
        transient.load(Relaxed),
        worst.0,
        worst.1,
        failed.as_deref().unwrap_or("clean")
    );
    if failed.is_some() {
        std::process::exit(1);
    }
}
