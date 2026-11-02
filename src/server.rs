//! Building a server: configuration, hooks, and validation.

use std::sync::Arc;
use std::time::Duration;

use crate::client::IngressKind;
use crate::config::ProxyKind;
use crate::config::validate::{self, Draft};
use crate::config::{
    Ingress, KeepAlive, Limits, ListenerSource, ListenerSpec, Profile, ProfileId, ProfileTable,
    ProxySource, Workers,
};
use crate::conn::session::{Cap, Factory, Runtime};
use crate::conn::{ListenerRuntime, Shared, Tables, UnixRuntime};
use crate::engine::{Family, Service};
use crate::error::{ConfigErrors, ConfigWarning, ServeError};
use crate::health::{HealthState, Tap};
use crate::hooks::{Admit, Checkpoint, DefaultHooks, Dispatch, HookSet, Hooks, Observer};
use crate::net::{ControlLine, EventSink, Gate};
use crate::notify::{Notifier, Notify, ServerEvents};
use crate::reload::{Reloader, ServerConfig, transport_key};
#[cfg(feature = "http1")]
use crate::running::Running;
use crate::running::{Bound, DrainSwitch, ListenerMeta, Merged, Parts};
use crate::snapshot::Snapshot;
use crate::stats::Counters;

/// The entry point for Tier 2.
///
/// ```
/// use serve_net::{ListenerSpec, Server};
/// let built = Server::builder().listener(ListenerSpec::tcp("web", "127.0.0.1:0")).build()?;
/// assert!(built.warnings().is_empty());
/// # Ok::<(), serve_net::ConfigErrors>(())
/// ```
#[derive(Debug)]
pub struct Server;

impl Server {
    /// A builder with no listeners, no profiles (profile 0 is created with
    /// defaults when none is added), default limits, workers, and hooks.
    pub fn builder() -> ServerBuilder {
        ServerBuilder {
            config: Config {
                listeners: Vec::new(),
                profiles: Vec::new(),
                limits: Limits::new(),
                keep_alive: KeepAlive::new(),
                workers: Workers::auto(),
                drain_deadline: Duration::from_secs(30),
                accept_failure_window: Duration::from_secs(30),
                has_control: false,
                has_tls: false,
                catch_panics: false,
                reserved_percent: 0,
                notify: Arc::new(crate::NoNotify),
                transport: None,
                clock: None,
            },
            hooks: DefaultHooks::default(),
        }
    }
}

/// Everything a builder holds except the hooks.
struct Config {
    listeners: Vec<ListenerSpec>,
    profiles: Vec<Profile>,
    limits: Limits,
    keep_alive: KeepAlive,
    workers: Workers,
    drain_deadline: Duration,
    accept_failure_window: Duration,
    has_control: bool,
    has_tls: bool,
    catch_panics: bool,
    reserved_percent: u32,
    notify: Arc<dyn Notify>,
    transport: Option<net_io::Limits>,
    clock: Option<Arc<dyn net_io::Clock>>,
}

/// Configuration and hooks; nothing is checked until [`build`](Self::build).
#[must_use]
pub struct ServerBuilder<H = DefaultHooks> {
    config: Config,
    hooks: H,
}

impl<H> ServerBuilder<H> {
    /// Adds a listener (at most 64).
    pub fn listener(mut self, spec: ListenerSpec) -> Self {
        self.config.listeners.push(spec);
        self
    }

    /// Adds a profile (at most 255).
    pub fn profile(mut self, profile: Profile) -> Self {
        self.config.profiles.push(profile);
        self
    }

