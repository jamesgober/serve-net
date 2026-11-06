//! A bound and a running server: the door, drain, and stop.

use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::client::IngressKind;
use crate::config::{ListenerSource, ProfileId};
use crate::error::ServeError;
use crate::health::{Health, HealthState, ListenerInfo, ListenerState};
use crate::net::ConnectionId;
use crate::net::{ControlLine, ControlState};
use crate::notify::Notifier;
use crate::reload::{ReloadSummary, Reloader, ServerConfig};
use crate::reply::Waker;
use crate::stats::{Counters, Stats};

/// The control line net-io reads: the owner's line merged with serve-net's
/// own drain request (D-032). A drain asked for through a [`DrainHandle`]
/// shows as `Draining` with its deadline; the owner's `Stopping` still wins.
pub(crate) struct Merged<C: ControlLine> {
    owner: Arc<C>,
    epoch: AtomicU64,
    drain: Mutex<Option<Instant>>,
    /// Upgraded connections, told when a drain begins: they wait in the idle
    /// phase, where net-io's drain would close them without a goodbye.
    upgraded: Mutex<std::collections::HashMap<ConnectionId, Arc<dyn Waker>>>,
    /// Upgraded connections told of the drain that have not said goodbye yet.
    goodbyes: Mutex<std::collections::HashSet<ConnectionId>>,
}

impl<C: ControlLine> Merged<C> {
    pub(crate) fn new(owner: Arc<C>) -> Self {
        Merged {
            owner,
            epoch: AtomicU64::new(0),
            drain: Mutex::new(None),
            upgraded: Mutex::new(std::collections::HashMap::new()),
            goodbyes: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// An upgraded connection sent its goodbye (or closed) after a drain wake.
    pub(crate) fn said_goodbye(&self, connection: ConnectionId) {
        self.goodbyes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&connection);
    }

    /// An upgraded connection hears about a drain through `waker`.
    #[cfg(feature = "ws")]
    pub(crate) fn register_upgraded(&self, connection: ConnectionId, waker: Arc<dyn Waker>) {
        self.upgraded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(connection, waker);
    }

    #[cfg(feature = "ws")]
    pub(crate) fn unregister_upgraded(&self, connection: ConnectionId) {
        self.upgraded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&connection);
        self.said_goodbye(connection);
    }

