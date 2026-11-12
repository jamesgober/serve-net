//! The connection machine as a net-io session.
//!
//! A thin translation: net-io's worker calls the session, the session calls
//! [`Conn`] with an [`Io`] over net-io's `SessionContext`, and maps the
//! returned step back. Soft connection caps are taken in the factory, so the
//! busy refusal is written by the session before anything is read (D-013).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use net_io::{
    CloseReason, Completer, ConnectionInfo, Input, PostError, RefusalCause, SessionContext,
    TimeoutKind,
};

use super::{Conn, Io, Phase, Shared, Step, Timeout};
use crate::client::{ClientInfo, IngressKind, Scheme};
use crate::config::{EngineChoice, ProxyKind};
use crate::engine::{Family, Service};
use crate::error::EndReason;
use crate::hooks::{Hooks, JobService, Observer};
use crate::net::{ConnectionId, WorkerId};
use crate::reply::{PostFail, Waker};
use crate::stats::Count;

/// A worker's completion queue as the machine's waker.
struct CompleterWaker(Completer);

impl Waker for CompleterWaker {
    fn post(&self, connection: ConnectionId, token: u32, value: u64) -> Result<(), PostFail> {
        self.0.post(connection, token, value).map_err(|e| match e {
            PostError::Full => PostFail::Full,
            _ => PostFail::Stopped,
        })
    }
}

/// One soft cap: a limit and the connections holding a seat under it.
#[derive(Debug)]
pub(crate) struct Cap {
    limit: AtomicU32,
    open: AtomicU32,
}

impl Cap {
    pub(crate) fn new(limit: Option<u32>) -> Arc<Cap> {
        Arc::new(Cap {
            limit: AtomicU32::new(limit.unwrap_or(u32::MAX)),
            open: AtomicU32::new(0),
        })
    }

    /// A new limit; connections over it stay, new ones are refused.
    pub(crate) fn set_limit(&self, limit: Option<u32>) {
        self.limit
            .store(limit.unwrap_or(u32::MAX), Ordering::Release);
    }

    /// Takes a seat; `true` when the cap is now exceeded.
    fn take(&self) -> bool {
        self.open.fetch_add(1, Ordering::AcqRel) >= self.limit.load(Ordering::Acquire)
    }

    /// Takes a seat only if one is free.
    fn try_take(&self) -> bool {
        let limit = self.limit.load(Ordering::Acquire);
        let mut open = self.open.load(Ordering::Acquire);
        while open < limit {
            match self.open.compare_exchange_weak(
                open,
                open + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => open = actual,
            }
        }
        false
    }

