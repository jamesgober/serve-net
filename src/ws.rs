//! WebSocket connections after an upgrade (feature `ws`).
//!
//! A service answers `head` with
//! `HeadAction::Upgrade(WebSocketTarget::new(chat).into())`; serve-net checks
//! the handshake through ws-net, writes the 101, and from then on calls the
//! target's [`WebSocketService`] on the worker once per complete message. The
//! service sends through the [`WebSocketConn`] it is given. serve-net does
//! every read and write: pings, the close handshake, and the budgets stay in
//! ws-net and the machine.

use core::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::ProfileId;
use crate::engine::{Engine, EngineError, OutBuf, RefusalKind};
use crate::envelope::Envelope;

/// The largest frame a target accepts by default: 64 KiB.
pub const DEFAULT_MAX_FRAME: u64 = 64 * 1024;
/// The largest message a target accepts by default: 1 MiB.
pub const DEFAULT_MAX_MESSAGE: u64 = 1024 * 1024;
/// How long a quiet connection waits before serve-net pings it, by default.
pub const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(30);
/// How long one message may take, first byte to last (`WebSocketTarget::message_timeout`).
pub const DEFAULT_MESSAGE_TIMEOUT: Duration = Duration::from_secs(30);

/// The conversation after a WebSocket upgrade, called on the worker.
///
/// Every call runs on the connection's worker, as a `Quick` service would: a
/// call that blocks delays every connection of that worker. Messages arrive
/// in order, one call at a time per connection.
pub trait WebSocketService: Send + Sync + 'static {
    /// After the 101 left, before the first message.
    fn opened(&self, env: &Envelope, conn: &mut WebSocketConn<'_>) {
        let _ = (env, conn);
    }

    /// One complete message, text validated as UTF-8.
    fn message(&self, env: &Envelope, message: WsMessage<'_>, conn: &mut WebSocketConn<'_>);

    /// The connection ended, exactly once after `opened`. `code` is the close
    /// code of the handshake that ended it (either side's), `None` when the
    /// connection ended without one.
    fn closed(&self, env: &Envelope, code: Option<u16>) {
        let _ = (env, code);
    }
}

/// A complete message.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsMessage<'a> {
    /// A text message.
    Text(&'a str),
    /// A binary message.
    Binary(&'a [u8]),
}

/// Why a send was refused.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsSendError {
    /// The output waiting for the client is over the target's
    /// `max_pending_output`: the client is not reading.
    Full,
    /// A Close was sent or received; nothing more can be sent.
    Closed,
    /// ws-net refused the call (a close code that may not be sent, a reason
    /// over 123 bytes, a ping payload over 125 bytes).
    Invalid,
}

impl fmt::Display for WsSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            WsSendError::Full => "the client is not reading: output over its budget",
            WsSendError::Closed => "the connection is closing",
            WsSendError::Invalid => "ws-net refused the frame",
        })
    }
}

impl std::error::Error for WsSendError {}

/// One connection's sending side, during a [`WebSocketService`] call.
pub struct WebSocketConn<'a> {
    pub(crate) conn: &'a mut ws_net::Connection,
    pub(crate) max_pending: usize,
    pub(crate) outbox: &'a Arc<Outbox>,
}

/// Sends on an open WebSocket from any thread: a handle from
/// [`WebSocketConn::sender`].
///
/// Messages wait in a queue bounded by the connection's output budget
/// (`max_pending_output`) and are written by the connection's worker, in
/// order, after anything the service sent from its own calls. A full queue
/// refuses with [`WsSendError::Full`]; a connection that closed refuses with
/// [`WsSendError::Closed`]. Each message is copied once into the queue.
#[derive(Clone)]
pub struct WsSender {
    outbox: Arc<Outbox>,
}