    /// Server-wide limits; profiles may override them.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.config.limits = limits;
        self
    }

    /// Server-wide keep-alive; profiles may override it.
    pub fn keep_alive(mut self, policy: KeepAlive) -> Self {
        self.config.keep_alive = policy;
        self
    }

    /// The driver and worker count.
    pub fn workers(mut self, workers: Workers) -> Self {
        self.config.workers = workers;
        self
    }

    /// The deadline `wait` uses when a drain is requested without one (default 30 s).
    pub fn drain_deadline(mut self, deadline: Duration) -> Self {
        self.config.drain_deadline = deadline;
        self
    }

    /// Catches a panicking service: the 500 refusal, the connection closes,
    /// `done` runs with `RequestEnd::ServicePanic`. Effective in
    /// `panic = "unwind"` builds; `build` warns in an abort build.
    pub fn catch_service_panics(mut self, on: bool) -> Self {
        self.config.catch_panics = on;
        self
    }

    /// The base net-io limits for every listener; what serve-net maps from
    /// its own limits is set on top.
    pub fn transport_limits(mut self, limits: net_io::Limits) -> Self {
        self.config.transport = Some(limits);
        self
    }

    /// The clock net-io's deadlines run on; serve-net reads time from net-io,
    /// so a `net_io::ManualClock` makes every deadline deterministic in tests.
    pub fn clock<K: net_io::Clock>(mut self, clock: Arc<K>) -> Self {
        self.config.clock = Some(clock);
        self
    }

    /// Everything [`ServerConfig`] holds, replacing what was set so far: the
    /// same value serves at start-up and at `Running::reload`.
    pub fn config(mut self, config: ServerConfig) -> Self {
        self.config.listeners = config.listeners;
        self.config.profiles = config.profiles;
        self.config.limits = config.limits;
        self.config.keep_alive = config.keep_alive;
        self.config.workers = config.workers;
        self.config.drain_deadline = config.drain_deadline;
        self
    }

    /// Readiness for a service manager or a supervisor (default `NoNotify`).
    pub fn notify<N: Notify>(mut self, notify: N) -> Self {
        self.config.notify = Arc::new(notify);
        self
    }

    /// How long a listener may report accept errors before health is
    /// `Degraded` (default 30 s).
    pub fn accept_failure_window(mut self, window: Duration) -> Self {
        self.config.accept_failure_window = window;
        self
    }
}

// Each setter changes one slot of the hook set, so every hook is a static
// call that inlines (docs/API.md, "ServerBuilder: hooks").
#[allow(clippy::type_complexity)]
impl<A, D, K, O, G, E, C, T> ServerBuilder<HookSet<A, D, K, O, G, E, C, T>> {
    /// Replaces admission.
    pub fn admit<A2: Admit>(
        mut self,
        admit: A2,
    ) -> ServerBuilder<HookSet<A2, D, K, O, G, E, C, T>> {
        self.config.reserved_percent = admit.reserved_percent();
        let HookSet {
            dispatch,
            checkpoint,
            observer,
            gate,
            events,
            control,
            tls,
            ..
        } = self.hooks;
        ServerBuilder {
            config: self.config,
            hooks: HookSet {
                admit,
                dispatch,
                checkpoint,
                observer,
                gate,
                events,
                control,
                tls,
            },
        }
    }

    /// Replaces dispatch.
    pub fn dispatch<D2: Dispatch>(
        self,
        dispatch: D2,
    ) -> ServerBuilder<HookSet<A, D2, K, O, G, E, C, T>> {
        let HookSet {
            admit,
            checkpoint,
            observer,
            gate,
            events,
            control,
            tls,
            ..
        } = self.hooks;
        ServerBuilder {
            config: self.config,
            hooks: HookSet {
                admit,
                dispatch,
                checkpoint,
                observer,
                gate,
                events,
                control,
                tls,
            },
        }
    }

    /// Replaces the revocation checkpoint.
    pub fn checkpoint<K2: Checkpoint>(
        self,
        checkpoint: K2,
    ) -> ServerBuilder<HookSet<A, D, K2, O, G, E, C, T>> {
        let HookSet {
            admit,
            dispatch,
            observer,
            gate,
            events,
            control,
            tls,
            ..
        } = self.hooks;
        ServerBuilder {
            config: self.config,
            hooks: HookSet {
                admit,
                dispatch,
                checkpoint,
                observer,
                gate,
                events,
                control,
                tls,
            },
        }
    }

    /// Replaces the observer.
    pub fn observer<O2: Observer>(
        self,
        observer: O2,
    ) -> ServerBuilder<HookSet<A, D, K, O2, G, E, C, T>> {
        let HookSet {
            admit,
            dispatch,
            checkpoint,
            gate,
            events,
            control,
            tls,
            ..
        } = self.hooks;
        ServerBuilder {
            config: self.config,
            hooks: HookSet {
                admit,
                dispatch,
                checkpoint,
                observer,
                gate,
                events,
                control,
                tls,
            },
        }
    }

    /// The connection gate net-io consults at accept, before every
    /// write, and at close (default `AllowAll`).
    pub fn gate<G2: Gate>(self, gate: G2) -> ServerBuilder<HookSet<A, D, K, O, G2, E, C, T>> {
        let HookSet {
            admit,
            dispatch,
            checkpoint,
            observer,
            events,
            control,
            tls,
            ..
        } = self.hooks;
        ServerBuilder {
            config: self.config,
            hooks: HookSet {
                admit,
                dispatch,
                checkpoint,
                observer,
                gate,
                events,
                control,
                tls,
            },
        }
    }