    fn own(&self) -> Option<Instant> {
        *self.drain.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<C: ControlLine> ControlLine for Merged<C> {
    fn epoch(&self) -> u64 {
        // Both parts only grow, so the sum moves whenever either does.
        self.owner
            .epoch()
            .wrapping_add(self.epoch.load(Ordering::Acquire))
    }

    fn state(&self) -> ControlState {
        let owner = self.owner.state();
        match (self.own(), owner) {
            (Some(_), ControlState::Stopping) | (None, _) => owner,
            (Some(_), _) => ControlState::Draining,
        }
    }

    fn deadline(&self) -> Option<Instant> {
        let owner = match self.owner.state() {
            ControlState::Draining => self.owner.deadline(),
            _ => None,
        };
        match (self.own(), owner) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// serve-net's half of the merged line, without the owner's line type.
pub(crate) trait DrainSwitch: Send + Sync + 'static {
    /// Asks for a drain ending at `deadline`; a shorter deadline wins.
    fn request(&self, deadline: Instant);
    /// Whether a drain was asked for, here or on the owner's line.
    fn is_draining(&self) -> bool;
    /// Waits until every upgraded connection told of the drain has said
    /// goodbye, or `until`: net-io's drain closes idle connections at once.
    fn await_goodbyes(&self, until: Instant);
}

impl<C: ControlLine> DrainSwitch for Merged<C> {
    fn request(&self, deadline: Instant) {
        let mut own = self.drain.lock().unwrap_or_else(PoisonError::into_inner);
        let next = match *own {
            Some(d) if d <= deadline => return,
            _ => deadline,
        };
        *own = Some(next);
        drop(own);
        self.epoch.fetch_add(1, Ordering::AcqRel);
        // Upgraded connections wait for input; wake each so it says goodbye
        // (a WebSocket's 1001) before net-io's drain closes idle connections.
        let upgraded: Vec<(ConnectionId, Arc<dyn Waker>)> = self
            .upgraded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(id, w)| (*id, Arc::clone(w)))
            .collect();
        self.goodbyes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(upgraded.iter().map(|(id, _)| *id));
        for (id, waker) in upgraded {
            if waker.post(id, crate::reply::WAKE_DRAIN, 0).is_err() {
                self.said_goodbye(id);
            }
        }
    }

    fn await_goodbyes(&self, until: Instant) {
        while Instant::now() < until
            && !self
                .goodbyes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn is_draining(&self) -> bool {
        matches!(
            self.state(),
            ControlState::Draining | ControlState::Stopping
        )
    }
}

/// What a drain did.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrainReport {
    /// Connections that finished and closed normally.
    pub graceful: u32,
    /// Connections closed at the deadline.
    pub killed: u32,
    /// Requests that completed during the drain.
    pub requests_finished: u64,
    /// Requests refused while the server was draining.
    pub refused_while_draining: u64,
    /// How long the drain took.
    pub elapsed: Duration,
    /// Whether the deadline passed with connections still open.
    pub deadline_hit: bool,
}

impl DrainReport {
    fn from_net(r: net_io::DrainReport, counters: &Counters) -> Self {
        let (requests_finished, refused_while_draining) = counters.draining();
        DrainReport {
            graceful: r.graceful,
            killed: r.killed,
            requests_finished,
            refused_while_draining,
            elapsed: r.took,
            deadline_hit: r.killed > 0,
        }
    }
}

/// Starts a drain from any thread.
#[derive(Clone)]
pub struct DrainHandle {
    switch: Arc<dyn DrainSwitch>,
    handle: net_io::ServerHandle,
    notifier: Arc<Notifier>,
}

impl DrainHandle {
    /// Starts a drain that ends at most `deadline` from now, and returns at
    /// once; [`Running::wait`] returns its report.
    pub fn drain(&self, deadline: Duration) {
        self.notifier.drain_deadline(deadline);
        self.switch.request(Instant::now() + deadline);
        // net-io's accept thread runs the drain; wake every thread waiting in
        // the kernel so the line is read now (D-043).
        self.handle.wake_all();
    }

    /// Whether a drain was asked for.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.switch.is_draining()
    }
}

impl fmt::Debug for DrainHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DrainHandle")
            .field("draining", &self.is_draining())
            .finish()
    }
}

/// One listener's facts for `listeners()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListenerMeta {
    pub(crate) name: String,
    pub(crate) ingress: IngressKind,
    pub(crate) profile: ProfileId,
    pub(crate) public_bind: Option<&'static str>,
    pub(crate) source: ListenerSource,
    /// What net-io fixed at bind (`reload::transport_key`).
    pub(crate) transport: String,
    /// The settings a reload can change in place, as text for comparison.
    pub(crate) settings: String,
}

impl ListenerMeta {
    pub(crate) fn same_settings(&self, other: &ListenerMeta) -> bool {
        self.settings == other.settings
            && self.profile == other.profile
            && self.ingress == other.ingress
    }
}

/// Everything `bind` hands to [`Bound`].
pub(crate) struct Parts {
    pub(crate) server: net_io::Server,
    pub(crate) listeners: Vec<ListenerMeta>,
    pub(crate) switch: Arc<dyn DrainSwitch>,
    pub(crate) counters: Arc<Counters>,
    pub(crate) health: Arc<HealthState>,
    pub(crate) notifier: Arc<Notifier>,
    pub(crate) reloader: Reloader,
}