impl WsSender {
    /// Queues a text message.
    ///
    /// # Errors
    ///
    /// [`WsSendError::Full`] when the queue holds the budget already,
    /// [`WsSendError::Closed`] once the connection closed.
    pub fn send_text(&self, text: &str) -> Result<(), WsSendError> {
        self.outbox.push(Queued::Text(text.to_owned()))
    }

    /// Queues a binary message.
    ///
    /// # Errors
    ///
    /// As [`send_text`](Self::send_text).
    pub fn send_binary(&self, bytes: &[u8]) -> Result<(), WsSendError> {
        self.outbox.push(Queued::Binary(bytes.to_vec()))
    }

    /// Whether the connection closed: every send is refused from then on.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.outbox.lock().closed
    }
}

impl fmt::Debug for WsSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.outbox.lock();
        f.debug_struct("WsSender")
            .field("queued", &state.items.len())
            .field("closed", &state.closed)
            .finish()
    }
}

/// A message waiting in an outbox.
pub(crate) enum Queued {
    Text(String),
    Binary(Vec<u8>),
}

impl Queued {
    pub(crate) fn len(&self) -> usize {
        match self {
            Queued::Text(t) => t.len(),
            Queued::Binary(b) => b.len(),
        }
    }
}

pub(crate) struct OutboxState {
    pub(crate) items: std::collections::VecDeque<Queued>,
    pub(crate) bytes: usize,
    pub(crate) closed: bool,
}

/// The queue between `WsSender`s and the connection's worker.
pub(crate) struct Outbox {
    state: crate::sync::Mutex<OutboxState>,
    waker: Arc<dyn crate::reply::Waker>,
    connection: crate::net::ConnectionId,
    max_bytes: usize,
}

impl Outbox {
    pub(crate) fn new(
        waker: Arc<dyn crate::reply::Waker>,
        connection: crate::net::ConnectionId,
        max_bytes: usize,
    ) -> Self {
        Outbox {
            state: crate::sync::Mutex::new(OutboxState {
                items: std::collections::VecDeque::new(),
                bytes: 0,
                closed: false,
            }),
            waker,
            connection,
            max_bytes,
        }
    }

    pub(crate) fn lock(&self) -> crate::sync::MutexGuard<'_, OutboxState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `push` of a text message (loom models).
    #[cfg(serve_net_loom)]
    pub(crate) fn push_text_for_tests(&self, text: &str) -> Result<(), WsSendError> {
        self.push(Queued::Text(text.to_owned()))
    }

    fn push(&self, message: Queued) -> Result<(), WsSendError> {
        let len = message.len();
        let mut state = self.lock();
        if state.closed {
            return Err(WsSendError::Closed);
        }
        if state.bytes.saturating_add(len) > self.max_bytes {
            return Err(WsSendError::Full);
        }
        let first = state.items.is_empty();
        state.items.push_back(message);
        state.bytes += len;
        drop(state);
        // One wake per batch: the worker takes everything queued when it runs.
        if first {
            let _ = self.waker.post(self.connection, crate::reply::WAKE_PUSH, 0);
        }
        Ok(())
    }

    /// The connection closed: refuse from now on and drop what waits,
    /// zeroed first under a wiping profile.
    pub(crate) fn close(&self, wipe: bool) {
        let mut state = self.lock();
        state.closed = true;
        if wipe {
            for item in &mut state.items {
                match item {
                    Queued::Text(t) => {
                        // A string's bytes, zeroed through its own buffer.
                        let mut bytes = core::mem::take(t).into_bytes();
                        crate::engine::wipe_bytes(&mut bytes);
                    }
                    Queued::Binary(b) => crate::engine::wipe_bytes(b),
                }
            }
        }
        state.items.clear();
        state.bytes = 0;
    }
}