    /// The sink for net-io's transport events (default `NoopSink`);
    /// serve-net also reads them for `health`.
    pub fn events<E2: EventSink>(
        self,
        events: E2,
    ) -> ServerBuilder<HookSet<A, D, K, O, G, E2, C, T>> {
        let HookSet {
            admit,
            dispatch,
            checkpoint,
            observer,
            gate,
            control,
            tls,
            ..
        } = self.hooks;
        ServerBuilder {
            config: self.config,
            hooks: HookSet {
                admit,
                dispatch,
                checkpoint,
                observer,
                gate,
                events,
                control,
                tls,
            },
        }
    }

    /// The TLS acceptor for `Ingress::DirectTls` listeners (tls-net, or any
    /// `net_io::TlsAcceptor`).
    pub fn tls<T2: net_io::TlsAcceptor>(
        mut self,
        acceptor: T2,
    ) -> ServerBuilder<HookSet<A, D, K, O, G, E, C, T2>> {
        self.config.has_tls = true;
        let HookSet {
            admit,
            dispatch,
            checkpoint,
            observer,
            gate,
            events,
            control,
            ..
        } = self.hooks;
        ServerBuilder {
            config: self.config,
            hooks: HookSet {
                admit,
                dispatch,
                checkpoint,
                observer,
                gate,
                events,
                control,
                tls: acceptor,
            },
        }
    }

    /// The process-wide control line, shared with net-io.
    pub fn control<C2: ControlLine>(
        mut self,
        line: Arc<C2>,
    ) -> ServerBuilder<HookSet<A, D, K, O, G, E, C2, T>> {
        self.config.has_control = true;
        let HookSet {
            admit,
            dispatch,
            checkpoint,
            observer,
            gate,
            events,
            tls,
            ..
        } = self.hooks;
        ServerBuilder {
            config: self.config,
            hooks: HookSet {
                admit,
                dispatch,
                checkpoint,
                observer,
                gate,
                events,
                control: line,
                tls,
            },
        }
    }
}

impl<H: Hooks> ServerBuilder<H> {
    /// Runs every validation rule and collects every failure; compiles the
    /// profile table. Nothing is bound and no thread is started.
    ///
    /// # Errors
    ///
    /// [`ConfigErrors`], never empty: every rule that failed.
    pub fn build(self) -> Result<Built<H>, ConfigErrors> {
        let c = self.config;
        let findings = validate::check(&Draft {
            listeners: &c.listeners,
            profiles: &c.profiles,
            limits: &c.limits,
            keep_alive: &c.keep_alive,
            workers: &c.workers,
            drain_deadline: c.drain_deadline,
            has_control: c.has_control,
            has_tls: c.has_tls,
            catch_panics: c.catch_panics,
            reserved_percent: c.reserved_percent,
        });
        if let Some(errors) = ConfigErrors::from_vec(findings.errors) {
            return Err(errors);
        }
        let mut profiles = c.profiles;
        if profiles.is_empty() {
            profiles.push(Profile::new(ProfileId(0)));
        }
        let table = ProfileTable::compile(&profiles, c.limits, c.keep_alive);
        Ok(Built {
            listeners: c.listeners,
            profiles: table,
            limits: c.limits,
            keep_alive: c.keep_alive,
            workers: c.workers,
            drain_deadline: c.drain_deadline,
            accept_failure_window: c.accept_failure_window,
            notify: c.notify,
            transport: c.transport,
            clock: c.clock,
            has_control: c.has_control,
            has_tls: c.has_tls,
            catch_panics: c.catch_panics,
            warnings: findings.warnings,
            hooks: self.hooks,
        })
    }
}

impl<A, D, K, O, G, E, C, T> ServerBuilder<HookSet<A, D, K, O, G, E, C, T>>
where
    A: Admit,
    D: Dispatch,
    K: Checkpoint,
    O: Observer,
    G: Gate,
    E: EventSink,
    C: ControlLine,
    T: net_io::TlsAcceptor,
{
    /// `build`, `bind`, and `open` in one call.
    ///
    /// # Errors
    ///
    /// [`ServeError::Config`] from validation, then as [`Built::bind`] and
    /// [`Bound::open`].
    #[cfg(feature = "http1")]
    pub fn serve<S: crate::HttpService + 'static>(self, service: S) -> Result<Running, ServeError> {
        self.build()?.bind(service)?.open()
    }
}

