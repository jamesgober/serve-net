//! # serve_net
//!
//! The network service engine for Rust: listeners through net-io, a per-connection engine with a slot for any protocol engine, the connection gate wired in, every ingress mode (direct, reverse proxy, Unix socket, FastCGI, CGI), drain, and a core for lifecycle.
//!
//! ## Status
//!
//! Scaffold. The design is written down in `dev/PLAN.md` and the milestone
//! schedule in `dev/ROADMAP.md` (both in the
//! [repository](https://github.com/jamesgober/serve-net)); the public API
//! begins with `0.2.0`.

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
    fn version_matches_manifest() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        assert_eq!(VERSION.split('.').count(), 3, "expected MAJOR.MINOR.PATCH");
    }
}