impl WebSocketConn<'_> {
    /// A handle that sends on this connection from any thread, for as long
    /// as it stays open.
    #[must_use]
    pub fn sender(&self) -> WsSender {
        WsSender {
            outbox: Arc::clone(self.outbox),
        }
    }

    fn check(&mut self, len: usize) -> Result<(), WsSendError> {
        if self.conn.state() != ws_net::State::Open {
            return Err(WsSendError::Closed);
        }
        if self.conn.output().len().saturating_add(len) > self.max_pending {
            return Err(WsSendError::Full);
        }
        Ok(())
    }

    /// Sends a text message.
    ///
    /// # Errors
    ///
    /// [`WsSendError::Full`] when the client is not reading, [`WsSendError::Closed`]
    /// after a Close.
    pub fn send_text(&mut self, text: &str) -> Result<(), WsSendError> {
        self.check(text.len())?;
        self.conn.send_text(text).map_err(|_| WsSendError::Invalid)
    }

    /// Sends a binary message.
    ///
    /// # Errors
    ///
    /// As [`send_text`](Self::send_text).
    pub fn send_binary(&mut self, data: &[u8]) -> Result<(), WsSendError> {
        self.check(data.len())?;
        self.conn
            .send_binary(data)
            .map_err(|_| WsSendError::Invalid)
    }

    /// Starts the closing handshake; the connection ends when the client
    /// answers, or at the next ping interval.
    ///
    /// # Errors
    ///
    /// [`WsSendError::Invalid`] for a code that may not be sent or a reason
    /// over 123 bytes, [`WsSendError::Closed`] after a Close.
    pub fn close(&mut self, code: u16, reason: &str) -> Result<(), WsSendError> {
        if self.conn.state() != ws_net::State::Open {
            return Err(WsSendError::Closed);
        }
        self.conn
            .close(ws_net::CloseCode::new(code), reason)
            .map_err(|_| WsSendError::Invalid)
    }

    /// Bytes waiting for the client.
    #[must_use]
    pub fn pending_output(&mut self) -> usize {
        self.conn.output().len()
    }
}

impl fmt::Debug for WebSocketConn<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebSocketConn")
            .field("state", &self.conn.state())
            .field("max_pending", &self.max_pending)
            .finish()
    }
}

/// Where a WebSocket upgrade goes: the service, ws-net's budgets, the
/// subprotocol, and which web pages may connect.
#[derive(Clone)]
#[must_use]
pub struct WebSocketTarget {
    pub(crate) service: Arc<dyn WebSocketService>,
    pub(crate) limits: ws_net::Limits,
    pub(crate) subprotocol: Option<&'static str>,
    pub(crate) profile: Option<ProfileId>,
    pub(crate) max_pending: Option<usize>,
    pub(crate) ping_interval: Duration,
    pub(crate) message_timeout: Duration,
    /// `None` is the same-origin rule, which costs no allocation.
    pub(crate) origins: Option<Arc<ws_net::OriginPolicy>>,
}

impl WebSocketTarget {
    /// A target for `service`, with 64 KiB frames, 1 MiB messages, no
    /// subprotocol, the connection's profile, a ping after 30 s of quiet, and
    /// the same-origin rule.
    pub fn new<S: WebSocketService>(service: S) -> Self {
        Self::shared(Arc::new(service))
    }

    /// A target for a service shared by many upgrades (one reference count
    /// per upgrade, no allocation).
    pub fn shared(service: Arc<dyn WebSocketService>) -> Self {
        WebSocketTarget {
            service,
            limits: ws_net::Limits::new()
                .with_max_frame(DEFAULT_MAX_FRAME)
                .with_max_message(DEFAULT_MAX_MESSAGE),
            subprotocol: None,
            profile: None,
            max_pending: None,
            ping_interval: DEFAULT_PING_INTERVAL,
            message_timeout: DEFAULT_MESSAGE_TIMEOUT,
            origins: None,
        }
    }

    /// ws-net's frame and message budgets, checked from frame headers before
    /// any payload is read.
    pub fn limits(mut self, limits: ws_net::Limits) -> Self {
        self.limits = limits;
        self
    }