impl<H> core::fmt::Debug for ServerBuilder<H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ServerBuilder")
            .field("listeners", &self.config.listeners)
            .field("profiles", &self.config.profiles.len())
            .finish()
    }
}

/// A validated configuration: nothing bound, no thread started.
#[must_use]
#[allow(dead_code)] // `keep_alive` is read by reload (0.4)
pub struct Built<H: Hooks = DefaultHooks> {
    pub(crate) listeners: Vec<ListenerSpec>,
    pub(crate) profiles: ProfileTable,
    pub(crate) limits: Limits,
    pub(crate) keep_alive: KeepAlive,
    pub(crate) workers: Workers,
    pub(crate) drain_deadline: Duration,
    pub(crate) accept_failure_window: Duration,
    pub(crate) notify: Arc<dyn Notify>,
    pub(crate) transport: Option<net_io::Limits>,
    pub(crate) clock: Option<Arc<dyn net_io::Clock>>,
    pub(crate) has_control: bool,
    pub(crate) has_tls: bool,
    pub(crate) catch_panics: bool,
    pub(crate) warnings: Vec<ConfigWarning>,
    pub(crate) hooks: H,
}

impl<H: Hooks> Built<H> {
    /// Values that are allowed but questionable.
    #[must_use]
    pub fn warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }

    /// The compiled profile table.
    #[must_use]
    pub fn profiles(&self) -> &ProfileTable {
        &self.profiles
    }
}