/// How often `settle_accepts` looks, how many quiet looks end it, and the most
/// it waits: about 3 ms per drain, none on a request.
const ACCEPT_SETTLE_TICK: Duration = Duration::from_millis(1);
const ACCEPT_SETTLE_QUIET: u32 = 3;
const ACCEPT_SETTLE_MAX: Duration = Duration::from_millis(50);
/// The longest a drain waits for an accept thread to start it.
/// The longest a drain waits for upgraded connections' goodbyes.
const GOODBYE_MAX: Duration = Duration::from_millis(250);
const DRAIN_START_MAX: Duration = Duration::from_millis(100);

/// What a bound and a running server hold.
struct Inner {
    /// Locked by `reload` and `listeners`; a request never takes it.
    server: Mutex<net_io::Server>,
    handle: net_io::ServerHandle,
    /// By net-io listener id.
    listeners: Mutex<Vec<Option<ListenerMeta>>>,
    switch: Arc<dyn DrainSwitch>,
    counters: Arc<Counters>,
    health: Arc<HealthState>,
    notifier: Arc<Notifier>,
    reloader: Reloader,
}

impl Inner {
    fn server(&self) -> std::sync::MutexGuard<'_, net_io::Server> {
        self.server.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn metas(&self) -> std::sync::MutexGuard<'_, Vec<Option<ListenerMeta>>> {
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn listener_infos(&self, opened: bool) -> Vec<ListenerInfo> {
        let door = self.handle.door();
        let metas = self.metas();
        let server = self.server();
        server
            .listeners()
            .iter()
            .filter_map(|l| {
                let i = usize::from(l.id.get());
                let meta = metas.get(i)?.as_ref()?;
                let state = if self.health.listener_failing(i) {
                    ListenerState::Failing
                } else if !opened {
                    ListenerState::Bound
                } else if door == net_io::DoorState::Open {
                    ListenerState::Accepting
                } else {
                    ListenerState::DoorClosed
                };
                Some(ListenerInfo {
                    name: meta.name.clone(),
                    local: l.local.clone(),
                    ingress: meta.ingress,
                    profile: meta.profile,
                    state,
                    public_bind_reason: meta.public_bind,
                })
            })
            .collect()
    }

    /// Closes the door and waits until accepting has gone quiet and every
    /// accepted connection reached `on_open`. An accept thread ends its batch
    /// after the door closes, so a drain that starts first can count nothing
    /// open and finish, and until `on_open` net-io takes a connection for
    /// idle: either way the connection is reset with its request unread.
    fn settle_accepts(&self) {
        self.handle.close_door();
        let give_up = Instant::now() + ACCEPT_SETTLE_MAX;
        let mut last = self.handle.stats().accepted;
        let mut quiet = 0;
        while quiet < ACCEPT_SETTLE_QUIET && Instant::now() < give_up {
            std::thread::sleep(ACCEPT_SETTLE_TICK);
            let accepted = self.handle.stats().accepted;
            let settled = accepted == last && self.counters.opened() >= accepted;
            quiet = if settled { quiet + 1 } else { 0 };
            last = accepted;
        }
    }

    /// Waits for net-io's accept thread to start the drain from the control
    /// line: it does so between batches, so nothing it accepted is left behind.
    /// A server whose accept threads all failed has nobody to start it, so
    /// the wait is bounded.
    fn await_drain_start(&self) {
        self.handle.wake_all();
        let give_up = Instant::now() + DRAIN_START_MAX;
        while !self.notifier.is_drain_started() && Instant::now() < give_up {
            std::thread::sleep(ACCEPT_SETTLE_TICK);
        }
    }

    /// Each TCP listener's name and address.
    fn local_addrs(&self) -> Vec<(String, SocketAddr)> {
        let metas = self.metas();
        let server = self.server();
        server
            .listeners()
            .iter()
            .filter_map(|l| {
                let name = metas
                    .get(usize::from(l.id.get()))?
                    .as_ref()
                    .map_or_else(String::new, |m| m.name.clone());
                l.local.socket_addr().map(|a| (name, a))
            })
            .collect()
    }

