//! What a service decides at the head, what it returns from a call, and how
//! a request ended.

use std::time::{Duration, Instant};

use crate::engine::RefusalKind;
use crate::envelope::Envelope;

/// How a request body is read.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyPolicy {
    /// Read the whole body before the service call, up to `max` bytes (and
    /// the engine's own limit); a declared length over it is refused with 413
    /// before any body byte is read.
    Buffer {
        /// The largest body accepted.
        max: u64,
    },
    /// Read and drop up to `max` bytes so the connection stays usable; beyond
    /// it, close after the reply.
    Discard {
        /// The most bytes read and dropped.
        max: u64,
    },
    /// Call the service once per piece of the body as it arrives, with reads
    /// paused during each call. The service returns [`Outcome::More`] until
    /// the last piece; answering earlier drops the rest of the body and
    /// closes the connection. The engine's body limit, the request deadline,
    /// and the minimum rate still apply.
    Stream,
}

/// What `head` decided.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum HeadAction {
    /// Continue with the profile's body policy.
    Proceed,
    /// Continue with this body policy.
    ProceedWith(BodyPolicy),
    /// Answer with the engine's refusal for `status` (400 to 599) and close.
    Refuse {
        /// The status; outside 400 to 599 it becomes 500.
        status: u16,
        /// `Retry-After`, when given.
        retry_after: Option<Duration>,
    },
    /// Switch the connection to another protocol after this request. The
    /// handshake is checked first: an invalid one is refused with 400, a
    /// request that offered no such upgrade with 500 (a service bug).
    /// Admission and the checkpoints apply to the upgrade request as to any
    /// other.
    Upgrade(UpgradeTarget),
}

impl HeadAction {
    /// `Refuse { status, retry_after: None }`.
    #[must_use]
    pub const fn refuse(status: u16) -> Self {
        HeadAction::Refuse {
            status,
            retry_after: None,
        }
    }
}

/// The protocol a connection switches to.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum UpgradeTarget {
    /// A WebSocket over ws-net (feature `ws`).
    #[cfg(feature = "ws")]
    WebSocket(crate::ws::WebSocketTarget),
}

#[cfg(feature = "ws")]
impl From<crate::ws::WebSocketTarget> for UpgradeTarget {
    fn from(target: crate::ws::WebSocketTarget) -> Self {
        UpgradeTarget::WebSocket(target)
    }
}

/// The connection after a switch, as `upgraded` sees it.
#[derive(Debug)]
pub struct UpgradedConn {
    pub(crate) env: Envelope,
    pub(crate) protocol: &'static str,
    pub(crate) subprotocol: Option<&'static str>,
}

impl UpgradedConn {
    /// The upgrade request's envelope; it stays the connection's envelope.
    #[must_use]
    pub fn envelope(&self) -> &Envelope {
        &self.env
    }

    /// The protocol switched to (`"websocket"`).
    #[must_use]
    pub fn protocol(&self) -> &'static str {
        self.protocol
    }

    /// The subprotocol the 101 named, if any.
    #[must_use]
    pub fn subprotocol(&self) -> Option<&'static str> {
        self.subprotocol
    }
}

/// What a service call returned.
#[non_exhaustive]
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The reply is complete.
    Done,
    /// The service called `reply.defer()` and completes the reply later.
    Deferred,
    /// Send what the reply holds and close the connection.
    Close,
    /// Call the service again: with the next piece of the body under
    /// [`BodyPolicy::Stream`], or, with a streamed reply open, when the client
    /// has taken what was sent. Anywhere else it is a service bug, answered
    /// with the 500 refusal.
    More,
}

/// How a service behaves, for dispatchers that move slow work off the worker.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WorkClass {
    /// Microseconds, never blocks.
    #[default]
    Quick,
    /// Waits on something: a database, a file, a network call.
    Io,
    /// Long computation: password hashing, image work.
    Cpu,
}

/// How a request ended.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestEnd {
    /// The reply was handed to the transport.
    Replied,
    /// serve-net or the service refused it.
    Refused(RefusalKind),
    /// Admission refused it.
    Shed,
    /// `Checkpoint` denied or killed it.
    Denied,
    /// A deadline passed.
    TimedOut,
    /// The peer left before the reply.
    PeerGone,
    /// The server drained before it finished.
    Drained,
    /// The connection was killed.
    Killed,
    /// The service panicked.
    ServicePanic,
    /// A deferred reply's token was dropped without completing it.
    Dropped,
    /// The switch response (101) was handed to the transport; the connection
    /// now speaks another protocol.
    Upgraded,
}

/// What happened to a request, given to `done` and the observer.
#[non_exhaustive]
#[derive(Clone, Copy, Debug)]
pub struct Finished {
    /// Reads it took to complete the head: a browser needs one or two, a
    /// slowloris tool many.
    pub head_reads: u16,
    /// The status written, if any.
    pub status: Option<u16>,
    /// How it ended.
    pub end: RequestEnd,
    /// Head and body bytes received.
    pub bytes_in: u64,
    /// Reply bytes handed to the transport.
    pub bytes_out: u64,
    /// When the first head byte arrived.
    pub head_at: Instant,
    /// When admission admitted it.
    pub admitted_at: Option<Instant>,
    /// When the reply was handed to the transport.
    pub replied_at: Option<Instant>,
}