impl<A, D, K, O, G, E, C, T> Built<HookSet<A, D, K, O, G, E, C, T>>
where
    A: Admit,
    D: Dispatch,
    K: Checkpoint,
    O: Observer,
    G: Gate,
    E: EventSink,
    C: ControlLine,
    T: net_io::TlsAcceptor,
{
    /// Builds net-io's server for `service`: every listener is bound and the
    /// workers start with the door closed.
    ///
    /// # Errors
    ///
    /// [`ServeError::Bind`] naming the listener, [`ServeError::Start`].
    #[cfg(feature = "http1")]
    pub fn bind<S: crate::HttpService + 'static>(self, service: S) -> Result<Bound, ServeError> {
        self.bind_family(crate::HttpFamily, service)
    }

    /// As [`bind`](Self::bind), for a custom protocol family.
    ///
    /// # Errors
    ///
    /// As `bind`.
    pub fn bind_family<F: Family, S: Service<F> + 'static>(
        self,
        family: F,
        service: S,
    ) -> Result<Bound, ServeError> {
        let Built {
            mut listeners,
            profiles,
            limits,
            workers,
            drain_deadline,
            accept_failure_window,
            notify,
            transport,
            clock,
            has_control,
            has_tls,
            catch_panics,
            hooks,
            ..
        } = self;
        let (soft, hard, threads) = connection_caps(&limits, &workers);

        let mut admit = hooks.admit;
        admit.driver(workers.resolved(), soft);
        admit.prepare(threads, &profiles);
        let counters = Arc::new(Counters::new(threads));
        let runtime_listeners = listeners
            .iter()
            .map(|l| listener_runtime(l, Cap::new(l.max_connections)))
            .collect::<Vec<_>>();
        // Filled after bind, once the socket files exist.
        let same_users: Vec<_> = runtime_listeners
            .iter()
            .map(|l| l.unix.as_ref().map(|u| Arc::clone(&u.same_user)))
            .collect();
        let profile_caps = profiles
            .iter()
            .map(|p| Cap::new(p.budget.max_connections))
            .collect();
        let dedicated = profiles
            .iter()
            .map(|p| p.budget.dedicated.map(|n| Cap::new(Some(u32::from(n)))))
            .collect();
        let rt = Arc::new(Runtime::new(Some(shared_seats(soft, &profiles)), threads));

        let global = transport
            .as_ref()
            .map_or_else(net_io::Limits::builder, net_io::Limits::to_builder)
            .max_connections(hard)
            .drain_timeout(drain_deadline)
            .build()
            .map_err(ServeError::Start)?;
        let merged = Arc::new(Merged::new(Arc::clone(&hooks.control)));
        let mut net = net_io::ServerBuilder::new()
            .driver(workers.kind())
            .workers(threads)
            .limits(global)
            .control(Arc::clone(&merged));
        if let Some(clock) = clock {
            net = net.clock(Arc::new(SharedClock(clock)));
        }
        // systemd's sockets, matched to inherited listeners by name or position.
        let mut inherited = if listeners
            .iter()
            .any(|l| matches!(l.source, ListenerSource::Inherited { .. }))
        {
            let passed = net_io::systemd().map_err(|_| ServeError::Inherited {
                listener: first_inherited(&listeners),
                reason: "the systemd environment (LISTEN_FDS, LISTEN_FDNAMES) is malformed, or a passed descriptor is not a listening socket",
            })?;
            match_inherited(&listeners, passed)
        } else {
            Vec::new()
        };
        for (index, spec) in listeners.iter_mut().enumerate() {
            let adopted = match &spec.source {
                ListenerSource::Inherited { fallback, .. } => {
                    match inherited.get_mut(index).and_then(Option::take) {
                        Some(handle) => Some(handle),
                        None if fallback.is_some() => None,
                        None => {
                            return Err(ServeError::Inherited {
                                listener: spec.name.clone(),
                                reason: "no socket was passed for this listener and no fallback_bind is set; check the unit's FileDescriptorName=",
                            });
                        }
                    }
                }
                ListenerSource::HandedOff => spec.handle.take(),
                _ => None,
            };
            if let Some(handle) = &adopted {
                let unix = handle.kind() == net_io::Transport::Unix;
                if unix != spec.ingress.is_unix() {
                    let reason = if unix {
                        "the socket is a Unix socket; use Ingress::Unix"
                    } else {
                        "the socket is a TCP socket; Ingress::Unix needs a Unix socket"
                    };
                    return Err(match spec.source {
                        ListenerSource::HandedOff => ServeError::HandOff {
                            listener: spec.name.clone(),
                            reason,
                        },
                        _ => ServeError::Inherited {
                            listener: spec.name.clone(),
                            reason,
                        },
                    });
                }
            }
            let net_spec = net_listener(
                spec,
                adopted,
                &profiles,
                &limits,
                transport.as_ref(),
                hard,
                true,
            )?;
            net = net.listener(net_spec);
        }
        let tables = Arc::new(Snapshot::new(Tables {
            profiles,
            listeners: runtime_listeners.into_iter().map(Some).collect(),
            profile_caps,
            dedicated,
        }));
        let shared = Arc::new(Shared::<F, S, HookSet<A, D, K, O, G, E, C, T>> {
            family,
            service,
            admit,
            dispatch: hooks.dispatch,
            checkpoint: hooks.checkpoint,
            observer: hooks.observer,
            control: Arc::clone(&merged),
            tables: Arc::clone(&tables),
            counters: Arc::clone(&counters),
            catch_panics,
        });
        for l in &listeners {
            if let Some(reason) = l.public_bind {
                shared.observer.public_bind(&l.name, reason);
            }
        }
        let notifier = Arc::new(Notifier::new(
            notify,
            Arc::clone(&shared) as Arc<dyn ServerEvents>,
        ));
        let factory = Factory {
            shared,
            rt: Arc::clone(&rt),
        };
        let health = Arc::new(HealthState::new(listeners.len(), accept_failure_window));
        let events = Tap {
            inner: hooks.events,
            state: Arc::clone(&health),
            notifier: Arc::clone(&notifier),
        };
        let server = net
            .sessions(factory)
            .gate(hooks.gate)
            .events(events)
            .tls(hooks.tls)
            .build()
            .map_err(|e| bind_error(&listeners, e))?;
        // SameUser: the uid this process runs as is the owner of the socket
        // file it created (or was given; an inherited file is the service's).
        for info in server.listeners() {
            let cell = same_users
                .get(usize::from(info.id.get()))
                .and_then(Option::as_ref);
            if let (net_io::Endpoint::Unix(path), Some(cell)) = (&info.local, cell) {
                if let Some(uid) = file_owner(path) {
                    let _ = cell.set(uid);
                }
            }
        }
        let listeners = listeners.iter().map(listener_meta).collect();
        let reloader = Reloader {
            transport,
            tables,
            rt: Arc::clone(&rt),
            notifier: Arc::clone(&notifier),
            has_control,
            has_tls,
            workers,
            threads,
        };
        Ok(Bound::new(Parts {
            server,
            listeners,
            switch: merged as Arc<dyn DrainSwitch>,
            counters,
            health,
            notifier,
            reloader,
        }))
    }
}

/// How far net-io's read timeout sits behind serve-net's request deadlines,
/// so the deadline with an answer fires first.
const READ_MARGIN: Duration = Duration::from_secs(1);

