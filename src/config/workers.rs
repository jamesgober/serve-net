//! Which net-io driver runs connections, and how many workers.

use crate::net::DriverKind;

/// Which net-io driver runs the connections, and how many workers.
///
/// ```
/// use serve_net::Workers;
/// let small = Workers::blocking(32);
/// assert_eq!(small.count(), Some(32));
/// assert_eq!(Workers::auto().count(), None);
/// assert_eq!(Workers::reactor(4).count(), Some(4));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Workers {
    pub(crate) kind: DriverKind,
    pub(crate) count: Option<u16>,
}

/// The blocking pool's default thread count.
pub(crate) const BLOCKING_DEFAULT: u16 = 256;

/// The connections a reactor's default cap holds, descriptors allowing.
pub(crate) const REACTOR_CONNECTIONS: u32 = 10_000;

impl Workers {
    /// The reactor, one worker per logical CPU.
    #[must_use]
    pub fn auto() -> Self {
        Workers {
            kind: DriverKind::Auto,
            count: None,
        }
    }

    /// One event loop per worker, each owning thousands of connections, with
    /// idle connections holding no read buffer. `workers` of 0 means one per
    /// logical CPU. A service must not block for long here: it delays every
    /// connection of its worker, so slow work belongs on an `Offload` pool.
    #[must_use]
    pub fn reactor(workers: u16) -> Self {
        Workers {
            kind: DriverKind::Reactor,
            count: (workers > 0).then_some(workers),
        }
    }

    /// A bounded pool of connection threads (default 256).
    #[must_use]
    pub fn blocking(threads: u16) -> Self {
        Workers {
            kind: DriverKind::Blocking,
            count: Some(threads),
        }
    }

    /// The driver kind.
    #[must_use]
    pub fn kind(&self) -> DriverKind {
        self.kind
    }

    /// The worker count; `None` for the driver's default.
    #[must_use]
    pub fn count(&self) -> Option<u16> {
        self.count
    }

    /// The driver that runs: `Auto` is the reactor for serve-net's sessions.
    /// CI builds with `serve_net_blocking_tests` to run the whole test suite
    /// on the blocking pool as well.
    pub(crate) fn resolved(&self) -> DriverKind {
        match self.kind {
            DriverKind::Auto if cfg!(serve_net_blocking_tests) => DriverKind::Blocking,
            DriverKind::Auto => DriverKind::Reactor,
            kind => kind,
        }
    }

    /// The event loops a reactor runs with.
    pub(crate) fn reactor_workers(&self) -> u16 {
        self.count.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(4, |n| u16::try_from(n.get()).unwrap_or(4096).min(4096))
        })
    }

    /// The thread count the blocking pool runs with.
    pub(crate) fn blocking_threads(&self) -> u16 {
        self.count.unwrap_or(BLOCKING_DEFAULT)
    }
}

impl Default for Workers {
    fn default() -> Self {
        Workers::auto()
    }
}