    /// The subprotocol the 101 names. It must be one the client offered in
    /// `Sec-WebSocket-Protocol` (read it in `head`); otherwise the upgrade is
    /// answered with 500, a service bug.
    pub fn subprotocol(mut self, name: &'static str) -> Self {
        self.subprotocol = Some(name);
        self
    }

    /// The profile whose `max_pending_output` the connection runs under after
    /// the switch; by default the connection's own.
    pub fn profile(mut self, id: ProfileId) -> Self {
        self.profile = Some(id);
        self
    }

    /// The most output waiting for the client before sends are refused with
    /// [`WsSendError::Full`] and reads stop; by default the profile's
    /// `max_pending_output`.
    pub fn max_pending_output(mut self, bytes: usize) -> Self {
        self.max_pending = Some(bytes);
        self
    }

    /// How long a quiet connection waits before serve-net pings it; a second
    /// interval with no byte from the client closes it. At least 1 s.
    pub fn ping_interval(mut self, interval: Duration) -> Self {
        self.ping_interval = interval.max(Duration::from_secs(1));
        self
    }

    /// How long one message may take from its first byte to its last; past
    /// it the connection closes with 1008. A client that trickles a message
    /// would otherwise hold up to `max_message` of buffer for as long as it
    /// likes. At least 1 s; 30 s by default.
    pub fn message_timeout(mut self, timeout: Duration) -> Self {
        self.message_timeout = timeout.max(Duration::from_secs(1));
        self
    }

    /// Which web pages may open the connection, checked against the
    /// request's `Origin` before the 101; one that may not is answered 403
    /// (`RefusalKind::CrossOrigin`) and nothing is switched. A browser sends
    /// its cookies and HTTP credentials with an upgrade from any site, so
    /// without this check any page could talk to the service as its visitor
    /// (cross-site WebSocket hijacking).
    ///
    /// The default is [`ws_net::OriginPolicy::same_origin`]: the page's host
    /// and port must be the request's authority (the trusted proxy's with
    /// `trust_host` on, else the `Host` field), and a request with no
    /// `Origin` (not a browser) is accepted. Use
    /// [`OriginPolicy::allow`](ws_net::OriginPolicy::allow) for pages on
    /// other origins, and [`any_origin`](ws_net::OriginPolicy::any_origin)
    /// only for an endpoint that uses no ambient credentials.
    ///
    /// The policy is shared by every clone of the target: build the target
    /// once and clone it per upgrade to keep an allowlist free of
    /// allocation.
    pub fn origins(mut self, policy: ws_net::OriginPolicy) -> Self {
        self.origins = Some(Arc::new(policy));
        self
    }
}

impl fmt::Debug for WebSocketTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebSocketTarget")
            .field("limits", &self.limits)
            .field("subprotocol", &self.subprotocol)
            .field("profile", &self.profile)
            .field("max_pending", &self.max_pending)
            .field("ping_interval", &self.ping_interval)
            .field("origins", &self.origins)
            .finish_non_exhaustive()
    }
}

/// One upgraded connection's WebSocket state, inside the machine.
pub(crate) struct WsLink {
    pub(crate) conn: ws_net::Connection,
    pub(crate) service: Arc<dyn WebSocketService>,
    pub(crate) max_pending: usize,
    pub(crate) interval: Duration,
    /// Input that arrived with the upgrade request, not yet taken by ws-net.
    pub(crate) backlog: Vec<u8>,
    /// The current message, copied out of ws-net so the service can send
    /// while it reads it: binary here, text in `text`.
    pub(crate) scratch: Vec<u8>,
    pub(crate) text: String,
    pub(crate) subprotocol: Option<&'static str>,
    pub(crate) last_input: Instant,
    /// The next exchange deadline: a ping, a ping unanswered, or the end of
    /// a close handshake. net-io clears the deadline on every move to the
    /// idle phase, so the machine sets it again after each.
    pub(crate) timer: Instant,
    /// A ping went out at this time and no byte came back since.
    pub(crate) ping_at: Option<Instant>,
    /// What `WsSender`s queued for this connection.
    pub(crate) outbox: Arc<Outbox>,
    /// A message (or frame) is part-way in since this time.
    pub(crate) partial_since: Option<Instant>,
    pub(crate) message_timeout: Duration,
    /// The close code of the handshake, for `closed`.
    pub(crate) code: Option<u16>,
    /// `opened` ran, so `closed` is owed.
    pub(crate) opened: bool,
    /// The connection ends once the output is flushed.
    pub(crate) finish: bool,
}