/// The owner's clock, type-erased in the builder.
struct SharedClock(Arc<dyn net_io::Clock>);

impl net_io::Clock for SharedClock {
    fn now(&self) -> std::time::Instant {
        self.0.now()
    }
}

/// The soft connection cap, net-io's hard cap, and the pool size.
///
/// On the blocking driver every open connection holds a thread, and net-io
/// refuses at accept once every thread is busy, so the busy refusal can only
/// be written when the pool covers the soft cap plus its headroom (D-043,
/// D-044).
pub(crate) fn connection_caps(limits: &Limits, workers: &Workers) -> (u32, u32, u16) {
    let asked = workers.blocking_threads();
    let soft = limits.max_connections.unwrap_or_else(|| {
        // A small pool keeps most of its threads for clients.
        let t = u32::from(asked);
        t.saturating_sub(limits.headroom_for(t).min(t / 4)).max(1)
    });
    let hard = soft.saturating_add(limits.headroom_for(soft));
    let threads = u16::try_from(hard).unwrap_or(u16::MAX).max(asked);
    (soft, hard, threads)
}

/// The soft cap left for connections without a dedicated seat.
pub(crate) fn shared_seats(soft: u32, profiles: &ProfileTable) -> u32 {
    let kept: u32 = profiles
        .iter()
        .filter_map(|p| p.budget.dedicated)
        .map(u32::from)
        .sum();
    soft.saturating_sub(kept)
}

/// A listener's entry in the tables.
pub(crate) fn listener_runtime(l: &ListenerSpec, cap: Arc<Cap>) -> ListenerRuntime {
    ListenerRuntime {
        name: l.name.clone(),
        profile: l.profile,
        proxy: l.ingress.proxy().cloned(),
        declared_scheme: l.scheme,
        unix: match &l.ingress {
            Ingress::Unix(trust, _) => Some(UnixRuntime {
                trust: trust.clone(),
                same_user: Arc::default(),
            }),
            _ => None,
        },
        cap,
    }
}

/// What `listeners()` and a reload need to know about a listener.
pub(crate) fn listener_meta(l: &ListenerSpec) -> ListenerMeta {
    ListenerMeta {
        ingress: match l.ingress.proxy().map(|p| p.kind) {
            None if l.ingress.is_unix() => IngressKind::Unix,
            None if matches!(l.ingress, Ingress::DirectTls) => IngressKind::DirectTls,
            None => IngressKind::Direct,
            Some(ProxyKind::Headers(_)) => IngressKind::Forwarded,
            Some(ProxyKind::ProxyProtocol(_)) => IngressKind::ProxyProtocol,
        },
        profile: l.profile,
        public_bind: l.public_bind,
        name: l.name.clone(),
        source: l.source.clone(),
        transport: transport_key(&l.ingress),
        settings: format!("{:?} {:?} {:?}", l.ingress, l.scheme, l.max_connections),
    }
}