    fn give_back(&self) {
        self.open.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The runtime state every session of one server shares, independent of the
/// service and hook types.
pub(crate) struct Runtime {
    pub(crate) global: Arc<Cap>,
    /// One waker per worker, made on first use (no allocation per connection).
    wakers: Box<[OnceLock<Arc<dyn Waker>>]>,
}

impl Runtime {
    pub(crate) fn new(global: Option<u32>, workers: u16) -> Runtime {
        Runtime {
            global: Cap::new(global),
            wakers: (0..workers.max(1)).map(|_| OnceLock::new()).collect(),
        }
    }
}

/// A connection's seat under the global, listener, and profile caps; given
/// back on drop, so a panicking session cannot leak it.
struct Seat {
    caps: [Option<Arc<Cap>>; 3],
}

impl Seat {
    fn take(
        global: &Arc<Cap>,
        dedicated: Option<&Arc<Cap>>,
        listener: &Arc<Cap>,
        profile: Option<&Arc<Cap>>,
    ) -> (Seat, bool) {
        // A profile's own seats first; they do not count against the others.
        let mut over = false;
        let pool = match dedicated {
            Some(own) if own.try_take() => own,
            _ => {
                over = global.take();
                global
            }
        };
        over |= listener.take();
        if let Some(c) = profile {
            over |= c.take();
        }
        let seat = Seat {
            caps: [
                Some(Arc::clone(pool)),
                Some(Arc::clone(listener)),
                profile.cloned(),
            ],
        };
        (seat, over)
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        for c in self.caps.iter().flatten() {
            c.give_back();
        }
    }
}

/// [`Io`] over net-io's session context.
struct NetIo<'a, 'b> {
    cx: &'a mut SessionContext<'b>,
    rt: &'a Runtime,
}

impl Io for NetIo<'_, '_> {
    fn now(&self) -> Instant {
        self.cx.now()
    }
    fn set_exchange_deadline(&mut self, at: Option<Instant>) {
        self.cx.set_exchange_deadline(at);
    }
    fn set_phase(&mut self, phase: Phase) {
        self.cx.set_phase(match phase {
            Phase::Idle => net_io::Phase::Idle,
            Phase::Receiving => net_io::Phase::Receiving,
            Phase::Sending => net_io::Phase::Sending,
        });
    }
    fn set_idle_timeout(&mut self, timeout: Option<Duration>) {
        self.cx.set_idle_timeout(timeout);
    }
    fn begin_reply(&mut self) {
        self.cx.output().begin_response();
    }
    fn push(&mut self, bytes: &[u8]) -> usize {
        self.cx.output().push_partial(bytes)
    }
    fn draining(&self) -> bool {
        self.cx.draining()
    }
    fn pressure(&self) -> f32 {
        self.cx.pressure()
    }
    fn worker(&self) -> WorkerId {
        self.cx.worker()
    }
    fn waker(&self) -> Arc<dyn Waker> {
        let made = || Arc::new(CompleterWaker(self.cx.completer())) as Arc<dyn Waker>;
        match self.rt.wakers.get(usize::from(self.cx.worker().get())) {
            Some(cell) => Arc::clone(cell.get_or_init(made)),
            None => made(),
        }
    }
}

fn step(s: Step) -> net_io::Step {
    match s {
        Step::Read => net_io::Step::Read,
        Step::Write => net_io::Step::Write,
        Step::Park { until } => net_io::Step::Park { until: Some(until) },
        Step::Close => net_io::Step::Close,
        Step::Abort => net_io::Step::Abort,
    }
}

/// The machine counts a reply as handed over once `push` took it, but net-io
/// sends queued output only on `Write` (and `Close`). So a `Read` or `Park`
/// with output queued becomes `Write`, and the step resumes after the flush
/// (D-045).
fn handed(s: Step, cx: &mut SessionContext<'_>, resume: &mut Option<Step>) -> net_io::Step {
    match s {
        Step::Read | Step::Park { .. } if !cx.output().is_empty() => {
            *resume = Some(s);
            net_io::Step::Write
        }
        other => step(other),
    }
}

/// One connection under net-io.
pub(crate) struct ServeSession<F: Family, S: Service<F>, H: Hooks> {
    conn: Conn<F, S, H>,
    rt: Arc<Runtime>,
    worker: WorkerId,
    over_cap: bool,
    /// The step a flush forced by [`handed`] interrupted.
    resume: Option<Step>,
    _seat: Seat,
}

impl<F: Family, S: Service<F> + 'static, H: Hooks> net_io::Session for ServeSession<F, S, H> {
    fn on_open(&mut self, cx: &mut SessionContext<'_>) -> net_io::Step {
        let mut io = NetIo {
            cx: &mut *cx,
            rt: &self.rt,
        };
        let s = self.conn.on_open(&mut io, self.over_cap);
        self.conn.count_opened(self.worker);
        handed(s, cx, &mut self.resume)
    }

    fn on_data(&mut self, cx: &mut SessionContext<'_>, input: &mut Input) -> net_io::Step {
        let mut io = NetIo {
            cx: &mut *cx,
            rt: &self.rt,
        };
        let (n, s) = self.conn.on_data(&mut io, input.bytes());
        input.consume(n);
        handed(s, cx, &mut self.resume)
    }

    fn on_writable(&mut self, cx: &mut SessionContext<'_>) -> net_io::Step {
        // A parked reply keeps waiting, a refusal closes; any other interrupted
        // step continues with what the machine does after a write (pipelined
        // input already in the engine).
        if let Some(s @ (Step::Park { .. } | Step::Close)) = self.resume.take() {
            return step(s);
        }
        let mut io = NetIo {
            cx: &mut *cx,
            rt: &self.rt,
        };
        let s = self.conn.on_writable(&mut io);
        handed(s, cx, &mut self.resume)
    }

    fn on_wake(&mut self, cx: &mut SessionContext<'_>, token: u32, value: u64) -> net_io::Step {
        let mut io = NetIo {
            cx: &mut *cx,
            rt: &self.rt,
        };
        let s = self.conn.on_wake(&mut io, token, value);
        handed(s, cx, &mut self.resume)
    }

    fn on_timeout(&mut self, cx: &mut SessionContext<'_>, which: TimeoutKind) -> net_io::Step {
        let which = match which {
            TimeoutKind::Exchange => Timeout::Exchange,
            TimeoutKind::Park => Timeout::Park,
            TimeoutKind::Handshake => Timeout::Transport(CloseReason::HandshakeTimeout),
            TimeoutKind::Idle => Timeout::Transport(CloseReason::Idle),
            TimeoutKind::Write => Timeout::Transport(CloseReason::WriteTimeout),
            TimeoutKind::Drain => Timeout::Transport(CloseReason::DrainDeadline),
            _ => Timeout::Transport(CloseReason::ReadTimeout),
        };
        let mut io = NetIo {
            cx: &mut *cx,
            rt: &self.rt,
        };
        let s = self.conn.on_timeout(&mut io, which);
        // net-io turns a `Close` from here into an immediate close, so a
        // refusal the machine just wrote goes out first (D-064).
        if s == Step::Close && !cx.output().is_empty() {
            self.resume = Some(Step::Close);
            return net_io::Step::Write;
        }
        handed(s, cx, &mut self.resume)
    }

    fn on_peer_closed(&mut self, cx: &mut SessionContext<'_>) -> net_io::Step {
        let mut io = NetIo {
            cx: &mut *cx,
            rt: &self.rt,
        };
        let s = self.conn.on_peer_closed(&mut io);
        handed(s, cx, &mut self.resume)
    }

    fn on_denied(&mut self, code: u16, output: &mut net_io::Output) {
        let refusal = self.conn.on_denied(code, self.worker);
        // net-io sends this once, bounded by its refusal limit; what does not
        // fit is dropped, and the connection closes either way.
        let _ = output.push_partial(refusal);
    }

    fn on_close(&mut self, _info: &ConnectionInfo, reason: CloseReason) {
        self.conn.on_close(reason);
    }
}

/// Makes one [`ServeSession`] per connection net-io admits.
pub(crate) struct Factory<F: Family, S: Service<F>, H: Hooks> {
    pub(crate) shared: Arc<Shared<F, S, H>>,
    pub(crate) rt: Arc<Runtime>,
}

impl<F: Family, S: Service<F> + 'static, H: Hooks> net_io::SessionFactory for Factory<F, S, H> {
    type Session = ServeSession<F, S, H>;