    fn into_server(self) -> net_io::Server {
        self.server
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// A server with every listener bound and the door closed.
#[must_use = "a Bound server accepts nothing until open() is called"]
pub struct Bound {
    inner: Inner,
    addrs: Vec<(String, SocketAddr)>,
}

impl Bound {
    pub(crate) fn new(parts: Parts) -> Self {
        let handle = parts.server.handle();
        let inner = Inner {
            server: Mutex::new(parts.server),
            handle,
            listeners: Mutex::new(parts.listeners.into_iter().map(Some).collect()),
            switch: parts.switch,
            counters: parts.counters,
            health: parts.health,
            notifier: parts.notifier,
            reloader: parts.reloader,
        };
        let addrs = inner.local_addrs();
        Bound { inner, addrs }
    }

    /// Each TCP listener's name and bound address.
    pub fn local_addrs(&self) -> impl Iterator<Item = (&str, SocketAddr)> + '_ {
        self.addrs.iter().map(|(name, addr)| (name.as_str(), *addr))
    }

    /// Name, address, ingress, profile, and state per listener.
    #[must_use]
    pub fn listeners(&self) -> Vec<ListenerInfo> {
        self.inner.listener_infos(false)
    }

    /// Opens the door on every listener.
    ///
    /// # Errors
    ///
    /// [`ServeError::Start`] when net-io refuses to open the door.
    pub fn open(self) -> Result<Running, ServeError> {
        self.inner.server().open_door().map_err(ServeError::Start)?;
        self.inner.notifier.ready();
        Ok(Running {
            inner: Some(self.inner),
        })
    }
}

impl fmt::Debug for Bound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bound")
            .field("listeners", &self.local_addrs().collect::<Vec<_>>())
            .finish()
    }
}

/// A running server.
#[must_use = "dropping Running closes every connection at once; call wait() or drain()"]
pub struct Running {
    inner: Option<Inner>,
}

impl Running {
    fn inner(&self) -> &Inner {
        // Present until `drain`, `wait`, or drop takes it.
        self.inner.as_ref().unwrap_or_else(|| unreachable!())
    }