/// net-io's listener for a spec.
pub(crate) fn net_listener(
    spec: &ListenerSpec,
    adopted: Option<net_io::ListenerHandle>,
    profiles: &ProfileTable,
    limits: &Limits,
    base: Option<&net_io::Limits>,
    hard: u32,
    named: bool,
) -> Result<net_io::ListenerSpec, ServeError> {
    let net_spec = match (&spec.source, adopted) {
        (_, Some(handle)) => net_io::ListenerSpec::adopt(handle),
        (
            ListenerSource::Inherited {
                fallback: Some(addr),
                ..
            },
            None,
        ) => net_io::ListenerSpec::tcp(*addr),
        (ListenerSource::HandedOff | ListenerSource::Inherited { .. }, None) => {
            return Err(ServeError::HandOff {
                listener: spec.name.clone(),
                reason: "the listener has no socket",
            });
        }
        (ListenerSource::Tcp(addr), None) => match &spec.listen_options {
            Some(options) => net_io::ListenerSpec::tcp_with(*addr, *options),
            None => net_io::ListenerSpec::tcp(*addr),
        },
        (ListenerSource::Unix(path), None) => {
            let m = spec.unix_mode;
            let mut options = net_io::LocalOptions::new().mode(m.mode);
            if let Some(uid) = m.owner {
                options = options.owner(uid);
            }
            if let Some(gid) = m.group {
                options = options.group(gid);
            }
            net_io::ListenerSpec::unix(path.clone(), options)
        }
        (ListenerSource::Unresolved, None) => {
            return Err(ServeError::Bind {
                listener: spec.name.clone(),
                source: net_io::NetError::NoAddresses,
            });
        }
    };
    let compiled = profiles.compiled(spec.profile);
    let behind = spec.ingress.proxy().is_some();
    let idle = compiled.map_or_else(
        || KeepAlive::new().idle_for(behind),
        |c| c.keep_alive.idle_for(behind),
    );
    let effective = compiled.map_or(limits, |c| &c.limits);
    let stall = effective.write_stall;
    // serve-net's own deadlines decide how long a head and a body may take,
    // and they answer 408; net-io's read timeout only backs them up.
    let read = effective.head_timeout.max(effective.request_timeout) + READ_MARGIN;
    let mut own = spec
        .transport
        .as_ref()
        .map_or_else(net_io::Limits::builder, net_io::Limits::to_builder)
        .idle_timeout(idle)
        .write_timeout(stall)
        .output_budget(effective.max_pending_output)
        .min_rate(if effective.min_rate_bytes == 0 {
            net_io::MinRate::off()
        } else {
            net_io::MinRate::new(effective.min_rate_bytes, effective.min_rate_window)
        })
        .graceful_linger(effective.linger_total, effective.linger_bytes);
    let read_set = spec
        .transport
        .as_ref()
        .or(base)
        .is_some_and(|t| t.read_timeout().is_some());
    if !read_set {
        own = own.read_timeout(read);
    }
    if let Some(n) = spec.backlog {
        own = own.backlog(n);
    }
    let listener_hard = spec
        .max_connections
        .map_or(hard, |n| n.saturating_add(limits.headroom_for(n)).min(hard));
    if spec.max_connections.is_some() {
        own = own.max_per_listener(listener_hard);
    }
    let proxy = spec.ingress.proxy();
    if proxy.is_some_and(|p| matches!(p.kind, ProxyKind::Headers(_))) {
        // Every connection comes from the front end, so a per-source cap
        // would cap the whole listener; per-client limits behind a proxy
        // belong to a gate that sees the forwarded client (D-046, per listener
        // since net-io 0.4). Behind the PROXY protocol net-io moves the slot
        // to the real client, so the cap stays.
        own = own.max_per_source(listener_hard, net_io::PrefixPolicy::DEFAULT);
    }
    let own = own.build().map_err(ServeError::Start)?;
    let mut net_spec = net_spec.limits(own);
    if named {
        // A reload that replaces a listener binds it before the old one goes,
        // and net-io wants names unique.
        net_spec = net_spec.name(&spec.name);
    }
    if matches!(spec.ingress, Ingress::DirectTls) {
        net_spec = net_spec.tls(true);
    }
    if let Some(ProxySource {
        kind: ProxyKind::ProxyProtocol(versions),
        trusted,
        ..
    }) = proxy
    {
        net_spec = net_spec.proxy(net_io::ProxyMode::Required {
            versions: *versions,
        });
        // Over a Unix socket every peer that passed its credential check is
        // trusted; net-io ignores the list there.
        if !spec.ingress.is_unix() {
            net_spec = net_spec.trusted_proxies(trusted.clone());
        }
    }
    Ok(net_spec)
}

/// The first inherited listener's name, for an error about the environment.
fn first_inherited(listeners: &[ListenerSpec]) -> String {
    listeners
        .iter()
        .find(|l| matches!(l.source, ListenerSource::Inherited { .. }))
        .map_or_else(String::new, |l| l.name.clone())
}

/// Gives each inherited listener its socket: by name first, then the
/// unnamed sockets in order to the listeners left. Sockets no listener takes
/// are dropped (closed).
fn match_inherited(
    listeners: &[ListenerSpec],
    passed: Vec<net_io::InheritedListener>,
) -> Vec<Option<net_io::ListenerHandle>> {
    let mut pool: Vec<Option<net_io::InheritedListener>> = passed.into_iter().map(Some).collect();
    let mut out: Vec<Option<net_io::ListenerHandle>> = (0..listeners.len()).map(|_| None).collect();
    for (i, l) in listeners.iter().enumerate() {
        let ListenerSource::Inherited { name, .. } = &l.source else {
            continue;
        };
        let found = pool.iter_mut().find(|p| {
            p.as_ref()
                .is_some_and(|p| p.name.as_deref() == Some(name.as_str()))
        });
        if let Some(slot) = found {
            out[i] = slot.take().map(|p| p.handle);
        }
    }
    for (i, l) in listeners.iter().enumerate() {
        if !matches!(l.source, ListenerSource::Inherited { .. }) || out[i].is_some() {
            continue;
        }
        let unnamed = pool
            .iter_mut()
            .find(|p| p.as_ref().is_some_and(|p| p.name.is_none()));
        if let Some(slot) = unnamed {
            out[i] = slot.take().map(|p| p.handle);
        }
    }
    out
}

