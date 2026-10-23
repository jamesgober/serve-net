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
//! Design lock (`0.2.0`). The public API is written and locked in
//! `docs/API.md`, and the design behind it in `dev/PLAN.md` (both in the
//! [repository](https://github.com/jamesgober/serve-net)). `0.3.0` is the
//! first working server; it waits for net-io `0.3.0`. This release holds only
//! [`VERSION`].

#![doc(html_root_url = "https://docs.rs/serve-net")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
// Without the HTTP family nothing drives the connection machine until the
// net-io session does (dev/ROADMAP.md, v0.3.0, wire-up items).
#![cfg_attr(not(feature = "http1"), allow(dead_code))]

mod client;
mod config;
mod conn;
pub mod engine;
mod envelope;
mod error;
mod forwarded;
mod hooks;
#[cfg(feature = "http1")]
mod http;
mod net;
mod reply;
mod server;
mod service;
mod snapshot;
#[cfg(feature = "http1")]
pub mod testing;

pub use client::{ClientInfo, IngressKind, Scheme};
pub use config::{
    Budget, EngineChoice, ForwardedSource, Ingress, KeepAlive, Limits, ListenerSource,
    ListenerSpec, MissingHeader, Profile, ProfileId, ProfileTable, ProxySource, UnknownClient,
    Workers,
};
pub use envelope::{Envelope, RequestSeq};
pub use error::{ConfigError, ConfigErrors, ConfigWarning, EndReason, RuleId, ServeError, Subject};
pub use hooks::{
    Admit, Admitted, Checkpoint, CheckpointAt, DefaultHooks, Dispatch, HookSet, Hooks, InFlightCap,
    Inline, NoCheckpoint, NoObserver, Observer, Shed, Verdict,
};
#[cfg(feature = "http1")]
pub use http::{
    Fields, Handler, HttpBody, HttpFamily, HttpHead, HttpParts, HttpReply, HttpRequest,
    HttpService, handler,
};
#[cfg(feature = "http1")]
pub use http_net::{Method, Version};
pub use net::{
    AddrError, Cidr, ConnectionId, ControlLine, ControlState, DriverKind, Lane, NoControl,
    ProxyVersions, SimpleControl, TrustedProxies, WorkerId,
};
pub use reply::{CompleteError, Reply, ReplyToken};
pub use server::{Built, Server, ServerBuilder};
pub use service::{BodyPolicy, Finished, HeadAction, Outcome, RequestEnd, WorkClass};
pub use snapshot::{Snapshot, SnapshotReader};

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