impl WsLink {
    pub(crate) fn new(
        target: &WebSocketTarget,
        max_pending: usize,
        now: Instant,
        outbox: Arc<Outbox>,
    ) -> WsLink {
        WsLink {
            conn: ws_net::Connection::server().with_limits(target.limits),
            service: Arc::clone(&target.service),
            max_pending,
            interval: target.ping_interval,
            backlog: Vec::new(),
            scratch: Vec::new(),
            text: String::new(),
            subprotocol: target.subprotocol,
            last_input: now,
            timer: now + target.ping_interval,
            ping_at: None,
            outbox,
            partial_since: None,
            message_timeout: target.message_timeout,
            code: None,
            opened: false,
            finish: false,
        }
    }
}

impl WsLink {
    /// The next deadline: `base`, or the end of the current message's time
    /// if that comes first.
    pub(crate) fn deadline(&self, base: Instant) -> Instant {
        self.partial_since
            .map_or(base, |since| base.min(since + self.message_timeout))
    }

    /// Empties the message copies, zeroing them first under a wiping
    /// profile, and keeps at most `retain` bytes of each.
    pub(crate) fn clear_message(&mut self, wipe: bool, retain: usize) {
        if wipe {
            crate::engine::wipe_bytes(&mut self.scratch);
            let mut text = core::mem::take(&mut self.text).into_bytes();
            crate::engine::wipe_bytes(&mut text);
            text.clear();
            self.text = String::from_utf8(text).unwrap_or_default();
        }
        self.scratch.clear();
        self.text.clear();
        if self.scratch.capacity() > retain {
            self.scratch = Vec::new();
        }
        if self.text.capacity() > retain {
            self.text = String::new();
        }
    }

    /// Lets go of the connection's buffers when it ends: the message copies,
    /// the backlog, and, under a wiping profile, ws-net's own input, message,
    /// and output buffers, each zeroed first.
    pub(crate) fn release(&mut self, wipe: bool) {
        self.clear_message(wipe, 0);
        if wipe {
            crate::engine::wipe_bytes(&mut self.backlog);
            self.conn.wipe();
        }
    }
}

/// Whether the target's origin policy lets this page connect: the head's
/// `Origin` against its authority. More than one `Origin` field is 400, as
/// ws-net's handshake check would answer; a disallowed one is 403.
pub(crate) fn origin_allowed(
    head: &crate::http::HttpHead<'_>,
    target: &WebSocketTarget,
) -> Result<(), EngineError> {
    let mut origins = head
        .headers()
        .filter(|(name, _)| name.eq_ignore_ascii_case("origin"));
    let origin = match (origins.next(), origins.next()) {
        (None, _) => None,
        (Some((_, v)), None) => Some(core::str::from_utf8(v).map_err(|_| EngineError {
            kind: RefusalKind::Malformed,
            status: 400,
        })?),
        (Some(_), Some(_)) => {
            return Err(EngineError {
                kind: RefusalKind::Malformed,
                status: 400,
            });
        }
    };
    let host = head.authority().unwrap_or("");
    let permits = match &target.origins {
        None => ws_net::OriginPolicy::same_origin().permits(origin, host),
        Some(policy) => policy.permits(origin, host),
    };
    if permits {
        Ok(())
    } else {
        Err(EngineError {
            kind: RefusalKind::CrossOrigin,
            status: 403,
        })
    }
}