    fn open(&self, info: &ConnectionInfo) -> Result<Self::Session, RefusalCause> {
        let session = self.session(info);
        if session.is_err() {
            let worker = info.worker.get();
            self.shared.counters.count(worker, Count::Opened);
        }
        session
    }
}

impl<F: Family, S: Service<F> + 'static, H: Hooks> Factory<F, S, H> {
    fn session(&self, info: &ConnectionInfo) -> Result<ServeSession<F, S, H>, RefusalCause> {
        let listener = info.listener.map_or(0, |l| usize::from(l.get()));
        let tables = self.shared.tables.load();
        let Some(runtime) = tables.listeners.get(listener).and_then(Option::as_ref) else {
            return Err(RefusalCause::Session);
        };
        let mut client = match &runtime.unix {
            Some(unix) => {
                // Checked before any byte is read; a peer that fails is closed.
                if !unix.admits(info.peer_credentials) {
                    return Err(RefusalCause::Credentials);
                }
                ClientInfo::unix(info.peer_credentials)
            }
            None => match info.client_addr() {
                Some(peer) => ClientInfo::direct(peer, info.tls.is_some()),
                None => return Err(RefusalCause::Session),
            },
        };
        let proxy_protocol = runtime
            .proxy
            .as_ref()
            .filter(|p| matches!(p.kind, ProxyKind::ProxyProtocol(_)));
        if let Some(p) = proxy_protocol {
            // net-io read the header from a trusted peer before calling this.
            client = match info.real_peer {
                Some(real) => {
                    client.behind_proxy(Some(real.ip()), real.port(), 1, IngressKind::ProxyProtocol)
                }
                // LOCAL: the proxy itself, for example its health check.
                None => client.with_proxy_local(true),
            };
            let tls = p.trust_scheme
                && info
                    .proxy
                    .as_ref()
                    // PP2_CLIENT_SSL: a proxy may send the SSL TLV on every
                    // connection and set the bit only for TLS clients.
                    .is_some_and(|f| f.ssl_client().is_some_and(|c| c & 0x01 != 0));
            client = client.with_scheme(if tls {
                Scheme::Https
            } else {
                runtime.declared_scheme
            });
        }
        // net-io opens the session after the TLS handshake, so the ALPN
        // result is known: a protocol with no engine closes the connection
        // before any byte reaches one.
        let engine = tables
            .profiles
            .compiled(runtime.profile)
            .map_or(EngineChoice::Http1, |c| c.profile.engine);
        let alpn = info.tls.as_ref().and_then(|t| t.alpn());
        if !engine.admits(alpn) {
            self.shared
                .observer
                .connection_ended(info.id, EndReason::NoEngine, 0);
            return Err(RefusalCause::Session);
        }
        let profile = usize::from(runtime.profile.0);
        let profile_cap = tables.profile_caps.get(profile);
        let dedicated = tables.dedicated.get(profile).and_then(Option::as_ref);
        let (seat, over_cap) = Seat::take(&self.rt.global, dedicated, &runtime.cap, profile_cap);
        if over_cap {
            self.shared
                .observer
                .capacity_refused(&runtime.name, runtime.profile);
        }
        let mut conn = Conn::new(
            Arc::clone(&self.shared),
            listener,
            info.id,
            client,
            info.accepted_at,
            info.worker,
        );
        conn.set_gate_tag(info.gate);
        if let Some(tls) = &info.tls {
            conn.set_tls(Some(Arc::clone(tls)));
        }
        if proxy_protocol.is_some() {
            conn.set_proxy_facts(info.proxy.clone());
        }
        conn.offload_through(Arc::clone(&self.shared) as Arc<dyn JobService<F>>);
        Ok(ServeSession {
            conn,
            rt: Arc::clone(&self.rt),
            worker: info.worker,
            over_cap,
            resume: None,
            _seat: seat,
        })
    }
}
