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
