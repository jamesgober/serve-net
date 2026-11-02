//! Telling a service manager or a supervisor what the server is doing.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::error::ConfigErrors;
use crate::reload::ReloadSummary;
use crate::running::{DrainReport, Running};
use crate::stats::Counters;

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
    fn reloaded(&self, summary: &ReloadSummary);
    fn reload_refused(&self, errors: &ConfigErrors);
    fn notify_failed(&self, error: &io::Error);
}

/// The owner's [`Notify`] with failures passed on to the observer.
pub(crate) struct Notifier {
    notify: Arc<dyn Notify>,
    events: Arc<dyn ServerEvents>,
    failing: AtomicBool,
    drain_started: AtomicBool,
    finished: AtomicBool,
}

impl Notifier {
    pub(crate) fn new(notify: Arc<dyn Notify>, events: Arc<dyn ServerEvents>) -> Notifier {
        Notifier {
            notify,
            events,
            failing: AtomicBool::new(false),
            drain_started: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        }
    }

    fn call(&self, f: impl FnOnce(&dyn Notify)) {
        f(self.notify.as_ref());
        self.record(self.notify.take_error());
    }

    fn record(&self, error: Option<io::Error>) {
        match error {
            Some(e) => {
                self.failing.store(true, Ordering::Release);
                self.events.notify_failed(&e);
            }
            None => self.failing.store(false, Ordering::Release),
        }
    }

    /// One watchdog ping through the pinger's own [`Notify`].
    fn ping(&self, notify: &dyn Notify) {
        notify.watchdog();
        self.record(notify.take_error());
    }

    pub(crate) fn ready(&self) {
        self.call(|n| n.ready());
    }

    /// From net-io's `DrainStarted` event: every way a drain can begin.
    pub(crate) fn drain_started(&self, open_connections: u32) {
        self.drain_started.store(true, Ordering::Release);
        self.events.drain_started(open_connections);
        self.call(|n| n.stopping());
    }

    pub(crate) fn drain_finished(&self, report: &DrainReport) {
        self.finished.store(true, Ordering::Release);
        self.events.drain_finished(report);
    }

    pub(crate) fn is_drain_started(&self) -> bool {
        self.drain_started.load(Ordering::Acquire)
    }

    /// The server closed without a drain.
    pub(crate) fn finish(&self) {
        self.finished.store(true, Ordering::Release);
    }

    fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// Asks the manager for more time when a drain may outlast its default.
    pub(crate) fn drain_deadline(&self, deadline: Duration) {
        if deadline > Duration::from_secs(5) {
            self.call(|n| n.extend_timeout(deadline));
        }
    }

    /// A reload is about to swap the tables.
    pub(crate) fn reloading(&self, monotonic_usec: u64) {
        self.call(|n| n.reloading(monotonic_usec));
    }

    /// A reload applied: the service is ready again.
    pub(crate) fn reloaded(&self, summary: &ReloadSummary) {
        self.events.reloaded(summary);
        self.call(|n| n.ready());
    }

    pub(crate) fn reload_refused(&self, errors: &ConfigErrors) {
        self.events.reload_refused(errors);
    }

    pub(crate) fn is_failing(&self) -> bool {
        self.failing.load(Ordering::Acquire)
    }
}

/// Pings a service manager's watchdog while every worker is healthy.
///
/// A parked thread wakes at half `interval` and calls [`Notify::watchdog`]
/// only if every worker made progress or sat outside a service call since the
/// last wake, so a stuck worker stops the pings and the manager restarts the
/// process. A service call that runs longer than `interval` looks stuck: keep
/// slow work on an `Offload` pool or raise the interval. Pings go on through a
/// drain and end when it finishes.
///
/// The thread is not started when `WATCHDOG_PID` names another process.
/// Stopping or dropping the pinger ends the thread at once.
///
/// Cost: one thread, one relaxed read per worker per wake.
///
/// ```no_run
/// # #[cfg(feature = "http1")]
/// # fn main() -> Result<(), serve_net::ServeError> {
/// use serve_net::{SystemdNotify, WatchdogPinger};
/// use std::sync::Arc;
/// # let server = serve_net::http("127.0.0.1:0", |_req, reply| { reply.text(200, "ok"); })?;
/// if let (Some(notify), Some(interval)) =
///     (SystemdNotify::from_env(), SystemdNotify::watchdog_interval())
/// {
///     let pinger = WatchdogPinger::start(&server, Arc::new(notify), interval);
///     let _report = server.wait();
///     pinger.stop();
/// }
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "http1"))]
/// # fn main() {}
/// ```
#[derive(Debug)]
pub struct WatchdogPinger {
    stop: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

/// The fastest a pinger wakes.
const PING_FLOOR: Duration = Duration::from_millis(1);

impl WatchdogPinger {
    /// Starts pinging `notify` for `server`. A failed ping reaches
    /// `Observer::notify_failed` and `health`, as every `Notify` failure does.
    #[must_use = "dropping a WatchdogPinger stops the pings"]
    pub fn start(server: &Running, notify: Arc<dyn Notify>, interval: Duration) -> Self {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let (counters, notifier) = server.vitals();
        let every = (interval / 2).max(PING_FLOOR);
        let thread = watchdog_pid_is_ours().then(|| {
            let stop = Arc::clone(&stop);
            let pinger = Arc::clone(&notifier);
            std::thread::Builder::new()
                .name("serve-net-watchdog".into())
                .spawn(move || ping_loop(&stop, &pinger, &counters, notify.as_ref(), every))
                .map_err(|e| notifier.record(Some(e)))
                .ok()
        });
        WatchdogPinger {
            stop,
            thread: thread.flatten(),
        }
    }

    /// Stops the pings and joins the thread.
    pub fn stop(self) {}
}

impl Drop for WatchdogPinger {
    fn drop(&mut self) {
        let (stopped, wake) = &*self.stop;
        *stopped.lock().unwrap_or_else(PoisonError::into_inner) = true;
        wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn ping_loop(
    stop: &(Mutex<bool>, Condvar),
    notifier: &Notifier,
    counters: &Counters,
    notify: &dyn Notify,
    every: Duration,
) {
    let (stopped, wake) = stop;
    let mut before: Vec<u64> = counters.workers().map(|(progress, _)| progress).collect();
    loop {
        let guard = stopped.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = wake
            .wait_timeout_while(guard, every, |stopped| !*stopped)
            .unwrap_or_else(PoisonError::into_inner);
        if *guard || notifier.is_finished() {
            return;
        }
        drop(guard);
        let mut healthy = true;
        for ((progress, in_call), last) in counters.workers().zip(&mut before) {
            healthy &= !in_call || progress != *last;
            *last = progress;
        }
        if healthy {
            notifier.ping(notify);
        }
    }
}

/// Whether `WATCHDOG_PID` is unset or names this process.
fn watchdog_pid_is_ours() -> bool {
    std::env::var("WATCHDOG_PID").map_or(true, |pid| pid.parse() == Ok(std::process::id()))
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
        if !watchdog_pid_is_ours() {
            return None;
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
