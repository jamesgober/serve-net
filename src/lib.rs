//! # serve_net
//!
//! The network service engine for Rust: listeners over net-io, ingress from
//! direct clients, reverse proxies (PROXY protocol or trusted forwarded
//! headers), Unix sockets, and FastCGI, a per-connection engine with a slot
//! for any sans-IO protocol engine, admission, drain, and restarts without
//! dropped connections.
//!
//! ## Status
//!
//! `0.5.0`: HTTP/1.1 over net-io's reactor (or its blocking pool), directly,
//! over TLS, behind a reverse proxy, or on a Unix socket, with streaming bodies,
//! an offload pool, live reload, and restarts that drop nothing. The public API is locked in `docs/API.md`, and the design behind
//! it in `dev/PLAN.md` (both in the
//! [repository](https://github.com/jamesgober/serve-net)).
//!
//! ```no_run
//! # #[cfg(feature = "http1")]
//! # fn main() -> Result<(), serve_net::ServeError> {
//! let server = serve_net::http("127.0.0.1:8080", |req, reply| {
//!     reply.text(200, &format!("hello from {}", req.path()));
//! })?;
//! server.wait();
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "http1"))]
//! # fn main() {}
//! ```

#![doc(html_root_url = "https://docs.rs/serve-net")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
// Without the HTTP family the forwarded-header walk and the HTTP head context
// have no reader: a custom family brings its own.
#![cfg_attr(not(feature = "http1"), allow(dead_code))]

mod client;
mod config;
mod conn;
pub mod engine;
mod envelope;
mod error;
mod forwarded;
mod health;
mod hooks;
#[cfg(feature = "http1")]
mod http;
#[cfg(all(serve_net_loom, test, feature = "http1"))]
mod loom_tests;
mod net;
mod notify;
mod reload;
mod reply;
mod running;
mod server;
mod service;
mod snapshot;
mod stats;
mod sync;
#[cfg(feature = "http1")]
pub mod testing;
#[cfg(feature = "ws")]
mod ws;

pub use client::{ClientInfo, IngressKind, Scheme};
pub use config::{
    Budget, EngineChoice, ForwardedSource, Ingress, KeepAlive, Limits, ListenerSource,
    ListenerSpec, MissingHeader, Profile, ProfileId, ProfileTable, ProxySource, UnixMode,
    UnixPeerTrust, UnknownClient, Workers,
};
pub use envelope::{Envelope, RequestSeq};
pub use error::{ConfigError, ConfigErrors, ConfigWarning, EndReason, RuleId, ServeError, Subject};
pub use health::{Health, HealthIssue, ListenerInfo, ListenerState};
pub use hooks::{
    Admit, Admitted, Checkpoint, CheckpointAt, DefaultHooks, Dispatch, HookSet, Hooks, InFlightCap,
    Inline, NoCheckpoint, NoObserver, Observer, Offload, OffloadJob, OffloadPool, Shed, Verdict,
};
#[cfg(feature = "http1")]
pub use http::{
    DetachedHead, Fields, Handler, HttpBody, HttpFamily, HttpHead, HttpParts, HttpReply,
    HttpRequest, HttpService, ReplyStream, handler, http, http_behind_proxy,
};
#[cfg(feature = "http1")]
pub use http_net::{Method, Version};
pub use net::{
    AddrError, AllowAll, Cidr, CloseReason, ConnectionId, ControlLine, ControlState, DriverKind,
    EventSink, Gate, Lane, NoControl, NoopSink, ProxyVersions, SimpleControl, TrustedProxies,
    WorkerId,
};
pub use notify::{NoNotify, Notify, SystemdNotify, WatchdogPinger};
pub use reload::{ReloadSummary, ServerConfig};
pub use reply::{CompleteError, Reply, ReplyToken};
pub use running::{Bound, DrainHandle, DrainReport, Running};
pub use server::{Built, Server, ServerBuilder};
pub use service::{
    BodyPolicy, Finished, HeadAction, Outcome, RequestEnd, UpgradeTarget, UpgradedConn, WorkClass,
};
pub use snapshot::{Snapshot, SnapshotReader};
pub use stats::Stats;
#[cfg(feature = "ws")]
pub use ws::{WebSocketConn, WebSocketService, WebSocketTarget, WsMessage, WsSendError};

/// http-net, for its engine limits and types.
#[cfg(feature = "http1")]
pub use http_net;
/// net-io, for everything serve-net does not re-export by name.
pub use net_io;
/// ws-net, for its budgets and close codes.
#[cfg(feature = "ws")]
pub use ws_net;

/// Crate version string, populated by Cargo at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn test_version_matches_manifest_three_parts() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        assert_eq!(VERSION.split('.').count(), 3, "expected MAJOR.MINOR.PATCH");
    }
}
