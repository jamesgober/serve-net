//! Telling a service manager or a supervisor what the server is doing.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::running::DrainReport;

/// Readiness for a service manager or a supervisor. Every method has an
/// empty default.
///
/// `ready` runs once every listener accepts, `stopping` when a drain starts,
/// `extend_timeout` for a drain longer than 5 s, `reloading` before a reload
/// swaps anything. A failure never stops the server: an implementation keeps
/// it for [`take_error`](Self::take_error), and the server reports it to
/// `Observer::notify_failed` and in `health`.
#[allow(unused_variables)]
pub trait Notify: Send + Sync + 'static {
    /// Every listener accepts.
    fn ready(&self) {}
    /// A reload starts; `monotonic_usec` is `net_io::monotonic_micros()`.
    fn reloading(&self, monotonic_usec: u64) {}
    /// A drain started.
    fn stopping(&self) {}
    /// One line of free-form status.
    fn status(&self, line: &str) {}
    /// The server is alive.
    fn watchdog(&self) {}
    /// The drain needs this much longer than the manager's stop timeout.
    fn extend_timeout(&self, by: Duration) {}
    /// The last failure, taken; `None` when there was none.
    fn take_error(&self) -> Option<io::Error> {
        None
    }
}

/// Tells nobody.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoNotify;

impl Notify for NoNotify {}

/// What the server calls when something happens outside a request.
pub(crate) trait ServerEvents: Send + Sync {
    fn drain_started(&self, open_connections: u32);
    fn drain_finished(&self, report: &DrainReport);
    fn notify_failed(&self, error: &io::Error);
}

/// The owner's [`Notify`] with failures passed on to the observer.
pub(crate) struct Notifier {
    notify: Arc<dyn Notify>,
    events: Arc<dyn ServerEvents>,
    failing: AtomicBool,
}

impl Notifier {
    pub(crate) fn new(notify: Arc<dyn Notify>, events: Arc<dyn ServerEvents>) -> Notifier {
        Notifier {
            notify,
            events,
            failing: AtomicBool::new(false),
        }
    }

    fn call(&self, f: impl FnOnce(&dyn Notify)) {
        f(self.notify.as_ref());
        match self.notify.take_error() {
            Some(e) => {
                self.failing.store(true, Ordering::Release);
                self.events.notify_failed(&e);
            }
            None => self.failing.store(false, Ordering::Release),
        }
    }

    pub(crate) fn ready(&self) {
        self.call(|n| n.ready());
    }

    /// From net-io's `DrainStarted` event: every way a drain can begin.
    pub(crate) fn drain_started(&self, open_connections: u32) {
        self.events.drain_started(open_connections);
        self.call(|n| n.stopping());
    }

    pub(crate) fn drain_finished(&self, report: &DrainReport) {
        self.events.drain_finished(report);
    }

    /// Asks the manager for more time when a drain may outlast its default.
    pub(crate) fn drain_deadline(&self, deadline: Duration) {
        if deadline > Duration::from_secs(5) {
            self.call(|n| n.extend_timeout(deadline));
        }
    }

    pub(crate) fn is_failing(&self) -> bool {
        self.failing.load(Ordering::Acquire)
    }
}

/// systemd's `sd_notify` in safe `std` code: one datagram per call to
/// `NOTIFY_SOCKET` (a path, or `@name` for the abstract namespace), with fixed
/// assignments only. Request data is never sent. Works with `Type=notify`
/// and `Type=notify-reload` (systemd 253 and later). Linux only.
///
/// ```no_run
/// use serve_net::{ListenerSpec, Server, SystemdNotify};
/// let mut builder = Server::builder().listener(ListenerSpec::tcp("web", "127.0.0.1:0"));
/// if let Some(notify) = SystemdNotify::from_env() {
///     builder = builder.notify(notify);
/// }
/// # let _ = builder;
/// ```
#[derive(Debug)]
pub struct SystemdNotify {
    #[cfg(target_os = "linux")]
    target: std::os::unix::net::SocketAddr,
    #[cfg(target_os = "linux")]
    socket: std::os::unix::net::UnixDatagram,
    #[cfg(target_os = "linux")]
    error: std::sync::Mutex<Option<io::Error>>,
}

