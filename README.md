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
        <strong>serve-net</strong> is the engine a network service runs on. It takes listeners from <a href="https://crates.io/crates/net-io">net-io</a>, learns the real client behind any front end (a reverse proxy with PROXY protocol or forwarded headers, a Unix socket, FastCGI), runs a sans-IO protocol engine over each connection, and hands one normalized request at a time to your service. It owns what sits between the socket and the service: request deadlines, keep-alive, limits, admission under load, drain, restarts without dropped connections, and readiness.
    </p>
    <p>
        The protocol engine is a slot. For the web it holds <a href="https://crates.io/crates/http-net">http-net</a> (with ws-net after an upgrade and fcgi-net behind a FastCGI front end); for a database it holds that database's own wire format. One engine, one set of limits and hooks, for every kind of service. No async runtime and no third-party dependencies.
    </p>
    <br>
    <hr>
    <p>
        <strong>MSRV is 1.85+</strong> (Rust 2024 edition). <code>#![forbid(unsafe_code)]</code>.
    </p>
    <blockquote>
        <strong>Status: design lock (<code>0.2.0</code>).</strong> No functional code yet. The public API is written and locked in <a href="./docs/API.md"><code>docs/API.md</code></a>; the design and every decision behind it are in <a href="./dev/PLAN.md"><code>dev/PLAN.md</code></a>, and the schedule in <a href="./dev/ROADMAP.md"><code>dev/ROADMAP.md</code></a>. <code>0.3.0</code> is the first working server; it waits for net-io <code>0.3.0</code>.
    </blockquote>
</div>

<hr>
<br>

## What it will look like

The API below is locked for `0.3.0`; it does not exist in `0.2.0`.

```rust
fn main() -> Result<(), serve_net::ServeError> {
    // A web server with every default limit and defence applied.
    let server = serve_net::http("127.0.0.1:8080", |req, reply| {
        reply.text(200, &format!("hello from {}", req.path()));
    })?;
    server.wait(); // runs until a drain is requested
    Ok(())
}
```

Behind Apache or nginx on the same host, with the real client taken from `X-Forwarded-For` only when the connection comes from the proxy:

```rust
let server = serve_net::http_behind_proxy("127.0.0.1:8080", "127.0.0.1, ::1", |req, reply| {
    reply.text(200, &format!("you are {:?}", req.client().addr()));
})?;
```

Several listeners, profiles, limits, a connection gate, a control line, and custom protocols go through `Server::builder()`.

<hr>
<br>

## Scope

- **Listeners** over net-io: TCP and Unix sockets, inherited from systemd, passed by mod_fcgid on descriptor 0, or handed over by a supervisor for restarts without a closed port.
- **Ingress**: direct (plain or TLS through net-io's seam), behind a reverse proxy (PROXY protocol v1 and v2, or trusted `Forwarded` / `X-Forwarded-For` with a right-to-left walk and a hop limit), over a Unix socket, or as a FastCGI responder. Every HTTP ingress gives the service the same request and the same client facts.
- **Per connection**: head and request deadlines, minimum transfer rates, keep-alive with request and age limits, pipelining in order, generic refusals that never name the reason, the switch to WebSocket after an upgrade (`h2c` is never honoured).
- **Admission**: soft connection caps answered with `503` and a jittered `Retry-After`, an in-flight cap per worker and per profile after the head is complete, priority lanes with reserved capacity, and a process-wide control line (shed, lockdown, drain, stop).
- **Hooks**: admission, dispatch (offload slow work), revocation checkpoints, observation, readiness (systemd `sd_notify` in safe code), plus net-io's gate and event sink.
- **Start-up safety**: every configuration error reported at once; a proxy or FastCGI listener on a public address, or a trust list that is too broad, is refused unless the override names its reason.

It is not a web framework (no routing, middleware, or templates), not a transport (net-io is), not a protocol parser (the engines are), and not a TLS implementation (tls-net is). There is no CGI mode: services run as persistent processes, and FastCGI covers shared hosting.

See [`dev/PLAN.md`](./dev/PLAN.md) for the full scope, what is left to other crates, and the decision log.

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
