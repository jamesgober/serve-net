//! A bound and a running server: the door, drain, and stop.

use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::conn::session::Runtime;
use crate::error::ServeError;
use crate::net::{ControlLine, ControlState};

/// The control line net-io reads: the owner's line merged with serve-net's
/// own drain request (D-032). A drain asked for through a [`DrainHandle`]
/// shows as `Draining` with its deadline; the owner's `Stopping` still wins.
pub(crate) struct Merged<C: ControlLine> {
    owner: Arc<C>,
    epoch: AtomicU64,
    drain: Mutex<Option<Instant>>,
}

impl<C: ControlLine> Merged<C> {
    pub(crate) fn new(owner: Arc<C>) -> Self {
        Merged {
            owner,
            epoch: AtomicU64::new(0),
            drain: Mutex::new(None),
        }
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
    /// Requests that completed during the drain (not counted yet: 0).
    pub requests_finished: u64,
    /// Requests refused because the server was draining (not counted yet: 0).
    pub refused_while_draining: u64,
    /// How long the drain took.
    pub elapsed: Duration,
    /// Whether the deadline passed with connections still open.
    pub deadline_hit: bool,
}

impl DrainReport {
    fn from_net(r: net_io::DrainReport) -> Self {
        DrainReport {
            graceful: r.graceful,
            killed: r.killed,
            requests_finished: 0,
            refused_while_draining: 0,
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
}

impl DrainHandle {
    /// Starts a drain that ends at most `deadline` from now, and returns at
    /// once; [`Running::wait`] returns its report.
    pub fn drain(&self, deadline: Duration) {
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

/// What a bound and a running server hold.
struct Inner {
    server: net_io::Server,
    names: Vec<String>,
    switch: Arc<dyn DrainSwitch>,
    /// Kept alive with the server; read by `stats` (not wired yet).
    _rt: Arc<Runtime>,
}

impl Inner {
    fn local_addrs(&self) -> impl Iterator<Item = (&str, SocketAddr)> + '_ {
        self.server.listeners().iter().filter_map(|l| {
            let name = self
                .names
                .get(usize::from(l.id.get()))
                .map_or("", String::as_str);
            l.local.socket_addr().map(|a| (name, a))
        })
    }
}

/// A server with every listener bound and the door closed.
#[must_use = "a Bound server accepts nothing until open() is called"]
pub struct Bound {
    inner: Inner,
}

impl Bound {
    pub(crate) fn new(
        server: net_io::Server,
        names: Vec<String>,
        switch: Arc<dyn DrainSwitch>,
        rt: Arc<Runtime>,
    ) -> Self {
        Bound {
            inner: Inner {
                server,
                names,
                switch,
                _rt: rt,
            },
        }
    }

    /// Each TCP listener's name and bound address.
    pub fn local_addrs(&self) -> impl Iterator<Item = (&str, SocketAddr)> + '_ {
        self.inner.local_addrs()
    }

    /// Opens the door on every listener.
    ///
    /// # Errors
    ///
    /// [`ServeError::Start`] when net-io refuses to open the door.
    pub fn open(self) -> Result<Running, ServeError> {
        self.inner.server.open_door().map_err(ServeError::Start)?;
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
        self.inner().local_addrs().next().map(|(_, a)| a)
    }

    /// Stops accepting on every listener; idempotent.
    pub fn close_door(&self) {
        self.inner().server.close_door();
    }

    /// Drains with `deadline` and shuts the server down.
    pub fn drain(mut self, deadline: Duration) -> DrainReport {
        let inner = self.inner.take().unwrap_or_else(|| unreachable!());
        DrainReport::from_net(inner.server.shutdown(Instant::now() + deadline))
    }

    /// A handle that starts a drain from any thread.
    #[must_use]
    pub fn drain_handle(&self) -> DrainHandle {
        let inner = self.inner();
        DrainHandle {
            switch: Arc::clone(&inner.switch),
            handle: inner.server.handle(),
        }
    }

    /// Parks until a drain is asked for, then drains and shuts down.
    pub fn wait(mut self) -> DrainReport {
        let inner = self.inner.take().unwrap_or_else(|| unreachable!());
        DrainReport::from_net(inner.server.wait_for_stop())
    }

    /// net-io's handle.
    #[must_use]
    pub fn server_handle(&self) -> net_io::ServerHandle {
        self.inner().server.handle()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            // A drop must not block for seconds: close everything now.
            let _ = inner.server.shutdown(Instant::now());
        }
    }
}

impl fmt::Debug for Running {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("Running");
        if let Some(inner) = &self.inner {
            d.field("listeners", &inner.local_addrs().collect::<Vec<_>>());
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