/// The longest `STATUS=` value sent.
const STATUS_MAX: usize = 256;

/// `STATUS=` text: control characters become spaces, at most 256 bytes.
fn status_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len().min(STATUS_MAX));
    for c in line.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if out.len() + c.len_utf8() > STATUS_MAX {
            break;
        }
        out.push(c);
    }
    out
}

impl SystemdNotify {
    /// The manager's socket from `NOTIFY_SOCKET`, read once; `None` when it
    /// is unset or invalid, or not on Linux.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            SystemdNotify::to(&std::env::var("NOTIFY_SOCKET").ok()?)
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    /// `WATCHDOG_USEC`, when `WATCHDOG_PID` is unset or names this process.
    #[must_use]
    pub fn watchdog_interval() -> Option<Duration> {
        if let Ok(pid) = std::env::var("WATCHDOG_PID") {
            if pid.parse::<u32>().ok()? != std::process::id() {
                return None;
            }
        }
        let usec: u64 = std::env::var("WATCHDOG_USEC").ok()?.parse().ok()?;
        (usec > 0).then(|| Duration::from_micros(usec))
    }

    #[cfg(target_os = "linux")]
    fn to(target: &str) -> Option<Self> {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixDatagram};
        let target = match target.strip_prefix('@') {
            Some(name) => SocketAddr::from_abstract_name(name).ok()?,
            None => SocketAddr::from_pathname(target).ok()?,
        };
        Some(SystemdNotify {
            target,
            socket: UnixDatagram::unbound().ok()?,
            error: std::sync::Mutex::new(None),
        })
    }

    #[cfg(target_os = "linux")]
    fn send(&self, message: &str) {
        if let Err(e) = self.socket.send_to_addr(message.as_bytes(), &self.target) {
            *self
                .error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e);
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn send(&self, _message: &str) {}
}

impl Notify for SystemdNotify {
    fn ready(&self) {
        self.send("READY=1");
    }

    fn reloading(&self, monotonic_usec: u64) {
        self.send(&format!("RELOADING=1\nMONOTONIC_USEC={monotonic_usec}"));
    }

    fn stopping(&self) {
        self.send("STOPPING=1");
    }

    fn status(&self, line: &str) {
        self.send(&format!("STATUS={}", status_line(line)));
    }

    fn watchdog(&self) {
        self.send("WATCHDOG=1");
    }

    fn extend_timeout(&self, by: Duration) {
        self.send(&format!("EXTEND_TIMEOUT_USEC={}", by.as_micros()));
    }

    #[cfg(target_os = "linux")]
    fn take_error(&self) -> Option<io::Error> {
        self.error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_line_strips_controls_and_caps_length() {
        assert_eq!(status_line("a\nb\tc"), "a b c");
        let long = "é".repeat(200);
        let s = status_line(&long);
        assert!(s.len() <= STATUS_MAX && s.chars().all(|c| c == 'é'));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_systemd_notify_sends_the_assignments() {
        let path = std::env::temp_dir().join(format!("serve-net-notify-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let manager = std::os::unix::net::UnixDatagram::bind(&path).unwrap();
        let notify = SystemdNotify::to(path.to_str().unwrap()).unwrap();
        let mut buf = [0u8; 512];
        let mut next = || {
            let n = manager.recv(&mut buf).unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        };
        notify.ready();
        assert_eq!(next(), "READY=1");
        notify.reloading(42);
        assert_eq!(next(), "RELOADING=1\nMONOTONIC_USEC=42");
        notify.status("line\none");
        assert_eq!(next(), "STATUS=line one");
        notify.extend_timeout(Duration::from_secs(30));
        assert_eq!(next(), "EXTEND_TIMEOUT_USEC=30000000");
        notify.stopping();
        assert_eq!(next(), "STOPPING=1");
        assert!(notify.take_error().is_none());
        drop(manager);
        let _ = std::fs::remove_file(&path);
        notify.ready();
        assert!(notify.take_error().is_some(), "a failed send is kept");
    }
}
