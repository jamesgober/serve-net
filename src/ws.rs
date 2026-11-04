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
}

impl WebSocketConn<'_> {
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

/// Where a WebSocket upgrade goes: the service, ws-net's budgets, and the
/// subprotocol.
#[derive(Clone)]
#[must_use]
pub struct WebSocketTarget {
    pub(crate) service: Arc<dyn WebSocketService>,
    pub(crate) limits: ws_net::Limits,
    pub(crate) subprotocol: Option<&'static str>,
    pub(crate) profile: Option<ProfileId>,
    pub(crate) max_pending: Option<usize>,
    pub(crate) ping_interval: Duration,
}

impl WebSocketTarget {
    /// A target for `service`, with 64 KiB frames, 1 MiB messages, no
    /// subprotocol, the connection's profile, and a ping after 30 s of quiet.
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
}

impl fmt::Debug for WebSocketTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebSocketTarget")
            .field("limits", &self.limits)
            .field("subprotocol", &self.subprotocol)
            .field("profile", &self.profile)
            .field("max_pending", &self.max_pending)
            .field("ping_interval", &self.ping_interval)
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
    /// The close code of the handshake, for `closed`.
    pub(crate) code: Option<u16>,
    /// `opened` ran, so `closed` is owed.
    pub(crate) opened: bool,
    /// The connection ends once the output is flushed.
    pub(crate) finish: bool,
}

impl WsLink {
    pub(crate) fn new(target: &WebSocketTarget, max_pending: usize, now: Instant) -> WsLink {
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
            code: None,
            opened: false,
            finish: false,
        }
    }
}

impl WsLink {
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
