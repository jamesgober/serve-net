<h1 align="center">
    <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br>
    <b>serve-net</b>
    <br>
    <sub><sup>THE SERVICE ENGINE</sup></sub>
</h1>

<div align="center">
    <a href="https://crates.io/crates/serve-net"><img alt="Crates.io" src="https://img.shields.io/crates/v/serve-net"></a>
    <a href="https://crates.io/crates/serve-net"><img alt="Downloads" src="https://img.shields.io/crates/d/serve-net?color=%230099ff"></a>
    <a href="https://docs.rs/serve-net"><img alt="docs.rs" src="https://img.shields.io/docsrs/serve-net"></a>
    <a href="https://github.com/jamesgober/serve-net/actions"><img alt="CI" src="https://github.com/jamesgober/serve-net/actions/workflows/ci.yml/badge.svg"></a>
    <a href="https://github.com/rust-lang/rfcs/blob/master/text/2495-min-rust-version.md"><img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.85%2B-blue"></a>
</div>

<br>

<div align="left">
    <p>
        <strong>serve-net</strong> is the engine a network service runs on. It binds listeners through net-io, wires the connection gate (bouncer-io) into every accept and every write, runs a protocol engine over each connection, and hands complete requests to the application. Its core owns the lifecycle of everything inside it: start, health, drain, and shutdown.
    </p>
    <p>
        The protocol engine is a slot. For the web it holds http-net (with ws-net after an upgrade, fcgi-net behind a FastCGI front end, or CGI); for a database it holds that database's own wire format. One engine, one set of limits and security hooks, for every kind of service.
    </p>
    <br>
    <hr>
    <p>
        <strong>MSRV is 1.85+</strong> (Rust 2024 edition). <code>#![forbid(unsafe_code)]</code>.
    </p>
    <blockquote>
        <strong>Status: scaffold (<code>0.1.0</code>).</strong> No functional code yet. The design is written down in <a href="./dev/PLAN.md"><code>dev/PLAN.md</code></a> and the schedule in <a href="./dev/ROADMAP.md"><code>dev/ROADMAP.md</code></a>; the public API is locked in <code>0.2.0</code>.
    </blockquote>
</div>

<hr>
<br>

## Scope

Listeners and workers over net-io, the per-connection loop, the protocol engine slot, ingress normalization for the web (direct, reverse proxy over TCP or a Unix socket, FastCGI, CGI) into one request type, the security pipeline order (bouncer, customs, gate-net, gate-sec) with checkpoints, responses through the egress check, keep-alive, limits, graceful drain, and a core that registers with the system core. It is not the web framework (web-net) and does no routing.

See [`dev/PLAN.md`](./dev/PLAN.md) for the full scope, what is deliberately left to other crates, and the decision log.

<hr>
<br>

## Contributing

See <a href="./dev/DIRECTIVES.md"><code>dev/DIRECTIVES.md</code></a> for engineering standards and the definition of done. Before a PR: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-features` must be clean.

<br>

<div id="license">
    <h2>License</h2>
    <p>Licensed under either of</p>
    <ul>
        <li><b>Apache License, Version 2.0</b> &mdash; <a href="./LICENSE-APACHE">LICENSE-APACHE</a></li>
        <li><b>MIT License</b> &mdash; <a href="./LICENSE-MIT">LICENSE-MIT</a></li>
    </ul>
    <p>at your option.</p>
</div>

<div align="center">
  <h2></h2>
  <sup>COPYRIGHT <small>&copy;</small> 2026 <strong>James Gober.</strong></sup>
</div>
