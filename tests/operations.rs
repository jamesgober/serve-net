//! Keeping a server alive and answering under pressure: the watchdog, lane
//! reserves, and the offload pool.

#![cfg(feature = "http1")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serve_net::{
    Envelope, Finished, HttpReply, HttpRequest, HttpService, ListenerSpec, Notify, Offload,
    OffloadJob, OffloadPool, Outcome, RequestEnd, Server, WatchdogPinger, WorkClass,
};

const GET: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";

fn ask(addr: SocketAddr, request: &[u8]) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(request).unwrap();
    let mut got = String::new();
    let _ = s.read_to_string(&mut got);
    got
}

#[derive(Default)]
struct Pings(AtomicU64);

impl Notify for Pings {
    fn watchdog(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn wait_for_pings(pings: &Pings, at_least: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while pings.0.load(Ordering::Relaxed) < at_least {
        assert!(Instant::now() < deadline, "no pings");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn test_watchdog_pings_an_idle_server() {
    let server = serve_net::http("127.0.0.1:0", |_req, reply| {
        reply.text(200, "ok");
    })
    .unwrap();
    let pings = Arc::new(Pings::default());
    let pinger = WatchdogPinger::start(&server, pings.clone(), Duration::from_millis(20));
    wait_for_pings(&pings, 3);
    pinger.stop();
    let stopped = pings.0.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(pings.0.load(Ordering::Relaxed), stopped, "stop stops");
    let _ = server.drain(Duration::from_secs(1));
}

#[test]
fn test_watchdog_stops_while_a_worker_is_stuck() {
    let (entered_tx, entered) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let (entered_tx, released) = (Mutex::new(entered_tx), Mutex::new(released));
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .workers(serve_net::Workers::blocking(2))
        .serve(serve_net::handler(move |_req, reply| {
            entered_tx.lock().unwrap().send(()).unwrap();
            let _ = released.lock().unwrap().recv();
            reply.text(200, "late");
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    let pings = Arc::new(Pings::default());
    let pinger = WatchdogPinger::start(&server, pings.clone(), Duration::from_millis(20));
    wait_for_pings(&pings, 2);

    let client = std::thread::spawn(move || ask(addr, GET));
    entered.recv().unwrap();
    // One wake may have seen the worker move as it entered the call.
    std::thread::sleep(Duration::from_millis(150));
    let stuck = pings.0.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(pings.0.load(Ordering::Relaxed), stuck, "a stuck worker");

    release.send(()).unwrap();
    assert!(client.join().unwrap().starts_with("HTTP/1.1 200"));
    wait_for_pings(&pings, stuck + 2);
    pinger.stop();
    let _ = server.drain(Duration::from_secs(1));
}

#[test]
fn test_watchdog_ends_with_the_server() {
    let server = serve_net::http("127.0.0.1:0", |_req, reply| {
        reply.text(200, "ok");
    })
    .unwrap();
    let pings = Arc::new(Pings::default());
    let pinger = WatchdogPinger::start(&server, pings.clone(), Duration::from_millis(20));
    wait_for_pings(&pings, 1);
    let _ = server.drain(Duration::from_secs(1));
    std::thread::sleep(Duration::from_millis(40));
    let after = pings.0.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(pings.0.load(Ordering::Relaxed), after);
    pinger.stop();
}

/// T-25: a bulk flood holds every slot it can; the reserved lane still answers.
#[test]
fn test_bulk_flood_critical_lane_still_admitted() {
    use serve_net::{InFlightCap, Lane, Profile, ProfileId};

    let (entered_tx, entered) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let (entered_tx, released) = (Mutex::new(entered_tx), Mutex::new(released));
    let server = Server::builder()
        .listener(ListenerSpec::tcp("bulk", "127.0.0.1:0").profile(ProfileId(0)))
        .listener(ListenerSpec::tcp("admin", "127.0.0.1:0").profile(ProfileId(1)))
        .profile(Profile::new(ProfileId(0)).lane(Lane(4)))
        .profile(Profile::new(ProfileId(1)).lane(Lane(0)))
        .limits(serve_net::Limits::new().max_connections(8))
        .workers(serve_net::Workers::blocking(8))
        .admit(InFlightCap::new().reserve(Lane(0), 50))
        .serve(serve_net::handler(move |req, reply| {
            if req.path() == "/slow" {
                entered_tx.lock().unwrap().send(()).unwrap();
                let _ = released.lock().unwrap().recv();
            }
            reply.text(200, "ok");
        }))
        .unwrap();
    let addrs: Vec<SocketAddr> = server
        .listeners()
        .iter()
        .map(|l| l.local.socket_addr().unwrap())
        .collect();
    let (bulk, admin) = (addrs[0], addrs[1]);

    // Half of the eight slots are the pool: four bulk requests fill it.
    const SLOW: &[u8] = b"GET /slow HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";
    let held: Vec<_> = (0..4)
        .map(|_| std::thread::spawn(move || ask(bulk, SLOW)))
        .collect();
    for _ in 0..4 {
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    assert!(
        ask(bulk, GET).starts_with("HTTP/1.1 503"),
        "the flood stops"
    );
    assert!(
        ask(admin, GET).starts_with("HTTP/1.1 200"),
        "the lane answers"
    );

    for _ in 0..4 {
        release.send(()).unwrap();
    }
    for h in held {
        assert!(h.join().unwrap().starts_with("HTTP/1.1 200"));
    }
    assert!(
        ask(bulk, GET).starts_with("HTTP/1.1 200"),
        "slots came back"
    );
    let _ = server.drain(Duration::from_secs(1));
}

/// T-25 on the reactor: one worker holds many requests, parked on deferred
/// replies, and the reserve is a share of that worker's cap.
#[test]
fn test_bulk_flood_critical_lane_still_admitted_on_the_reactor() {
    use serve_net::{InFlightCap, Lane, Profile, ProfileId, ReplyToken, Workers};

    let held: Arc<Mutex<Vec<ReplyToken>>> = Arc::default();
    let parked = Arc::clone(&held);
    let server = Server::builder()
        .listener(ListenerSpec::tcp("bulk", "127.0.0.1:0").profile(ProfileId(0)))
        .listener(ListenerSpec::tcp("admin", "127.0.0.1:0").profile(ProfileId(1)))
        .profile(Profile::new(ProfileId(0)).lane(Lane(4)))
        .profile(Profile::new(ProfileId(1)).lane(Lane(0)))
        .workers(Workers::reactor(1))
        .admit(InFlightCap::new().per_worker(4).reserve(Lane(0), 50))
        .serve(serve_net::handler(move |req, reply| {
            reply.text(200, "ok");
            if req.path() == "/slow" {
                parked.lock().unwrap().push(reply.defer());
            }
        }))
        .unwrap();
    let addrs: Vec<SocketAddr> = server
        .listeners()
        .iter()
        .map(|l| l.local.socket_addr().unwrap())
        .collect();
    let (bulk, admin) = (addrs[0], addrs[1]);

    const SLOW: &[u8] = b"GET /slow HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";
    let waiting: Vec<_> = (0..2)
        .map(|_| std::thread::spawn(move || ask(bulk, SLOW)))
        .collect();
    let deadline = Instant::now() + Duration::from_secs(5);
    while held.lock().unwrap().len() < 2 {
        assert!(Instant::now() < deadline, "the bulk requests never parked");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        ask(bulk, GET).starts_with("HTTP/1.1 503"),
        "the flood stops"
    );
    assert!(
        ask(admin, GET).starts_with("HTTP/1.1 200"),
        "the lane answers"
    );

    for token in held.lock().unwrap().drain(..) {
        token.complete().unwrap();
    }
    for w in waiting {
        assert!(w.join().unwrap().starts_with("HTTP/1.1 200"));
    }
    let _ = server.drain(Duration::from_secs(1));
}

/// T-25 again, for connections: a flood fills every seat it may take; the
/// dedicated listener still gets a worker.
#[test]
fn test_bulk_flood_dedicated_workers_stay_free() {
    use serve_net::{Budget, Limits, Profile, ProfileId};

    let (entered_tx, entered) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let (entered_tx, released) = (Mutex::new(entered_tx), Mutex::new(released));
    let server = Server::builder()
        .listener(ListenerSpec::tcp("bulk", "127.0.0.1:0").profile(ProfileId(0)))
        .listener(ListenerSpec::tcp("admin", "127.0.0.1:0").profile(ProfileId(1)))
        .profile(Profile::new(ProfileId(0)))
        .profile(Profile::new(ProfileId(1)).budget(Budget::new().dedicated_workers(1)))
        .limits(Limits::new().max_connections(3))
        .workers(serve_net::Workers::blocking(8))
        .serve(serve_net::handler(move |req, reply| {
            if req.path() == "/slow" {
                entered_tx.lock().unwrap().send(()).unwrap();
                let _ = released.lock().unwrap().recv();
            }
            reply.text(200, "ok");
        }))
        .unwrap();
    let addrs: Vec<SocketAddr> = server
        .listeners()
        .iter()
        .map(|l| l.local.socket_addr().unwrap())
        .collect();
    let (bulk, admin) = (addrs[0], addrs[1]);

    const SLOW: &[u8] = b"GET /slow HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";
    let held: Vec<_> = (0..2)
        .map(|_| std::thread::spawn(move || ask(bulk, SLOW)))
        .collect();
    for _ in 0..2 {
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    // Two seats of three are the bulk listener's; the third waits for admin.
    assert!(
        ask(bulk, GET).starts_with("HTTP/1.1 503"),
        "the flood stops"
    );
    assert!(ask(admin, GET).starts_with("HTTP/1.1 200"), "admin answers");

    for _ in 0..2 {
        release.send(()).unwrap();
    }
    for h in held {
        assert!(h.join().unwrap().starts_with("HTTP/1.1 200"));
    }
    let _ = server.drain(Duration::from_secs(1));
}

/// A pool with `threads` threads and room for `queue` waiting jobs.
struct Pool(mpsc::SyncSender<OffloadJob>);

impl OffloadPool for Pool {
    fn try_submit(&self, _class: WorkClass, job: OffloadJob) -> Result<(), OffloadJob> {
        self.0.try_send(job).map_err(|e| match e {
            mpsc::TrySendError::Full(job) | mpsc::TrySendError::Disconnected(job) => job,
        })
    }
}

fn pool(threads: usize, queue: usize) -> Offload<Pool> {
    let (jobs, waiting) = mpsc::sync_channel::<OffloadJob>(queue);
    let waiting = Arc::new(Mutex::new(waiting));
    for _ in 0..threads {
        let waiting = Arc::clone(&waiting);
        std::thread::spawn(move || {
            loop {
                let job = waiting.lock().unwrap().recv();
                match job {
                    Ok(job) => job.run(),
                    Err(_) => return,
                }
            }
        });
    }
    Offload::new(Pool(jobs))
}

struct Slow {
    entered: Mutex<mpsc::Sender<std::thread::ThreadId>>,
    release: Mutex<mpsc::Receiver<()>>,
    ends: Mutex<Vec<RequestEnd>>,
}

#[derive(Clone)]
struct SlowService(Arc<Slow>);

impl HttpService for SlowService {
    const WORK: WorkClass = WorkClass::Cpu;

    fn call(&self, req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
        match req.path() {
            "/wait" => {
                let me = std::thread::current().id();
                self.0.entered.lock().unwrap().send(me).unwrap();
                let _ = self.0.release.lock().unwrap().recv();
                reply.text(200, "waited");
                Outcome::Done
            }
            "/boom" => panic!("a service bug"),
            "/close" => {
                reply.text(200, "bye");
                Outcome::Close
            }
            "/later" => {
                let mut token = reply.defer();
                std::thread::spawn(move || {
                    token.reply().text(200, "deferred twice");
                    let _ = token.complete();
                });
                Outcome::Deferred
            }
            "/lied" => Outcome::Deferred,
            _ => {
                let host = req.head().authority().unwrap_or("");
                let body = String::from_utf8_lossy(req.body_bytes()).into_owned();
                let query = req.query().unwrap_or("");
                reply.text(200, &format!("{host} {query} {body}"));
                Outcome::Done
            }
        }
    }

    fn done(&self, _env: &Envelope, finished: &Finished) {
        self.0.ends.lock().unwrap().push(finished.end);
    }
}

fn slow() -> (
    SlowService,
    mpsc::Receiver<std::thread::ThreadId>,
    mpsc::Sender<()>,
) {
    let (entered, in_call) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let service = SlowService(Arc::new(Slow {
        entered: Mutex::new(entered),
        release: Mutex::new(released),
        ends: Mutex::new(Vec::new()),
    }));
    (service, in_call, release)
}

const WAIT: &[u8] = b"GET /wait HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";

/// T-27: past the pool's bound the client gets the busy refusal, not a queue.
#[test]
fn test_offload_full_refused_503() {
    let (service, in_call, release) = slow();
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .dispatch(pool(1, 1))
        .serve(service.clone())
        .unwrap();
    let addr = server.local_addr().unwrap();

    let running = std::thread::spawn(move || ask(addr, WAIT));
    let on_pool = in_call.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_ne!(on_pool, std::thread::current().id());
    // One takes the queue's only place; the other finds the pool full.
    let more: Vec<_> = (0..2)
        .map(|_| std::thread::spawn(move || ask(addr, WAIT)))
        .collect();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !service.0.ends.lock().unwrap().contains(&RequestEnd::Shed) {
        assert!(Instant::now() < deadline, "never refused");
        std::thread::sleep(Duration::from_millis(5));
    }

    for _ in 0..2 {
        release.send(()).unwrap();
    }
    assert!(running.join().unwrap().ends_with("waited"));
    let mut more: Vec<String> = more.into_iter().map(|h| h.join().unwrap()).collect();
    more.sort();
    assert!(more[0].starts_with("HTTP/1.1 200"), "{}", more[0]);
    assert!(more[0].ends_with("waited"));
    assert!(more[1].starts_with("HTTP/1.1 503"), "{}", more[1]);
    assert!(more[1].to_ascii_lowercase().contains("retry-after"));
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_offload_copies_the_request_and_keeps_the_connection() {
    let (service, _in_call, _release) = slow();
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .dispatch(pool(2, 8))
        .serve(service)
        .unwrap();
    let addr = server.local_addr().unwrap();

    // Two requests on one connection: each reply comes back to its place.
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    for n in 0..2 {
        write!(
            s,
            "POST /x?q={n} HTTP/1.1\r\nHost: site.test\r\nContent-Length: 4\r\n\r\nbody"
        )
        .unwrap();
        let mut buf = [0u8; 512];
        let len = s.read(&mut buf).unwrap();
        let got = String::from_utf8_lossy(&buf[..len]);
        assert!(got.starts_with("HTTP/1.1 200"), "{got}");
        assert!(got.ends_with(&format!("site.test q={n} body")), "{got}");
    }
    drop(s);
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_offload_panic_answers_500_and_the_pool_survives() {
    let (service, _in_call, _release) = slow();
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .dispatch(pool(1, 4))
        .serve(service)
        .unwrap();
    let addr = server.local_addr().unwrap();
    let get = |path: &str| {
        ask(
            addr,
            format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
        )
    };
    assert!(get("/boom").starts_with("HTTP/1.1 500"));
    assert!(get("/ok").starts_with("HTTP/1.1 200"), "one thread, alive");
    assert!(get("/lied").starts_with("HTTP/1.1 500"));
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_offload_service_may_close_or_defer() {
    let (service, _in_call, _release) = slow();
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .dispatch(pool(1, 4))
        .serve(service)
        .unwrap();
    let addr = server.local_addr().unwrap();

    // `Outcome::Close` closes a keep-alive connection after the reply.
    let closed = ask(addr, b"GET /close HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(closed.starts_with("HTTP/1.1 200") && closed.ends_with("bye"));

    let later = ask(
        addr,
        b"GET /later HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    );
    assert!(later.ends_with("deferred twice"), "{later}");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_offload_dropped_job_answers_500() {
    struct Drops;
    impl OffloadPool for Drops {
        fn try_submit(&self, class: WorkClass, job: OffloadJob) -> Result<(), OffloadJob> {
            assert_eq!(job.class(), class);
            drop(job);
            Ok(())
        }
    }
    let (service, _in_call, _release) = slow();
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .dispatch(Offload::new(Drops))
        .serve(service)
        .unwrap();
    let got = ask(server.local_addr().unwrap(), GET);
    assert!(got.starts_with("HTTP/1.1 500"), "{got}");
    let _ = server.drain(Duration::from_secs(2));
}

#[test]
fn test_offload_quick_service_stays_on_the_worker() {
    struct Refuses;
    impl OffloadPool for Refuses {
        fn try_submit(&self, _class: WorkClass, _job: OffloadJob) -> Result<(), OffloadJob> {
            panic!("a Quick service never reaches the pool");
        }
    }
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .dispatch(Offload::new(Refuses))
        .serve(serve_net::handler(|_req, reply| {
            reply.text(200, "ok");
        }))
        .unwrap();
    assert!(ask(server.local_addr().unwrap(), GET).ends_with("ok"));
    let _ = server.drain(Duration::from_secs(2));
}

/// T-26: `max_deferred_per_worker` caps the requests parked on a worker.
/// It only sized net-io's completion queue: every deferral parked. Past the
/// cap a deferral is answered with the busy refusal and its token can only
/// expire; a place comes back when a parked reply leaves (D-111).
#[test]
fn test_t26_deferrals_past_the_per_worker_cap_are_refused() {
    struct Defer(Arc<Mutex<Vec<serve_net::ReplyToken>>>);
    impl HttpService for Defer {
        fn call(&self, _req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
            let mut token = reply.defer();
            token.reply().text(200, "late");
            self.0.lock().unwrap().push(token);
            Outcome::Deferred
        }
    }
    let tokens = Arc::new(Mutex::new(Vec::new()));
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .workers(serve_net::Workers::reactor(1))
        .limits(serve_net::Limits::new().max_deferred_per_worker(2))
        .serve(Defer(Arc::clone(&tokens)))
        .unwrap();
    let addr = server.local_addr().unwrap();
    let ask = || {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"GET / HTTP/1.1\r\nHost: a\r\nConnection: close\r\n\r\n")
            .unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s
    };
    let read_all = |mut s: TcpStream| {
        let mut got = String::new();
        let _ = s.read_to_string(&mut got);
        got
    };
    let parked: Vec<TcpStream> = (0..2).map(|_| ask()).collect();
    let wait = Instant::now();
    while tokens.lock().unwrap().len() < 2 && wait.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(5));
    }
    // A third deferral on the same worker: refused at once.
    let third = read_all(ask());
    assert!(third.starts_with("HTTP/1.1 503"), "{third}");
    let refused = tokens.lock().unwrap().pop().expect("the third token");
    assert!(refused.is_cancelled());
    assert!(matches!(
        refused.complete(),
        Err(serve_net::CompleteError::Expired(_))
    ));
    // The two parked replies complete; their places come back.
    for t in tokens.lock().unwrap().drain(..) {
        assert!(t.complete().is_ok());
    }
    for s in parked {
        let got = read_all(s);
        assert!(
            got.starts_with("HTTP/1.1 200") && got.ends_with("late"),
            "{got}"
        );
    }
    let s = ask();
    let wait = Instant::now();
    while tokens.lock().unwrap().is_empty() && wait.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(5));
    }
    for t in tokens.lock().unwrap().drain(..) {
        assert!(t.complete().is_ok());
    }
    let got = read_all(s);
    assert!(got.starts_with("HTTP/1.1 200"), "{got}");
    let _ = server.drain(Duration::from_secs(2));
}

/// A worker that never comes back cannot hang `drain`: it returns about 5 s
/// past its deadline with `stuck` set (D-111).
#[test]
fn test_drain_returns_when_a_worker_is_stuck() {
    let (release, hold) = mpsc::channel::<()>();
    let hold = Mutex::new(hold);
    let (entered, inside) = mpsc::channel::<()>();
    let entered = Mutex::new(entered);
    let server = Server::builder()
        .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
        .workers(serve_net::Workers::reactor(1))
        .serve(serve_net::handler(move |_req, reply| {
            let _ = entered.lock().unwrap().send(());
            // Blocks until the test lets it go, after the drain returned.
            let _ = hold.lock().unwrap().recv();
            reply.text(200, "late");
        }))
        .unwrap();
    let addr = server.local_addr().unwrap();
    let mut c = TcpStream::connect(addr).unwrap();
    c.write_all(b"GET / HTTP/1.1\r\nHost: a\r\n\r\n").unwrap();
    inside.recv_timeout(Duration::from_secs(10)).unwrap();
    let started = Instant::now();
    let report = server.drain(Duration::from_millis(200));
    let took = started.elapsed();
    assert!(report.stuck, "{report:?}");
    assert!(took < Duration::from_secs(8), "drain took {took:?}");
    let _ = release.send(());
}