/// The owner of a file, where the platform has one.
#[cfg(unix)]
pub(crate) fn file_owner(path: &net_io::UnixPath) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path.as_path()).ok().map(|m| m.uid())
}

#[cfg(not(unix))]
pub(crate) fn file_owner(_path: &net_io::UnixPath) -> Option<u32> {
    None
}

/// Names the listener a net-io bind error is about.
fn bind_error(listeners: &[ListenerSpec], e: net_io::NetError) -> ServeError {
    use net_io::NetError;
    let addr = match &e {
        NetError::Bind { addr, .. }
        | NetError::AddrInUse { addr }
        | NetError::AddrNotAvailable { addr } => Some(*addr),
        NetError::PermissionDenied { .. } => None,
        _ => return ServeError::Start(e),
    };
    let named = listeners
        .iter()
        .find(|l| matches!(l.source, ListenerSource::Tcp(a) if Some(a) == addr))
        .or(listeners.first());
    ServeError::Bind {
        listener: named.map_or_else(String::new, |l| l.name.clone()),
        source: e,
    }
}

impl<H: Hooks> core::fmt::Debug for Built<H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Built")
            .field("listeners", &self.listeners)
            .field("warnings", &self.warnings)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProxySource, RuleId, TrustedProxies};

    /// Listening sockets, released by a throwaway net-io server.
    fn handles(n: usize) -> Vec<net_io::ListenerHandle> {
        let mut b = net_io::ServerBuilder::new();
        for _ in 0..n {
            b = b.listener(net_io::ListenerSpec::tcp("127.0.0.1:0"));
        }
        let s = b
            .handler(|_c: &mut net_io::TcpConnection| Ok(()))
            .gate(net_io::AllowAll)
            .build()
            .unwrap();
        s.release_listeners().unwrap()
    }

    #[test]
    fn test_match_inherited_by_name_then_by_position() {
        let mut h = handles(3);
        let passed = vec![
            net_io::InheritedListener {
                name: None,
                handle: h.pop().unwrap(),
            },
            net_io::InheritedListener {
                name: Some("api".into()),
                handle: h.pop().unwrap(),
            },
            net_io::InheritedListener {
                name: Some("unused".into()),
                handle: h.pop().unwrap(),
            },
        ];
        let api_addr = passed[1].handle.local().clone();
        let listeners = vec![
            ListenerSpec::inherited("web"),
            ListenerSpec::tcp("plain", "127.0.0.1:0"),
            ListenerSpec::inherited("api"),
            ListenerSpec::inherited("extra"),
        ];
        let got = match_inherited(&listeners, passed);
        // "api" by name; "web" takes the unnamed socket; "extra" gets none,
        // and the socket named "unused" is dropped.
        assert_eq!(got[2].as_ref().map(|h| h.local().clone()), Some(api_addr));
        assert!(got[0].is_some());
        assert!(got[1].is_none());
        assert!(got[3].is_none());
    }

    #[test]
    fn test_build_reports_every_error_at_once() {
        let errors = Server::builder()
            .listener(ListenerSpec::tcp("Bad Name", "127.0.0.1:0"))
            .listener(
                ListenerSpec::tcp("site", "0.0.0.0:8080").ingress(Ingress::Proxy(
                    ProxySource::forwarded_for(TrustedProxies::loopback()),
                )),
            )
            .build()
            .unwrap_err();
        assert!(errors.has(RuleId::BadName));
        assert!(errors.has(RuleId::PublicBind));
    }

    #[test]
    fn test_build_creates_profile_zero() {
        let built = Server::builder()
            .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
            .build()
            .unwrap();
        assert_eq!(built.profiles().len(), 1);
        assert!(built.warnings().is_empty());
    }

    #[test]
    fn test_hook_setters_change_types() {
        let built = Server::builder()
            .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
            .admit(crate::InFlightCap::new().per_worker(8))
            .dispatch(crate::Inline)
            .checkpoint(crate::NoCheckpoint)
            .observer(crate::NoObserver)
            .control(Arc::new(crate::SimpleControl::new()))
            .profile(Profile::new(ProfileId(0)).critical(true))
            .build()
            .unwrap();
        assert!(built.warnings().is_empty());
    }
}