/// Checks the handshake and writes the 101 through http-net, so the HTTP
/// engine knows the connection switched and hands over what followed the
/// request.
pub(crate) fn switch_http1(
    engine: &mut crate::engine::Http1,
    target: &WebSocketTarget,
    unix_secs: u64,
    out: &mut OutBuf,
) -> Result<(), EngineError> {
    let internal = EngineError {
        kind: RefusalKind::Internal,
        status: 500,
    };
    let malformed = EngineError {
        kind: RefusalKind::Malformed,
        status: 400,
    };
    let Some(head) = engine.head() else {
        return Err(internal);
    };
    // The service asked for an upgrade the client did not offer: its bug.
    if !head.is_upgrade() {
        return Err(internal);
    }
    let handshake = match ws_net::ServerHandshake::from_request(
        head.method_str(),
        head.target(),
        head.headers(),
    ) {
        Ok(h) => h,
        Err(ws_net::HandshakeError::MissingUpgrade) => return Err(internal),
        Err(_) => return Err(malformed),
    };
    if let Some(p) = target.subprotocol {
        if !handshake.protocols().any(|offered| offered == p) {
            return Err(internal);
        }
    }
    let mut fields: [(&str, &[u8]); 2] = [("", &[]); 2];
    fields[0] = ("sec-websocket-accept", handshake.accept_key().as_bytes());
    let mut n = 1;
    if let Some(p) = target.subprotocol {
        fields[1] = ("sec-websocket-protocol", p.as_bytes());
        n = 2;
    }
    let mut response =
        http_net::ResponseHeadRef::new(101, &fields[..n], http_net::OutgoingBody::Empty);
    response.upgrade = Some("websocket");
    let date = engine.dates.update(unix_secs);
    let start = out.len();
    if engine.conn.write_head(&response, Some(date), out).is_err() {
        out.bytes.truncate(start);
        return Err(internal);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::ConnectionId;
    use crate::reply::{PostFail, Waker};

    struct Quiet;
    impl WebSocketService for Quiet {
        fn message(&self, _e: &Envelope, _m: WsMessage<'_>, _c: &mut WebSocketConn<'_>) {}
    }
    impl Waker for Quiet {
        fn post(&self, _c: ConnectionId, _t: u32, _v: u64) -> Result<(), PostFail> {
            Ok(())
        }
    }

    fn link() -> WsLink {
        let target = WebSocketTarget::new(Quiet);
        let outbox = Arc::new(Outbox::new(
            Arc::new(Quiet),
            ConnectionId::new(1).unwrap(),
            1 << 16,
        ));
        let mut link = WsLink::new(&target, 1 << 16, Instant::now(), outbox);
        // A masked binary frame of three bytes, read in, and a send queued.
        let _ = link
            .conn
            .feed(&[0x82, 0x83, 1, 2, 3, 4, 7 ^ 1, 7 ^ 2, 7 ^ 3]);
        assert!(matches!(link.conn.poll(), Ok(ws_net::Event::Binary(_))));
        link.conn.send_text("secret").unwrap();
        link
    }

    #[test]
    fn test_release_wipes_ws_net_buffers_under_a_wiping_profile() {
        let mut link = link();
        assert!(!link.conn.output().is_empty());
        link.release(true);
        // ws-net's wipe empties its output and ends the connection.
        assert!(link.conn.output().is_empty());
        assert_eq!(link.conn.state(), ws_net::State::Closed);
    }

    #[test]
    fn test_release_without_wipe_leaves_ws_net_alone() {
        let mut link = link();
        link.release(false);
        assert!(!link.conn.output().is_empty());
        assert_eq!(link.conn.state(), ws_net::State::Open);
    }
}