    /// The first TCP listener's address (useful with port 0).
    #[must_use]
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.inner().local_addrs().first().map(|(_, a)| *a)
    }

    /// Name, address, ingress, profile, and state per listener (allocates;
    /// not a hot path).
    #[must_use]
    pub fn listeners(&self) -> Vec<ListenerInfo> {
        self.inner().listener_infos(true)
    }

    /// Stops accepting on every listener; idempotent.
    pub fn close_door(&self) {
        self.inner().handle.close_door();
    }

    /// Request counters summed over the workers, with net-io's transport
    /// counters.
    #[must_use]
    pub fn stats(&self) -> Stats {
        let inner = self.inner();
        let mut s = inner.counters.sum();
        s.transport = inner.handle.stats();
        s
    }

    /// Health, computed on call: listeners accepting, workers making
    /// progress, the share of busy refusals since the last call, the drain.
    #[must_use]
    pub fn health(&self) -> Health {
        let inner = self.inner();
        let names: Vec<(usize, String)> = inner
            .metas()
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.as_ref().map(|m| (i, m.name.clone())))
            .collect();
        inner.health.compute(
            &names,
            &inner.counters,
            inner.switch.is_draining(),
            inner.notifier.is_failing(),
        )
    }

    /// Drains with `deadline` and shuts the server down.
    pub fn drain(mut self, deadline: Duration) -> DrainReport {
        let inner = self.inner.take().unwrap_or_else(|| unreachable!());
        inner.notifier.drain_deadline(deadline);
        let deadline = Instant::now() + deadline;
        inner.settle_accepts();
        // Replies in flight read the line live and carry the close signal.
        inner.switch.request(deadline);
        // Upgraded connections say goodbye first (a WebSocket's 1001): at most
        // 250 ms, never past the deadline (D-092).
        inner
            .switch
            .await_goodbyes(deadline.min(Instant::now() + GOODBYE_MAX));
        inner.await_drain_start();
        let (counters, notifier) = (Arc::clone(&inner.counters), Arc::clone(&inner.notifier));
        let report = DrainReport::from_net(inner.into_server().shutdown(deadline), &counters);
        notifier.drain_finished(&report);
        report
    }

    /// A handle that starts a drain from any thread.
    #[must_use]
    pub fn drain_handle(&self) -> DrainHandle {
        let inner = self.inner();
        DrainHandle {
            switch: Arc::clone(&inner.switch),
            handle: inner.handle.clone(),
            notifier: Arc::clone(&inner.notifier),
        }
    }

    /// Parks until a drain is asked for, then drains and shuts down.
    pub fn wait(mut self) -> DrainReport {
        let inner = self.inner.take().unwrap_or_else(|| unreachable!());
        let (counters, notifier) = (Arc::clone(&inner.counters), Arc::clone(&inner.notifier));
        let report = DrainReport::from_net(inner.into_server().wait_for_stop(), &counters);
        notifier.drain_finished(&report);
        report
    }

    /// What a watchdog reads and reports through.
    pub(crate) fn vitals(&self) -> (Arc<Counters>, Arc<Notifier>) {
        let inner = self.inner();
        (Arc::clone(&inner.counters), Arc::clone(&inner.notifier))
    }

    /// net-io's handle.
    #[must_use]
    pub fn server_handle(&self) -> net_io::ServerHandle {
        self.inner().handle.clone()
    }

    /// Applies a new configuration without dropping a connection: validates
    /// it (nothing changes on failure), binds added listeners, publishes the
    /// new tables, opens the new doors, then closes and drains removed
    /// listeners. Connections see the new tables at their next request head;
    /// work in flight keeps the tables it started with. Reloads are
    /// serialized.
    ///
    /// # Errors
    ///
    /// [`ServeError::Reload`] (nothing changed), [`ServeError::ReloadBind`]
    /// (nothing changed; the new listeners were closed),
    /// [`ServeError::Draining`].
    pub fn reload(&self, next: ServerConfig) -> Result<ReloadSummary, ServeError> {
        let inner = self.inner();
        if inner.switch.is_draining() {
            return Err(ServeError::Draining);
        }
        let mut server = inner.server();
        let mut metas = inner.metas();
        inner.reloader.run(&mut server, &mut metas, next)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            // A drop must not block for seconds: close everything now.
            let notifier = Arc::clone(&inner.notifier);
            let _ = inner.into_server().shutdown(Instant::now());
            notifier.finish();
        }
    }
}

impl fmt::Debug for Running {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("Running");
        if let Some(inner) = &self.inner {
            d.field("listeners", &inner.local_addrs());
        }
        d.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::SimpleControl;

    #[test]
    fn test_merged_line_drain_and_owner_stop() {
        let owner = Arc::new(SimpleControl::new());
        let line = Merged::new(Arc::clone(&owner));
        let e0 = line.epoch();
        assert_eq!(line.state(), ControlState::Normal);
        let soon = Instant::now() + Duration::from_secs(5);
        line.request(soon);
        assert!(line.epoch() > e0);
        assert_eq!(line.state(), ControlState::Draining);
        assert_eq!(line.deadline(), Some(soon));
        // A longer deadline is ignored; a shorter one wins.
        let e1 = line.epoch();
        line.request(soon + Duration::from_secs(10));
        assert_eq!(line.epoch(), e1);
        line.request(soon - Duration::from_secs(1));
        assert_eq!(line.deadline(), Some(soon - Duration::from_secs(1)));
        owner.set(ControlState::Stopping, None);
        assert_eq!(line.state(), ControlState::Stopping);
    }
}
