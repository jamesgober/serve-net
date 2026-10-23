# serve-net - API Reference

> Reference for every public item in `serve-net`, with signatures, behaviour, errors, costs, and examples.
> **Current release: `0.2.0` (design lock; no functional code yet).** This document is the contract for `0.3.0` to `1.0.0`. Each item carries the version that ships it; an item marked with a later version than the current release is specified here and not yet in the crate. Names and signatures do not change after `0.2.0` without a decision-log entry in [`../dev/PLAN.md`](../dev/PLAN.md).

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>

## Table of contents

- [Conventions](#conventions)
  - [Grades, tiers, versions](#grades-tiers-versions)
  - [Costs](#costs)
  - [Errors and panics](#errors-and-panics)
  - [Feature flags](#feature-flags)
  - [Platforms](#platforms)
- [Tier 1 at a glance](#tier-1-at-a-glance)
- [Crate root](#crate-root)
  - [`VERSION`](#version)
  - [`http`](#http)
  - [`http_behind_proxy`](#http_behind_proxy)
  - [`fastcgi`, `FastCgiListen`](#fastcgi-fastcgilisten)
  - [`handler`, `Handler`](#handler-handler)
  - [Re-exports](#re-exports)
- [Building and running](#building-and-running)
  - [`Server`](#server)
  - [`ServerBuilder`: configuration](#serverbuilder-configuration)
  - [`ServerBuilder`: hooks](#serverbuilder-hooks)
  - [`ServerBuilder::build`, `Built`, `ConfigWarning`](#serverbuilderbuild-built-configwarning)
  - [`Built::bind`, `Bound`](#builtbind-bound)
  - [`Bound::open`, `ServerBuilder::serve`](#boundopen-serverbuilderserve)
  - [`Running`](#running)
  - [`Running::close_door`](#runningclose_door)
  - [`Running::drain`, `Running::wait`, `DrainHandle`](#runningdrain-runningwait-drainhandle)
  - [`DrainReport`](#drainreport)
  - [`ListenerInfo`, `ListenerState`](#listenerinfo-listenerstate)
  - [`ServerConfig`, `Running::reload`, `ReloadSummary`](#serverconfig-runningreload-reloadsummary)
- [Listeners](#listeners)
  - [`ListenerSpec`: sources](#listenerspec-sources)
  - [`ListenerSpec`: options](#listenerspec-options)
  - [`ListenerSource`](#listenersource)
  - [`UnixMode`](#unixmode)
- [Ingress](#ingress)
  - [`Ingress`](#ingress-1)
  - [`ProxySource`](#proxysource)
  - [`ForwardedSource`](#forwardedsource)
  - [`MissingHeader`, `UnknownClient`](#missingheader-unknownclient)
  - [`UnixPeerTrust`](#unixpeertrust)
  - [`FastCgiIngress`](#fastcgiingress)
- [Clients and envelopes](#clients-and-envelopes)
  - [`ClientInfo`](#clientinfo)
  - [`Scheme`](#scheme)
  - [`IngressKind`](#ingresskind)
  - [`Envelope`](#envelope)
  - [`RequestSeq`](#requestseq)
- [Profiles](#profiles)
  - [`Profile`](#profile)
  - [`ProfileId`](#profileid)
  - [`Budget`](#budget)
  - [`ProfileTable`](#profiletable)
  - [`EngineChoice`](#enginechoice)
- [The HTTP family](#the-http-family)
  - [`HttpService`](#httpservice)
  - [`HttpHead`, `Fields`](#httphead-fields)
  - [`HeadAction`](#headaction)
  - [`BodyPolicy`](#bodypolicy)
  - [`HttpRequest`, `HttpBody`](#httprequest-httpbody)
  - [`HttpReply`](#httpreply)
  - [`Outcome`](#outcome)
  - [`ReplyToken`, `CompleteError`](#replytoken-completeerror)
  - [`Finished`, `RequestEnd`](#finished-requestend)
  - [`WorkClass`](#workclass)
  - [`DetachedHead`](#detachedhead)
  - [Upgrades: `UpgradeTarget`, `UpgradedConn`](#upgrades-upgradetarget-upgradedconn)
- [Limits, keep-alive, workers](#limits-keep-alive-workers)
  - [`Limits`](#limits)
  - [`KeepAlive`](#keepalive)
  - [`Workers`](#workers)
  - [Defaults and reasons](#defaults-and-reasons)
- [Hooks](#hooks)
  - [`Hooks`, `HookSet`, `DefaultHooks`](#hooks-hookset-defaulthooks)
  - [`Admit`, `Admitted`, `Shed`](#admit-admitted-shed)
  - [`InFlightCap`](#inflightcap)
  - [`Dispatch`, `Inline`](#dispatch-inline)
  - [`Offload`, `OffloadPool`, `OffloadJob`](#offload-offloadpool-offloadjob)
  - [`Checkpoint`, `CheckpointAt`, `Verdict`, `NoCheckpoint`](#checkpoint-checkpointat-verdict-nocheckpoint)
  - [`Observer`, `NoObserver`](#observer-noobserver)
  - [`Notify`, `NoNotify`](#notify-nonotify)
  - [`SystemdNotify`](#systemdnotify)
  - [`WatchdogPinger`](#watchdogpinger)
- [The control line](#the-control-line)
- [Health and statistics](#health-and-statistics)
  - [`Health`, `HealthIssue`](#health-healthissue)
  - [`Stats`](#stats)
- [Errors](#errors)
  - [`ServeError`](#serveerror)
  - [`ConfigErrors`, `ConfigError`](#configerrors-configerror)
  - [`RuleId`](#ruleid)
  - [`Subject`](#subject)
  - [`EndReason`](#endreason)
- [Engines and families (Tier 3)](#engines-and-families-tier-3)
  - [`Engine`, `EngineEvent`, `EngineProgress`](#engine-engineevent-engineprogress)
  - [`EngineError`, `Refusal`, `RefusalKind`](#engineerror-refusal-refusalkind)
  - [`OutBuf`](#outbuf)
  - [`Family`, `HttpFamily`](#family-httpfamily)
  - [`Service`](#service)
  - [`NoCustom`](#nocustom)
  - [`engine::Http1`](#enginehttp1)
  - [`engine::FastCgi`](#enginefastcgi)
  - [`engine::WebSocket`](#enginewebsocket)
  - [`engine::H2`](#engineh2)
- [Snapshots](#snapshots)
  - [`Snapshot`](#snapshot)
  - [`SnapshotReader`](#snapshotreader)
- [Testing services](#testing-services)
- [Thread safety and sizes](#thread-safety-and-sizes)
- [Recipes](#recipes)
- [Names changed at the design lock](#names-changed-at-the-design-lock)

---

## Conventions

### Grades, tiers, versions

Every item carries three marks:

| Mark | Values |
|---|---|
| **Grade** | **P0** required (the job cannot be done without it), **P1** expected (what users of the best servers of this kind expect), **P2** high value (enterprise, high traffic, security), **P3** nice to have (after 1.0 is acceptable) |
| **Tier** | **1**: free functions for about 80% of uses, no builder and no type parameter to name. **2**: builders and configuration for the next 15%. **3**: the full surface for the last 5% and for other libraries |
| **Version** | the release that ships the item. Everything graded P0, P1, or P2 ships by `1.0.0` |

### Costs

Every cost is a target on reference hardware. Numbers marked **[est.]** are estimates that a benchmark replaces at `0.6.0`; `docs/PERFORMANCE.md` (from `0.6.0`) holds the measured numbers with method and hardware. Costs exclude net-io's own work (system calls, its timers and meters) and the protocol engine's parse and encode, which their crates document.

### Errors and panics

- Start-up and run-time operations return `Result<_, ServeError>`; `build` returns `Result<_, ConfigErrors>` with every rule that failed, not only the first.
- Every error enum and every extensible struct is `#[non_exhaustive]`. Match with a wildcard arm.
- Every error type implements `Display` (what happened and what to do) and `std::error::Error`.
- No error, event, or `Debug` output carries request bytes or header values; they name the rule, the listener, or the profile.
- Library code never panics. A service that panics is handled as [`catch_service_panics`](#serverbuilder-configuration) says: caught in `panic = "unwind"` builds when that option is on, otherwise the connection is closed by net-io (unwind builds) or the process aborts (abort builds, where the supervisor restarts it).
- Every public type is `Send` and `Sync` unless its section says otherwise, checked by `const` assertions in the crate.

### Feature flags

| Feature | Default | Contents | Version |
|---|---|---|---|
| (none) | always | the server, listeners, ingress, profiles, limits, hooks, snapshots, the engine and family traits | `0.3.0` |
| `http1` | on | the HTTP family over http-net: `HttpService`, `HttpRequest`, `HttpReply`, `engine::Http1`, the Tier 1 functions `http` and `http_behind_proxy`, `testing` | `0.3.0` |
| `fastcgi` | off | FastCGI over fcgi-net: `engine::FastCgi`, `fastcgi`, `Ingress::FastCgi` (implies the HTTP family types) | `0.4.0` |
| `ws` | off | WebSocket upgrades over ws-net: `engine::WebSocket`, `UpgradeTarget::WebSocket` (implies `http1`) | `0.4.0` |
| `h2` | off | HTTP/2 over h2-net: `engine::H2`, ALPN `h2` | after `1.0.0`, or earlier as a minor version when h2-net reaches its 0.3 |

Features are additive. `--no-default-features` builds the server with custom families only and passes CI on its own. The crate has no third-party runtime dependency in any feature combination; its first-party dependencies are net-io (always), http-net, fcgi-net, ws-net, and h2-net (by feature), each at a published version.

### Platforms

serve-net reaches the operating system only through net-io, so its platform support is net-io's: Linux, macOS, and Windows are first class on stable and Rust 1.85; FreeBSD joins CI with net-io's reactor. Items that exist on one platform family carry `#[cfg(unix)]` or `#[cfg(windows)]` in their signature, with the counterpart or the reason in their section.

---

## Tier 1 at a glance

```rust,no_run
fn main() -> Result<(), serve_net::ServeError> {
    // A web server on port 8080 with every default limit and defence applied.
    let server = serve_net::http("127.0.0.1:8080", |req, reply| {
        reply.text(200, &format!("hello from {}", req.path()));
    })?;

    // Run until something asks for a drain (a DrainHandle, or the control line).
    let report = server.wait();
    println!("{} connections closed gracefully, {} killed", report.graceful, report.killed);
    Ok(())
}
```

```rust,no_run
// Behind Apache (ProxyPass) or nginx (proxy_pass) on the same host: the real client
// comes from X-Forwarded-For, believed only when the connection comes from 127.0.0.1 or ::1.
let server = serve_net::http_behind_proxy("127.0.0.1:8080", "127.0.0.1, ::1", |req, reply| {
    let client = req.client().addr();
    reply.text(200, &format!("you are {client:?}"));
})?;
# Ok::<(), serve_net::ServeError>(())
```

| Want | Call |
|---|---|
| an HTTP server with safe defaults | [`serve_net::http(addr, handler)`](#http) |
| the same behind a reverse proxy on the same host | [`serve_net::http_behind_proxy(addr, trusted, handler)`](#http_behind_proxy) |
| a FastCGI application under Apache or nginx | [`serve_net::fastcgi(listen, handler)`](#fastcgi-fastcgilisten) |
| stop it | [`Running::drain(deadline)`](#runningdrain-runningwait-drainhandle), or [`DrainHandle::drain`](#runningdrain-runningwait-drainhandle) from another thread |
| the real client address | [`req.client().addr()`](#clientinfo) |
| test a handler without sockets | [`serve_net::testing::respond`](#testing-services) |

Everything else (several listeners, profiles, limits, hooks, TLS, PROXY, custom protocols) goes through [`Server::builder()`](#server) (Tier 2).

---

## Crate root

### `VERSION`

```rust
pub const VERSION: &str;
```

The crate version, populated by Cargo at build time. P0, Tier 1, `0.1.0`.

```rust
println!("serve-net {}", serve_net::VERSION);
```

### `http`

```rust
pub fn http<A, F>(addr: A, handler: F) -> Result<Running, ServeError>
where
    A: ToSocketAddrs,
    F: Fn(&HttpRequest<'_>, &mut HttpReply) + Send + Sync + 'static;
```

Starts an HTTP/1.1 server and returns it running. One direct TCP listener per resolved address (named `http-0`, `http-1`, ...), one profile with the defaults of [`Limits::new()`](#limits) and [`KeepAlive::new()`](#keepalive), [`Workers::auto()`](#workers) (the reactor where net-io provides it on this platform, the blocking pool before that), the [default hooks](#hooks-hookset-defaulthooks), and net-io's `AllowAll` as the connection gate (no bans or rates; set a gate through `Server::builder()` for a server facing the internet). Plain HTTP; TLS needs Tier 2. Every request is buffered (body up to 1 MiB) before the handler runs, and the reply is buffered before it is written.

The handler is called once per request on a worker thread. It must not block for long: on the reactor driver a slow handler delays every connection of its worker (declare the work and use [`Offload`](#offload-offloadpool-offloadjob) for that, Tier 3).

| Grade | Tier | Version | Feature | Cost |
|---|---|---|---|---|
| P0 | 1 | `0.3.0` | `http1` | start-up only |

Errors: `ServeError::Bind { listener, source }` (the address is in use, needs privileges, or is not on this host; the message names the address and what to change), `ServeError::Config` with `RuleId::AddressResolve` (the address resolved to nothing).

```rust,no_run
let server = serve_net::http("127.0.0.1:8080", |req, reply| {
    match req.path() {
        "/" => { reply.html(200, "<h1>It works</h1>"); }
        _ => { reply.text(404, "not found"); }
    }
})?;
server.wait();
# Ok::<(), serve_net::ServeError>(())
```

```rust,no_run
// Port 0 picks a free port; local_addr says which.
let server = serve_net::http("127.0.0.1:0", |_req, reply| { reply.text(200, "ok"); })?;
println!("listening on {:?}", server.local_addr());
let report = server.drain(std::time::Duration::from_secs(5));
assert_eq!(report.killed, 0);
# Ok::<(), serve_net::ServeError>(())
```

### `http_behind_proxy`

```rust
pub fn http_behind_proxy<A, F>(addr: A, trusted: &str, handler: F) -> Result<Running, ServeError>
where
    A: ToSocketAddrs,
    F: Fn(&HttpRequest<'_>, &mut HttpReply) + Send + Sync + 'static;
```

As [`http`](#http), with [`Ingress::Proxy`](#ingress-1) and [`ProxySource::forwarded_for`](#proxysource): the real client comes from `X-Forwarded-For` (what Apache `mod_proxy_http` and the common nginx configuration send), walked right to left, believed only when the socket peer is in `trusted`. `trusted` is a list of addresses and CIDR blocks separated by commas or whitespace (`"127.0.0.1, ::1"`, `"10.0.0.0/8"`). A trusted peer that sends no `X-Forwarded-For` is refused with 400 ([`MissingHeader::Refuse`](#missingheader-unknownclient)); scheme and host are not taken from the proxy (use Tier 2 for `trust_scheme` and `trust_host`).

Validation applies in full: the listening address must be loopback or private, because anyone who can reach a trusting listener can forge `X-Forwarded-For` (rule [`PublicBind`](#ruleid)); the trust list must not be empty or broader than /8 (IPv4) or /32 (IPv6).

| Grade | Tier | Version | Feature | Cost |
|---|---|---|---|---|
| P0 | 1 | `0.3.0` | `http1` | per request: the trusted walk, under 100 ns for three entries [est.] |

Errors: as `http`, plus `ServeError::Config` with `RuleId::PublicBind`, `TrustParse`, `TrustEmpty`, `TrustTooBroad`, or `TrustWildcard`.

```rust,no_run
// Apache: ProxyPass "/" "http://127.0.0.1:8080/" and ProxyPassReverse.
let server = serve_net::http_behind_proxy("127.0.0.1:8080", "127.0.0.1", |req, reply| {
    reply.text(200, &format!("{:?} asked for {}", req.client().addr(), req.path()));
})?;
server.wait();
# Ok::<(), serve_net::ServeError>(())
```

```rust
// Refused at start-up: a trusting listener on every interface.
let err = serve_net::http_behind_proxy("0.0.0.0:8080", "127.0.0.1", |_req, reply| { reply.text(200, "x"); })
    .unwrap_err();
assert!(err.to_string().contains("public"));
```

### `fastcgi`, `FastCgiListen`

```rust
#[non_exhaustive]
pub enum FastCgiListen<'a> {
    DescriptorZero,           // the listening socket a process manager passes on descriptor 0 (mod_fcgid)
    Tcp(&'a str),             // "127.0.0.1:9000"
    Unix(&'a Path),           // "/run/app/fcgi.sock"
    Auto,                     // descriptor 0 when it is a listening socket, otherwise an error
}

pub fn fastcgi<F>(listen: FastCgiListen<'_>, handler: F) -> Result<Running, ServeError>
where
    F: Fn(&HttpRequest<'_>, &mut HttpReply) + Send + Sync + 'static;
```

A FastCGI responder for Apache (`mod_fcgid`, `mod_proxy_fcgi`) or nginx (`fastcgi_pass`). The handler sees the same [`HttpRequest`](#httprequest-httpbody) an HTTP listener produces: method, target, fields, and body from the FastCGI parameters, the client from `REMOTE_ADDR` and `REMOTE_PORT`, the scheme from `HTTPS` or `REQUEST_SCHEME`, the authority from `HTTP_HOST` or `SERVER_NAME` and `SERVER_PORT`. A TCP listener must be loopback or private and trusts only loopback peers; `FCGI_WEB_SERVER_ADDRS`, when set, further restricts peers. Under mod_fcgid a process receives one request at a time, so the worker count follows the expected concurrency (one per process by default) and `FCGI_GET_VALUES` answers `FCGI_MPXS_CONNS=0`.

| Grade | Tier | Version | Feature |
|---|---|---|---|
| P1 | 1 | `0.4.0` | `fastcgi` |

Errors: `ServeError::Inherited` ("descriptor 0 is not a listening socket: the process was not started by a FastCGI process manager; use FastCgiListen::Tcp or Unix"), `ServeError::Bind`, `ServeError::Config` (`PublicBind`). `DescriptorZero` and `Unix` are Unix only (mod_fcgid on Windows uses named pipes, after `1.0.0`).

```rust,no_run
use serve_net::FastCgiListen;
// Started by mod_fcgid: FcgidWrapper /usr/local/bin/app
let server = serve_net::fastcgi(FastCgiListen::Auto, |req, reply| {
    reply.html(200, &format!("<p>{}</p>", req.path()));
})?;
server.wait();
# Ok::<(), serve_net::ServeError>(())
```

### `handler`, `Handler`

```rust
pub fn handler<F>(f: F) -> Handler<F>
where
    F: Fn(&HttpRequest<'_>, &mut HttpReply) + Send + Sync + 'static;

pub struct Handler<F> { /* the closure */ }
impl<F> HttpService for Handler<F> where F: Fn(&HttpRequest<'_>, &mut HttpReply) + Send + Sync + 'static;
```

Turns a closure into an [`HttpService`](#httpservice) where a type is expected (`ServerBuilder::serve`, `testing::respond`). The function fixes the closure's argument types, so the closure needs no annotations. `call` runs the closure and returns `Outcome::Done`; `head` keeps the profile's body policy. P0, Tier 2, `0.3.0`, feature `http1`; no cost beyond the closure call.

```rust,no_run
use serve_net::{ListenerSpec, Server};
let server = Server::builder()
    .listener(ListenerSpec::tcp("web", "127.0.0.1:8080"))
    .serve(serve_net::handler(|req, reply| { reply.text(200, req.path()); }))?;
# drop(server);
# Ok::<(), serve_net::ServeError>(())
```

### Re-exports

serve-net names a few net-io and http-net types in its own signatures. They are re-exported so a program needs no direct dependency at a matching version, and the crates themselves are re-exported for everything else:

| Re-export | From | Used in |
|---|---|---|
| `Lane` | `net_io::Lane` | `Envelope::lane`, `Profile::lane`, `InFlightCap::reserve` |
| `ConnectionId`, `WorkerId` | net-io | `Envelope::connection`, `Envelope::worker`, `Admit` |
| `TrustedProxies`, `Cidr` | net-io | `ProxySource`, `FastCgiIngress` |
| `ControlLine`, `ControlState`, `SimpleControl`, `NoControl` | net-io | [the control line](#the-control-line) |
| `Gate`, `AllowAll`, `EventSink`, `NoopSink`, `CloseReason` | net-io | `ServerBuilder::gate`, `ServerBuilder::events`, `EndReason::Transport` |
| `Method`, `Version` | http-net (feature `http1`) | `HttpHead::method`, `HttpHead::version` |
| `net_io` | the crate | `ServerBuilder::transport_limits`, `ListenerSpec::listen_options`, hand-off |
| `http_net` | the crate (feature `http1`) | `Limits::engine_limits` |

---

## Building and running

### `Server`

```rust
pub struct Server;
impl Server {
    pub fn builder() -> ServerBuilder;
}
```

The entry point for Tier 2. `builder()` returns a builder with no listeners, no profiles (profile 0 is created with defaults when none is added), [default limits](#limits), [default workers](#workers), and [default hooks](#hooks-hookset-defaulthooks). P0, Tier 2, `0.3.0`; no cost.

```rust,no_run
use serve_net::{ListenerSpec, Server};
let server = Server::builder()
    .listener(ListenerSpec::tcp("web", "127.0.0.1:8080"))
    .serve(serve_net::handler(|_req, reply| { reply.text(200, "hello"); }))?;
server.wait();
# Ok::<(), serve_net::ServeError>(())
```

### `ServerBuilder`: configuration

```rust
#[must_use]
pub struct ServerBuilder<H: Hooks = DefaultHooks> { /* ... */ }

impl<H: Hooks> ServerBuilder<H> {
    pub fn listener(self, spec: ListenerSpec) -> Self;            // repeatable, at most 64
    pub fn profile(self, profile: Profile) -> Self;               // repeatable, at most 255
    pub fn limits(self, limits: Limits) -> Self;                  // server-wide; profiles may override
    pub fn keep_alive(self, policy: KeepAlive) -> Self;           // server-wide; profiles may override
    pub fn workers(self, workers: Workers) -> Self;
    pub fn drain_deadline(self, deadline: Duration) -> Self;      // default 30 s; used by wait() and reload
    pub fn accept_failure_window(self, window: Duration) -> Self; // default 30 s, then health Degraded
    pub fn catch_service_panics(self, on: bool) -> Self;          // P2, 0.5.0
    pub fn transport_limits(self, limits: net_io::Limits) -> Self;// net-io values serve-net does not map
    pub fn clock<K: net_io::Clock>(self, clock: Arc<K>) -> Self;  // tests: a net_io::ManualClock
    pub fn config(self, config: ServerConfig) -> Self;            // P1, 0.4.0: replaces all of the above
}
```

Pure setters: nothing is checked until [`build`](#serverbuilderbuild-built-configwarning), so every error is reported together. Two listeners with one name, or two profiles with one id, are reported by `build`.

| Item | Behaviour | Grade | Tier | Version |
|---|---|---|---|---|
| `listener`, `profile`, `limits`, `keep_alive`, `workers` | as above | P0 | 2 | `0.3.0` |
| `drain_deadline` | the deadline `wait` uses when a drain is requested without one, and the deadline for listeners removed by a reload; 1 ms to 1 h (rule `DrainRange`) | P0 | 2 | `0.3.0` |
| `accept_failure_window` | how long a listener may report accept errors before health is `Degraded` | P1 | 2 | `0.3.0` |
| `catch_service_panics` | wrap service calls in `catch_unwind`: a caught panic answers the 500 refusal, closes the connection, calls `done` with `RequestEnd::ServicePanic`, and is reported. Effective only with `panic = "unwind"`; with `abort`, `build` adds a warning | P2 | 2 | `0.5.0` |
| `transport_limits` | the base net-io limits for every listener; values serve-net maps (section [Defaults and reasons](#defaults-and-reasons)) are set from serve-net's own limits on top | P1 | 3 | `0.3.0` |
| `clock` | the clock net-io's deadlines run on; serve-net reads time from net-io, so a `ManualClock` makes every serve-net deadline deterministic too | P2 | 3 | `0.3.0` |
| `config` | replaces listeners, profiles, limits, keep-alive, workers, and the drain deadline with a `ServerConfig` (the same value [`reload`](#serverconfig-runningreload-reloadsummary) takes) | P1 | 2 | `0.4.0` |

```rust
use serve_net::{KeepAlive, Limits, ListenerSpec, Profile, ProfileId, Server, Workers};
use std::time::Duration;

let built = Server::builder()
    .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
    .profile(Profile::new(ProfileId(0)))
    .limits(Limits::new().head_timeout(Duration::from_secs(5)).max_body(256 * 1024))
    .keep_alive(KeepAlive::new().idle(Duration::from_secs(30)))
    .workers(Workers::blocking(64))
    .drain_deadline(Duration::from_secs(10))
    .build()?;
assert!(built.warnings().is_empty());
# Ok::<(), serve_net::ConfigErrors>(())
```

### `ServerBuilder`: hooks

```rust
// The builder carries a HookSet<A, D, K, O, G, E, C, T> (see Hooks); each setter replaces one slot.
impl<A, D, K, O, G, E, C, T> ServerBuilder<HookSet<A, D, K, O, G, E, C, T>> {
    pub fn gate<G2: net_io::Gate>(self, gate: G2) -> ServerBuilder<HookSet<A, D, K, O, G2, E, C, T>>;
    pub fn events<E2: net_io::EventSink>(self, sink: E2) -> ServerBuilder<HookSet<A, D, K, O, G, E2, C, T>>;
    pub fn control<C2: net_io::ControlLine>(self, line: Arc<C2>) -> ServerBuilder<HookSet<A, D, K, O, G, E, C2, T>>;
    pub fn tls<T2: net_io::TlsAcceptor>(self, acceptor: T2) -> ServerBuilder<HookSet<A, D, K, O, G, E, C, T2>>;  // P0, 0.4.0
    pub fn admit<A2: Admit>(self, admit: A2) -> ServerBuilder<HookSet<A2, D, K, O, G, E, C, T>>;
    pub fn dispatch<D2: Dispatch>(self, dispatch: D2) -> ServerBuilder<HookSet<A, D2, K, O, G, E, C, T>>;
    pub fn checkpoint<K2: Checkpoint>(self, checkpoint: K2) -> ServerBuilder<HookSet<A, D, K2, O, G, E, C, T>>;
    pub fn observer<O2: Observer>(self, observer: O2) -> ServerBuilder<HookSet<A, D, K, O2, G, E, C, T>>;
    pub fn notify<N: Notify>(self, notify: N) -> Self;                                                         // P1, 0.4.0
}
```

Each call replaces one hook. The builder's type changes with it, so every hot hook is a static call that inlines (a no-op default compiles to nothing); programs never name these types because they only chain calls. `notify` keeps the type: it is called a few times per process, through a box.

| Hook | Default | Called | Grade | Tier | Version |
|---|---|---|---|---|---|
| `gate` | `net_io::AllowAll` | by net-io at accept, after a PROXY header, before every write, at close | P0 | 3 | `0.3.0` |
| `events` | `net_io::NoopSink` | by net-io for every transport event; serve-net taps it for health | P0 | 3 | `0.3.0` |
| `control` | `net_io::NoControl` | by net-io per loop iteration, by serve-net at request checkpoints | P0 | 2 | `0.3.0` |
| `tls` | `net_io::NoTls` | by net-io per TLS connection | P0 | 2 | `0.4.0` |
| `admit` | [`InFlightCap`](#inflightcap) | after `head`, before any body byte | P0 | 3 | `0.3.0` |
| `dispatch` | [`Inline`](#dispatch-inline) | between "request complete" and the service call | P0 | 3 | `0.3.0` |
| `checkpoint` | [`NoCheckpoint`](#checkpoint-checkpointat-verdict-nocheckpoint) | after admission, before dispatch, before the reply | P0 | 3 | `0.3.0` |
| `observer` | [`NoObserver`](#observer-noobserver) | at request and connection events | P0 | 3 | `0.3.0` |
| `notify` | [`NoNotify`](#notify-nonotify) | ready, reloading, stopping | P1 | 3 | `0.4.0` |

Errors: none here; `build` checks what depends on hooks (a TLS listener without `tls` is `RuleId::TlsWithoutAcceptor`).

```rust,no_run
use serve_net::{ListenerSpec, Server, SimpleControl};
use std::sync::Arc;

// A gate (for example bouncer-io's) and a process-wide control line.
# struct MyGate;
# impl serve_net::Gate for MyGate {
#     type Ticket = ();
#     fn admit(&self, _: &serve_net::net_io::ConnectionInfo, _: serve_net::net_io::AdmitStage)
#         -> Result<serve_net::net_io::Admitted<()>, serve_net::net_io::Refusal> {
#         Ok(serve_net::net_io::Admitted { lane: serve_net::Lane(4), ticket: (), tag: serve_net::net_io::GateTag(0) })
#     }
#     fn readmit(&self, _: &mut (), _: &serve_net::net_io::ConnectionInfo) -> Result<(), serve_net::net_io::Refusal> { Ok(()) }
#     fn check(&self, _: &mut (), _: &serve_net::net_io::WriteCheck) -> Result<(), serve_net::net_io::Denial> { Ok(()) }
# }
let control = Arc::new(SimpleControl::new());
let server = Server::builder()
    .listener(ListenerSpec::tcp("web", "0.0.0.0:8080"))
    .gate(MyGate)
    .control(control.clone())
    .serve(serve_net::handler(|_req, reply| { reply.text(200, "ok"); }))?;
# drop(server);
# Ok::<(), serve_net::ServeError>(())
```

### `ServerBuilder::build`, `Built`, `ConfigWarning`

```rust
impl<H: Hooks> ServerBuilder<H> {
    pub fn build(self) -> Result<Built<H>, ConfigErrors>;
}

#[must_use]
pub struct Built<H: Hooks = DefaultHooks> { /* validated configuration and hooks */ }
impl<H: Hooks> Built<H> {
    pub fn warnings(&self) -> &[ConfigWarning];
    pub fn profiles(&self) -> &ProfileTable;
    pub fn bind<S: HttpService>(self, service: S) -> Result<Bound, ServeError>;           // A-21
    pub fn bind_family<F: Family, S: Service<F>>(self, family: F, service: S) -> Result<Bound, ServeError>;
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigWarning { pub rule: RuleId, pub subject: Subject, pub message: &'static str }
impl fmt::Display for ConfigWarning;
```

`build` runs every [validation rule](#ruleid) and collects every failure; compiles the profile table; computes per-worker shares of the in-flight caps; resolves listener addresses. Nothing is bound and no thread is started, so `build` is safe to run as a configuration check ("refuse to start" before anything is opened). Values that are allowed but questionable (a proxy listener with `Scheme::Http` declared and `trust_scheme` off, keep-alive idle under 5 s behind a proxy, `catch_service_panics` in an abort build) are kept in `warnings()`.

| Item | Grade | Tier | Version | Cost |
|---|---|---|---|---|
| `build`, `Built::warnings`, `Built::profiles`, `ConfigWarning` | P0 | 2 | `0.3.0` | at most 1 ms for 64 listeners and 255 profiles [est.] |

Errors: [`ConfigErrors`](#configerrors-configerror), never empty, each entry with a rule, the listener or profile, a message, and a hint ("bind 127.0.0.1 or a Unix socket, or call allow_public_bind with the reason").

```rust
use serve_net::{Ingress, ListenerSpec, ProxySource, RuleId, Server, TrustedProxies};

let errors = Server::builder()
    .listener(ListenerSpec::tcp("Bad Name", "127.0.0.1:0"))
    .listener(ListenerSpec::tcp("site", "0.0.0.0:8080")
        .ingress(Ingress::Proxy(ProxySource::forwarded_for(TrustedProxies::loopback()))))
    .build()
    .unwrap_err();
// Both problems are reported at once.
assert!(errors.iter().any(|e| e.rule == RuleId::BadName));
assert!(errors.iter().any(|e| e.rule == RuleId::PublicBind));
```

### `Built::bind`, `Bound`

```rust
#[must_use = "a Bound server accepts nothing until open() is called"]
pub struct Bound { /* the net-io server with its door closed */ }
impl Bound {
    pub fn local_addrs(&self) -> impl Iterator<Item = (&str, SocketAddr)> + '_;
    pub fn listeners(&self) -> Vec<ListenerInfo>;
    pub fn open(self) -> Result<Running, ServeError>;
}
```

`bind` takes the service and builds net-io's server: every listener is bound, adopted, inherited, or received in declaration order, net-io's PROXY expectation and trusted list are set per listener, and the workers start with the **door closed**: nothing is accepted (the kernel backlog may queue connections). This is the moment to drop privileges, after binding a privileged port and before any untrusted byte is read. `bind_family` does the same for a [custom family](#family-httpfamily) on listeners whose profile engine is `EngineChoice::Custom`.

If any listener fails, everything bound so far is closed (inherited and handed-off sockets are released, not closed, so the owner can retry or pass them on) and the error names the failing listener.

| Item | Grade | Tier | Version |
|---|---|---|---|
| `bind`, `Bound`, `local_addrs`, `listeners` | P0 | 2 | `0.3.0` |
| `bind_family` | P0 | 3 | `0.3.0` |

Errors: `ServeError::Bind { listener, source }`, `ServeError::Inherited`, `ServeError::HandOff` (`0.4.0`), `ServeError::Start` (net-io could not start its workers: every started worker is stopped and the listeners are released), `ServeError::Config` (`bind` called while a profile needs a custom engine, or `bind_family` while one needs HTTP).

```rust,no_run
use serve_net::{ListenerSpec, Server};
let built = Server::builder().listener(ListenerSpec::tcp("web", "0.0.0.0:80")).build()?;
let bound = built.bind(serve_net::handler(|_req, reply| { reply.text(200, "hi"); }))?;
// Port 80 is bound; drop root privileges here, before anything is accepted.
let running = bound.open()?;
# drop(running);
# Ok::<(), Box<dyn std::error::Error>>(())
```

### `Bound::open`, `ServerBuilder::serve`

```rust
impl Bound {
    pub fn open(self) -> Result<Running, ServeError>;
}
impl<H: Hooks> ServerBuilder<H> {
    pub fn serve<S: HttpService>(self, service: S) -> Result<Running, ServeError>;  // build + bind + open
}
```

`open` opens the door on every listener, then calls `Notify::ready()` once every listener is accepting. `serve` is `build`, `bind`, and `open` in one call; a validation failure becomes `ServeError::Config`.

| Item | Grade | Tier | Version | Cost |
|---|---|---|---|---|
| `open` | P0 | 2 | `0.3.0` | accepting within 1 ms (net-io) |
| `serve` | P0 | 2 | `0.3.0` | start-up only |

Errors: `ServeError::Start` (net-io refused to open the door, for example `ShuttingDown`), `ServeError::Config` (from `serve`).

### `Running`

```rust
#[must_use = "dropping Running closes every connection at once; call wait() or drain()"]
pub struct Running { /* the net-io server, serve-net's shared state */ }
impl Running {
    pub fn local_addr(&self) -> Option<SocketAddr>;
    pub fn listeners(&self) -> Vec<ListenerInfo>;
    pub fn close_door(&self);
    pub fn drain(self, deadline: Duration) -> DrainReport;
    pub fn drain_handle(&self) -> DrainHandle;
    pub fn wait(self) -> DrainReport;
    pub fn reload(&self, next: ServerConfig) -> Result<ReloadSummary, ServeError>;   // P1, 0.4.0
    pub fn health(&self) -> Health;
    pub fn stats(&self) -> Stats;
    pub fn server_handle(&self) -> net_io::ServerHandle;
}
impl Drop for Running;   // immediate abortive close of every connection, reported to the observer
impl fmt::Debug for Running;   // listener names and addresses, state; never request data
```

A running server. `Send + Sync`. Not generic: the service and hook types live inside the workers, so the type a program holds is always `Running`.

| Item | Behaviour | Errors | Cost | Grade | Tier | Version |
|---|---|---|---|---|---|---|
| `local_addr` | the first TCP listener's address (useful with port 0 in tests) | | none | P1 | 1 | `0.3.0` |
| `listeners` | name, address or path, ingress, profile, and state per listener (allocates; not a hot path) | | O(listeners) | P1 | 2 | `0.3.0` |
| `close_door` | see [below](#runningclose_door) | | under 1 ms [est.] | P0 | 2 | `0.3.0` |
| `drain`, `drain_handle`, `wait` | see [below](#runningdrain-runningwait-drainhandle) | | | P0 | 1 | `0.3.0` |
| `reload` | see [below](#serverconfig-runningreload-reloadsummary) | `Reload`, `ReloadBind`, `Draining` | validation plus binds | P1 | 2 | `0.4.0` |
| `health` | [`Health`](#health-healthissue), computed on call | | O(listeners + workers) | P1 | 1 | `0.3.0` |
| `stats` | [`Stats`](#stats), the sum of per-worker counters and net-io's transport statistics | | O(workers) | P1 | 2 | `0.3.0` |
| `server_handle` | net-io's handle (kill one connection by id, wake every worker, a worker's completer) | | an `Arc` clone | P1 | 3 | `0.3.0` |
| `Drop` | closes every connection abortively at once (a `Drop` must not block for seconds), calls `done` for open requests with `RequestEnd::Killed`, reports it | | | P0 | 1 | `0.3.0` |

### `Running::close_door`

```rust
pub fn close_door(&self);
```

Stops accepting on every listener within one worker loop iteration; idempotent. Listening sockets stay open until the drain ends, so connections queued in the kernel backlog stay there for another process that holds the same socket (a restart by hand-off); in-flight work continues. Call it first in a shutdown, before stopping anything the service depends on. P0, Tier 2, `0.3.0`; under 1 ms to stop every accept loop [est.]; no error (failures are reported to the observer).

### `Running::drain`, `Running::wait`, `DrainHandle`

```rust
impl Running {
    pub fn drain(self, deadline: Duration) -> DrainReport;
    pub fn drain_handle(&self) -> DrainHandle;
    pub fn wait(self) -> DrainReport;
}

#[derive(Clone)]
pub struct DrainHandle { /* Send + Sync */ }
impl DrainHandle {
    pub fn drain(&self, deadline: Duration);    // starts a drain and returns at once
    pub fn is_draining(&self) -> bool;
}
```

A drain closes the door; closes idle keep-alive connections at once; lets requests in flight finish, with the close signal on their replies (`Connection: close` for HTTP/1.1); lets deferred and streaming requests continue until the deadline; then closes every remaining connection abortively and returns a [`DrainReport`](#drainreport). `Notify::stopping()` is called at the start, and `Notify::extend_timeout(deadline)` too when the deadline is longer than 5 s.

- `drain` blocks the caller (parked, no CPU) until the drain finishes, then shuts the server down.
- `DrainHandle::drain` starts a drain from any thread (a signal-handling thread, a supervisor channel, an admin endpoint) and returns at once; `wait` returns its report.
- `wait` parks until a drain is requested (through a `DrainHandle`, or the [control line](#the-control-line) moving to `Draining` or `Stopping`), then drains with that deadline (the handle's, the line's, or `drain_deadline`).
- A second drain with a shorter deadline shortens it; a longer one is ignored.

| Grade | Tier | Version | Cost |
|---|---|---|---|
| P0 | 1 | `0.3.0` | no per-request cost beyond the control checkpoint that already exists; idle connections close within one timer tick |

Errors: none; the report says what happened.

```rust,no_run
use std::time::Duration;

let server = serve_net::http("127.0.0.1:8080", |_req, reply| { reply.text(200, "ok"); })?;
let handle = server.drain_handle();
std::thread::spawn(move || {
    // ... wait for the application's stop signal (a channel, a signal library) ...
    handle.drain(Duration::from_secs(30));
});
let report = server.wait();
println!("drained in {:?}: {} graceful, {} killed", report.elapsed, report.graceful, report.killed);
# Ok::<(), serve_net::ServeError>(())
```

```rust,no_run
// An ordered shutdown: the door closes first, then in-flight work drains.
# let server = serve_net::http("127.0.0.1:0", |_req, reply| { reply.text(200, "ok"); })?;
server.close_door();
// ... stop accepting jobs elsewhere ...
let report = server.drain(std::time::Duration::from_secs(10));
assert!(!report.deadline_hit || report.killed > 0);
# Ok::<(), serve_net::ServeError>(())
```

### `DrainReport`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrainReport {
    pub graceful: u32,                  // connections that finished and closed normally
    pub killed: u32,                    // connections closed at the deadline
    pub requests_finished: u64,         // requests that completed during the drain
    pub refused_while_draining: u64,    // requests refused because the server was draining
    pub elapsed: Duration,
    pub deadline_hit: bool,
}
```

P0, Tier 2, `0.3.0`.

### `ListenerInfo`, `ListenerState`

```rust
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct ListenerInfo {
    pub name: String,
    pub local: net_io::Endpoint,               // address or path
    pub ingress: IngressKind,
    pub profile: ProfileId,
    pub state: ListenerState,
    pub public_bind_reason: Option<&'static str>,
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenerState { Bound, Accepting, DoorClosed, Failing }
```

`Failing` means accept errors for longer than `accept_failure_window`. `public_bind_reason` repeats the reason given to `allow_public_bind`, so the exception shows in status pages and logs. P1, Tier 2, `0.3.0`.

### `ServerConfig`, `Running::reload`, `ReloadSummary`

```rust
#[non_exhaustive]
#[derive(Debug)]
pub struct ServerConfig {
    pub listeners: Vec<ListenerSpec>,
    pub profiles: Vec<Profile>,
    pub limits: Limits,
    pub keep_alive: KeepAlive,
    pub workers: Workers,              // a reload may not change it (rule ReloadWorkers)
    pub drain_deadline: Duration,
}
impl ServerConfig { pub fn new() -> Self; }
impl Default for ServerConfig;

impl Running {
    pub fn reload(&self, next: ServerConfig) -> Result<ReloadSummary, ServeError>;
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReloadSummary { pub added: Vec<String>, pub removed: Vec<String>, pub changed: Vec<String>, pub profiles: u16 }
```

`ServerConfig` is plain data, so a program can build it from its own configuration format and pass the same value at start-up (`ServerBuilder::config`) and at reload.

`reload` validates `next` with every rule (nothing changes on failure); diffs listeners by name (a changed address or source is a removal plus an addition; changed ingress, trust, profile, or limits apply in place); binds every added listener with its door closed (on failure only the new listeners are closed and nothing changes); calls `Notify::reloading`; publishes the new profile table, per-listener ingress and trust, and limits through [snapshots](#snapshots); opens the door on added listeners; closes the door on removed listeners and drains their connections with the drain deadline; calls `Notify::ready()` and `Observer::reloaded`. Connection-level limits apply to connections accepted after the swap; request-level limits to requests whose head completes after it; work in flight keeps the table it started with. Reloads are serialized: a reload while another runs waits for it.

| Grade | Tier | Version | Cost |
|---|---|---|---|
| P1 | 2 | `0.4.0` | validation and binds; requests see the new tables at their next checkpoint (one atomic load when nothing changed) |

Errors: `ServeError::Reload(ConfigErrors)` (nothing changed), `ServeError::ReloadBind { listener, source }` (nothing changed; the new listeners were closed), `ServeError::Draining`.

```rust,no_run
use serve_net::{Limits, ListenerSpec, ServerConfig};
# let server = serve_net::http("127.0.0.1:0", |_req, reply| { reply.text(200, "ok"); })?;
// Usually built from the program's own configuration file, read again on a reload request.
let mut next = ServerConfig::new();
next.listeners.push(ListenerSpec::tcp("http-0", "127.0.0.1:8080"));
next.limits = Limits::new().max_body(4 * 1024 * 1024);
match server.reload(next) {
    Ok(summary) => println!("reloaded: +{:?} -{:?}", summary.added, summary.removed),
    Err(e) => eprintln!("reload refused, old configuration kept: {e}"),
}
# Ok::<(), serve_net::ServeError>(())
```

---

## Listeners

### `ListenerSpec`: sources

```rust
#[must_use]
#[derive(Debug)]
pub struct ListenerSpec { /* name, source, options; not Clone: a handed-off socket is move-only */ }
impl ListenerSpec {
    pub fn tcp<A: ToSocketAddrs>(name: &str, addr: A) -> Self;
    pub fn unix(name: &str, path: impl Into<PathBuf>) -> Self;                     // P1, 0.4.0
    pub fn inherited(name: &str) -> Self;                                           // P1, 0.4.0
    pub fn fastcgi_descriptor_zero(name: &str) -> Self;                             // P1, 0.4.0, Unix
    pub fn handed_off(name: &str, listener: net_io::ListenerHandle) -> Self;        // P0, 0.4.0
}
```

One listener. The name (1 to 64 bytes of `[a-z0-9._-]`, unique per server) appears in errors, events, health, and hand-off manifests.

| Constructor | Source | Defaults | Notes |
|---|---|---|---|
| `tcp` | binds the first address `addr` resolves to | `Ingress::Direct`, profile 0, `Scheme::Http`, backlog 1,024 | resolved when called; an address that resolves to nothing is `RuleId::AddressResolve` at `build` |
| `unix` | binds a Unix socket path (net-io binds it atomically, mode `0o660`, removes a stale socket only when nothing listens on it) | `Ingress::Unix(UnixPeerTrust::SameUser, None)` | Windows when net-io supports AF_UNIX (`0.5.0`) |
| `inherited` | the socket systemd passed under this name (`LISTEN_FDNAMES`), or by position when the manager passed no names | as `tcp` | `fallback_bind` binds an address when the process was not socket-activated; unused inherited sockets are closed and warned about |
| `fastcgi_descriptor_zero` | descriptor 0, when it is a listening socket with no peer (the FastCGI convention used by mod_fcgid) | `Ingress::FastCgi`, peers trusted (the socket is private to the process manager) | Unix only until named pipes (after `1.0.0`) |
| `handed_off` | a listening socket received from a supervisor through net-io's hand-off functions | as `tcp`, or `Unix` for a Unix socket | serve-net checks the socket is listening and of the kind the ingress expects |

Errors (at `bind`): `ServeError::Bind` (in use, permission, not on this host; for paths: the directory is missing or the path exists and is not a socket), `ServeError::Inherited` ("no inherited socket named web; LISTEN_FDNAMES has: api, admin"), `ServeError::HandOff` ("socket web is not listening").

```rust,no_run
use serve_net::{ListenerSpec, Server};
let server = Server::builder()
    .listener(ListenerSpec::tcp("public", "0.0.0.0:8080"))
    .listener(ListenerSpec::tcp("public6", "[::]:8080"))
    .serve(serve_net::handler(|_req, reply| { reply.text(200, "dual stack"); }))?;
# drop(server);
# Ok::<(), serve_net::ServeError>(())
```

```rust,ignore
// 0.4.0: systemd socket activation, with a fallback for development.
let spec = ListenerSpec::inherited("web").fallback_bind("127.0.0.1:8080".parse()?);
```

### `ListenerSpec`: options

```rust
impl ListenerSpec {
    pub fn ingress(self, ingress: Ingress) -> Self;
    pub fn profile(self, id: ProfileId) -> Self;
    pub fn scheme(self, scheme: Scheme) -> Self;                         // the declared scheme
    pub fn backlog(self, n: u32) -> Self;                                // 1 to 65,535
    pub fn listen_options(self, options: net_io::ListenOptions) -> Self; // reuse port, IPv6-only, buffers, defer accept
    pub fn transport_limits(self, limits: net_io::Limits) -> Self;       // net-io values for this listener only
    pub fn max_connections(self, n: u32) -> Self;                        // soft cap for this listener
    pub fn allow_public_bind(self, reason: &'static str) -> Self;
    pub fn allow_broad_trust(self, reason: &'static str) -> Self;
    pub fn fallback_bind(self, addr: SocketAddr) -> Self;                // P1, 0.4.0, inherited only
    pub fn unix_mode(self, mode: UnixMode) -> Self;                      // P1, 0.4.0, unix only
    pub fn allow_world_writable(self, reason: &'static str) -> Self;     // P1, 0.4.0, unix only
    pub fn name(&self) -> &str;
    pub fn source(&self) -> &ListenerSource;
}
```

| Option | Behaviour | Grade | Tier | Version |
|---|---|---|---|---|
| `ingress` | how clients arrive ([Ingress](#ingress)) | P0 | 2 | `0.3.0` |
| `profile` | which [profile](#profiles) its connections use | P0 | 2 | `0.3.0` |
| `scheme` | the scheme to report when the front end says nothing about it (a proxy that terminates TLS and sends no `X-Forwarded-Proto`); default `Http` | P1 | 2 | `0.3.0` |
| `backlog` | the accept queue length (the OS may cap it; the effective value is in `ListenerInfo`) | P1 | 2 | `0.3.0` |
| `listen_options` | net-io's socket options for this listener | P1 | 3 | `0.3.0` |
| `transport_limits` | net-io limits for this listener, on top of the server's `transport_limits` | P2 | 3 | `0.3.0` |
| `max_connections` | the listener's soft connection cap ([connection caps](#limits)) | P0 | 2 | `0.3.0` |
| `allow_public_bind` | permits a proxy or FastCGI listener on a public or unspecified address; the reason is required, kept in `ListenerInfo`, and reported to the observer at `open` | P0 | 2 | `0.3.0` |
| `allow_broad_trust` | permits trust entries broader than /8 (IPv4) or /32 (IPv6); a /0 entry is never allowed | P1 | 2 | `0.3.0` |
| `fallback_bind`, `unix_mode`, `allow_world_writable` | as named | P1 | 2 | `0.4.0` |

```rust
use serve_net::{Ingress, ListenerSpec, ProfileId, ProxySource, Scheme, TrustedProxies};

let site = ListenerSpec::tcp("site", "127.0.0.1:8080")
    .ingress(Ingress::Proxy(ProxySource::forwarded_for(TrustedProxies::loopback())))
    .scheme(Scheme::Https)        // Apache terminates TLS and sends no X-Forwarded-Proto
    .profile(ProfileId(0))
    .max_connections(20_000);
assert_eq!(site.name(), "site");

// A deliberate exception, visible in ListenerInfo and reported at open.
let lb = ListenerSpec::tcp("lb", "0.0.0.0:8081")
    .ingress(Ingress::Proxy(ProxySource::forwarded_for(TrustedProxies::parse("10.0.0.0/8")?)))
    .allow_public_bind("load balancer subnet only; host firewall drops other sources");
# let _ = lb;
# Ok::<(), serve_net::net_io::AddrError>(())
```

### `ListenerSource`

```rust
#[non_exhaustive]
#[derive(Debug)]
pub enum ListenerSource {
    Tcp(SocketAddr),
    Unix(PathBuf),
    Inherited { name: String, fallback: Option<SocketAddr> },
    DescriptorZero,
    HandedOff(net_io::ListenerHandle),
    Unresolved,                      // the address resolved to nothing; build reports it
}
```

Where a listener's socket comes from, readable through `ListenerSpec::source()` for wrappers that build specs from their own configuration and want to log them. P0, Tier 3, `0.3.0` (`Tcp`, `Unresolved`), `0.4.0` (the rest).

### `UnixMode`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnixMode { pub mode: u32, pub owner: Option<u32>, pub group: Option<u32> }
impl UnixMode { pub const fn new(mode: u32) -> Self; }
impl Default for UnixMode;   // 0o660, owner and group unchanged
```

The socket file's permissions. A mode that lets others write (`0o002`) is refused (`RuleId::WorldWritableSocket`) unless `allow_world_writable(reason)` is set: anyone who can connect to a trusting listener can forge client facts. P1, Tier 2, `0.4.0`, Unix.

---

## Ingress

### `Ingress`

```rust
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum Ingress {
    Direct,                                     // clients connect to this listener themselves
    DirectTls,                                  // P0, 0.4.0: the same over TLS (net-io's TLS seam)
    Proxy(ProxySource),                         // behind a reverse proxy or load balancer
    Unix(UnixPeerTrust, Option<ProxySource>),   // P1, 0.4.0: None means the local peer is the client
    FastCgi(FastCgiIngress),                    // P1, 0.4.0, feature fastcgi
}
impl Default for Ingress;   // Direct
```

How clients reach a listener, and therefore where the real client comes from. Exactly one client-address source per listener.

| Variant | Client | Scheme | Cost per request |
|---|---|---|---|
| `Direct` | the socket peer | `http` | none |
| `DirectTls` | the socket peer | `https` | none (TLS is net-io's and tls-net's) |
| `Proxy(source)` | from the PROXY header (once per connection, net-io) or the forwarded headers (the walk, per request) | declared, or the proxy's when `trust_scheme` | PROXY: none per request; headers: under 100 ns for 3 entries [est.] |
| `Unix(trust, inner)` | the local peer, or the inner source's client | declared, or the inner source's | as the inner source |
| `FastCgi(options)` | `REMOTE_ADDR`, `REMOTE_PORT` | `HTTPS`, `REQUEST_SCHEME`, or declared | none beyond fcgi-net |

| Grade | Tier | Version |
|---|---|---|
| P0 | 2 | `0.3.0` (`Direct`, `Proxy` with forwarded headers); `0.4.0` (`DirectTls`, `Proxy` with PROXY protocol, `Unix`, `FastCgi`) |

Errors (at `build`): `ForwardedOnDirect`, `TwoClientSources`, `PublicBind`, `TrustEmpty`, `TrustWildcard`, `TrustTooBroad`, `FastCgiTrust`, `TlsOnFastCgi`, `TlsWithoutAcceptor`, `PeerCredentialsUnavailable`.

### `ProxySource`

```rust
#[must_use]
#[derive(Clone, Debug)]
pub struct ProxySource { /* kind, trusted list, options */ }
impl ProxySource {
    pub fn forwarded_for(trusted: TrustedProxies) -> Self;                       // X-Forwarded-For
    pub fn headers(source: ForwardedSource, trusted: TrustedProxies) -> Self;
    pub fn proxy_protocol(versions: net_io::ProxyVersions, trusted: TrustedProxies) -> Self;  // P0, 0.4.0
    pub fn trust_scheme(self, on: bool) -> Self;        // default off
    pub fn trust_host(self, on: bool) -> Self;          // default off
    pub fn max_hops(self, n: u8) -> Self;               // default 16, 1 to 64
    pub fn missing(self, policy: MissingHeader) -> Self;        // default Refuse
    pub fn unknown(self, policy: UnknownClient) -> Self;        // default Allow
    pub fn trusted(&self) -> &TrustedProxies;
}
```

Where the real client comes from behind a front end, and whom to believe.

**Forwarded headers** (`forwarded_for`, `headers`). Used only when the socket peer is in `trusted`; from any other peer the headers are ignored and the client is the peer. All field lines of the configured header are joined in order and walked **right to left**: trusted entries are skipped (each was written by a proxy on the list), the first untrusted entry is the client, and if every entry is trusted the leftmost is. At most `max_hops` entries are walked; more is a 400 refusal, as is a malformed entry, a zone identifier, or a list in a `SingleAddress` header. IPv4-mapped IPv6 entries are normalized before the trust test. A trusted peer that sends no header gets the [`MissingHeader`](#missingheader-unknownclient) policy. A walk that stops at RFC 7239's `unknown` or an obfuscated identifier gives `ClientInfo::addr() == None` unless `UnknownClient::Refuse` is set.

With `trust_scheme`, the scheme comes from `X-Forwarded-Proto` (or `Forwarded` `proto=`) and must be `http` or `https` (ASCII case-insensitive). With `trust_host`, the authority comes from `X-Forwarded-Host` (or `host=`) and must be a valid RFC 3986 authority of at most 255 bytes. Only the value the nearest trusted hop wrote (the rightmost) is used; anything invalid is a 400 refusal.

**PROXY protocol** (`proxy_protocol`, `0.4.0`). net-io requires a complete, valid header before any byte reaches the engine, from trusted peers only, and closes every other connection before reading (fail closed; the PROXY specification requires both). A v2 `LOCAL` command (the proxy's own health check) keeps the socket peer and sets `ClientInfo::is_proxy_local()`. With `trust_scheme`, `PP2_TYPE_SSL` marks the connection `https`; with `trust_host`, `PP2_TYPE_AUTHORITY` gives the authority. net-io keeps one trusted list per server, so serve-net gives it the union of every PROXY listener's list and checks each connection against its own listener's list when it opens, before any byte reaches the engine.

| Item | Grade | Tier | Version | Cost |
|---|---|---|---|---|
| `forwarded_for`, `headers`, options | P0 | 2 | `0.3.0` | under 100 ns for a 3-entry header [est.]; no allocation |
| `proxy_protocol` | P0 | 2 | `0.4.0` | one parse per connection in net-io |

```rust
use serve_net::{ForwardedSource, MissingHeader, ProxySource, TrustedProxies};

// nginx in front, sending X-Real-IP and X-Forwarded-Proto.
let nginx = ProxySource::headers(ForwardedSource::SingleAddress("x-real-ip"), TrustedProxies::loopback())
    .trust_scheme(true);

// A health checker on the proxy host itself sends no header: treat it as the client.
let lenient = ProxySource::forwarded_for(TrustedProxies::loopback()).missing(MissingHeader::UsePeer);
# let _ = (nginx, lenient);
```

```text
Peer 10.0.0.2 (trusted: 10.0.0.0/8), X-Forwarded-For: 198.51.100.7, 203.0.113.9, 10.0.0.5
  walk: 10.0.0.5 trusted, skip; 203.0.113.9 untrusted: client = 203.0.113.9, hops = 2
  (198.51.100.7 is whatever the client claimed; it is never used)
```

### `ForwardedSource`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardedSource {
    Forwarded,                       // RFC 7239: for=, proto=, host=
    XForwardedFor,                   // X-Forwarded-For with X-Forwarded-Proto and X-Forwarded-Host
    SingleAddress(&'static str),     // one address in a named field, for example "x-real-ip"; a list is refused
}
```

P0, Tier 2, `0.3.0`. The field name of `SingleAddress` is matched ASCII case-insensitively and must be a valid field name (`RuleId::BadHeaderName` otherwise).

### `MissingHeader`, `UnknownClient`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MissingHeader { #[default] Refuse, UsePeer }

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnknownClient { #[default] Allow, Refuse }
```

`MissingHeader::Refuse` answers 400 when a trusted peer sends no forwarded header (fail closed: a misconfigured front end shows at once instead of every request appearing to come from the proxy). `UsePeer` treats the peer as the client, for health checks the proxy host makes itself. `UnknownClient` decides what a walk that ends at `unknown` or an obfuscated identifier means. P0 (`MissingHeader`), P1 (`UnknownClient`), Tier 2, `0.3.0`.

### `UnixPeerTrust`

```rust
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnixPeerTrust { SameUser, Users(Vec<u32>), Groups(Vec<u32>), AnyLocalPeer }
```

Who may connect to a Unix listener, checked against net-io's peer credentials when the connection opens; a peer that fails is closed before any byte is read. On a platform without peer credentials only `AnyLocalPeer` validates (`RuleId::PeerCredentialsUnavailable`). P1, Tier 2, `0.4.0`.

### `FastCgiIngress`

```rust
#[must_use]
#[derive(Clone, Debug)]
pub struct FastCgiIngress { /* trusted, options */ }
impl FastCgiIngress {
    pub fn new(trusted: TrustedProxies) -> Self;            // TCP: the front ends allowed to connect
    pub fn local() -> Self;                                 // descriptor 0 or a Unix socket: the process manager
    pub fn honour_web_server_addrs(self, on: bool) -> Self; // default true: FCGI_WEB_SERVER_ADDRS narrows the list
    pub fn max_requests_answer(self, n: u16) -> Self;       // FCGI_MAX_REQS in GET_VALUES; default: the profile's in-flight cap
}
```

The FastCGI ingress options. `REMOTE_ADDR` is believed only because only trusted front ends can connect (rule `FastCgiTrust` refuses an empty list on TCP). P1, Tier 2, `0.4.0`, feature `fastcgi`.

---

## Clients and envelopes

### `ClientInfo`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientInfo { /* private */ }
impl ClientInfo {
    pub fn direct(peer: SocketAddr, tls: bool) -> ClientInfo;   // a client that connected itself (custom families, tests)
    pub fn addr(&self) -> Option<IpAddr>;               // None only when a walk ended at unknown or an obfuscated id
    pub fn port(&self) -> u16;                          // 0 when not known
    pub fn socket_addr(&self) -> Option<SocketAddr>;    // addr and port together
    pub fn scheme(&self) -> Scheme;
    pub fn hops(&self) -> u8;                           // 0 direct, 1 behind one proxy, ...
    pub fn ingress(&self) -> IngressKind;
    pub fn peer(&self) -> Option<SocketAddr>;           // the socket peer, always kept (None on a Unix socket)
    pub fn peer_credentials(&self) -> Option<net_io::PeerCredentials>;   // P1, 0.4.0: Unix sockets
    pub fn is_proxy_local(&self) -> bool;               // PROXY v2 LOCAL
    pub fn is_unknown(&self) -> bool;
}
```

The real client of a request, the same whichever way it arrived ([ingress normalization](#ingress)). Addresses are parsed `IpAddr` values (never raw strings, so a log line built from them cannot carry injected text); IPv4-mapped IPv6 is normalized to IPv4. `Copy`, at most 64 bytes [est.]. The authority lives on the request ([`HttpHead::authority`](#httphead-fields)), not here. P0, Tier 1, `0.3.0`.

```rust
use std::net::{IpAddr, Ipv4Addr};
# let reply = serve_net::testing::TestRequest::new(b"GET / HTTP/1.1\r\nHost: a\r\n\r\n")
#     .client("192.0.2.7:51000".parse().unwrap())
#     .send(&serve_net::handler(|req, reply| {
let client = req.client();
assert_eq!(client.addr(), Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7))));
assert_eq!(client.port(), 51000);
assert_eq!(client.scheme(), serve_net::Scheme::Http);
assert_eq!(client.hops(), 0);
#     reply.text(200, "ok");
# }));
# assert_eq!(reply.status, 200);
```

### `Scheme`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Scheme { #[default] Http, Https }
impl Scheme { pub const fn as_str(self) -> &'static str; }   // "http", "https"
impl fmt::Display for Scheme;
```

P0, Tier 1, `0.3.0`.

### `IngressKind`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IngressKind { Direct, DirectTls, ProxyProtocol, Forwarded, Unix, FastCgi }
impl IngressKind { pub const fn as_str(self) -> &'static str; }   // "direct", "forwarded", ...
```

How a request arrived, for logs and policy. P0, Tier 2, `0.3.0`.

### `Envelope`

```rust
#[derive(Clone, Debug)]
pub struct Envelope { /* private */ }
impl Envelope {
    pub fn connection(&self) -> ConnectionId;
    pub fn seq(&self) -> RequestSeq;
    pub fn profile(&self) -> ProfileId;
    pub fn tag(&self) -> u8;                      // the profile's opaque tag
    pub fn lane(&self) -> Lane;
    pub fn set_lane(&mut self, lane: Lane);       // honoured in head() only; later calls are ignored and reported
    pub fn client(&self) -> &ClientInfo;
    pub fn arrived(&self) -> Instant;             // first byte of the head
    pub fn deadline(&self) -> Instant;            // the request deadline
    pub fn worker(&self) -> WorkerId;
    pub fn user_word(&self) -> u64;               // one word a wrapper may set in head()
    pub fn set_user_word(&mut self, word: u64);
}
```

The facts serve-net keeps about one request, passed to every hook and service call. `connection` plus `seq` identify a request within the process. `user_word` lets a wrapper link its own per-request record (for example the index of a slot taken in `Admit`) without serve-net knowing its type. `Send + Sync`, about 96 bytes [est.], stored in the connection record: no allocation. P0, Tier 2, `0.3.0`.

```rust
use serve_net::{Envelope, HeadAction, HttpHead, HttpReply, HttpRequest, HttpService, Lane, Outcome};

struct Site;
impl HttpService for Site {
    fn head(&self, head: &HttpHead<'_>, env: &mut Envelope) -> HeadAction {
        if head.path() == "/health" {
            env.set_lane(Lane(0));    // never shed health checks before other traffic
        }
        HeadAction::Proceed
    }
    fn call(&self, req: &HttpRequest<'_>, env: &Envelope, reply: &mut HttpReply) -> Outcome {
        reply.text(200, &format!("request {} on connection {}", env.seq().get(), env.connection()));
        let _ = req;
        Outcome::Done
    }
}
```

### `RequestSeq`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestSeq(u32);
impl RequestSeq { pub const fn get(self) -> u32; }
```

1 for the first request on a connection, incremented per request (HTTP/2: the stream id). Not a security token. P1, Tier 2, `0.3.0`.

---

## Profiles

### `Profile`

```rust
#[must_use]
#[derive(Clone, Debug, PartialEq)]
pub struct Profile { /* private */ }
impl Profile {
    pub fn new(id: ProfileId) -> Self;
    pub fn tag(self, tag: u8) -> Self;                    // opaque, copied into every envelope; default 0
    pub fn engine(self, engine: EngineChoice) -> Self;    // default EngineChoice::Http1
    pub fn lane(self, lane: Lane) -> Self;                // default Lane(4)
    pub fn budget(self, budget: Budget) -> Self;
    pub fn limits(self, limits: Limits) -> Self;          // overrides the server's limits
    pub fn keep_alive(self, policy: KeepAlive) -> Self;   // overrides the server's keep-alive
    pub fn critical(self, on: bool) -> Self;              // admitted under Lockdown; default false
    pub fn wipe_buffers(self, on: bool) -> Self;          // default true
    pub fn body(self, policy: BodyPolicy) -> Self;        // default BodyPolicy::Buffer with the profile's max_body
    pub fn id(&self) -> ProfileId;
}
```

The answer to "connections on this listener use this engine, this lane, this budget, these limits". A single-service program has one profile (created with defaults if none is added), or two (a public and an admin listener with a smaller budget, a more important lane, and `critical(true)`).

`wipe_buffers` zeroes the used part of every serve-net buffer when it returns to a pool and turns on http-net's request wipe, so credentials in one request never linger into another; it matches net-io's own default. Cost about 50-100 ns per 2 KiB [est.]; turn it off only for traffic that never carries secrets. P0, Tier 2, `0.3.0`.

```rust
use serve_net::{Budget, Lane, Limits, Profile, ProfileId};
use std::time::Duration;

let public = Profile::new(ProfileId(0)).lane(Lane(4)).budget(Budget::new().max_connections(20_000));
let admin = Profile::new(ProfileId(1))
    .lane(Lane(1))
    .critical(true)
    .budget(Budget::new().max_connections(50).max_in_flight(16))
    .limits(Limits::new().request_timeout(Duration::from_secs(120)));
assert_eq!(admin.id(), ProfileId(1));
# let _ = public;
```

### `ProfileId`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ProfileId(pub u8);
impl ProfileId { pub const MAX: ProfileId = ProfileId(255); }   // reserved
```

Dense ids from 0, at most 255 profiles (`RuleId::ProfileIdReserved`, `ProfileGap`). P0, Tier 2, `0.3.0`.

### `Budget`

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Budget { /* private */ }
impl Budget {
    pub const fn new() -> Self;
    pub const fn max_connections(self, n: u32) -> Self;     // soft cap for the profile
    pub const fn max_in_flight(self, n: u32) -> Self;       // split across workers by InFlightCap
    pub const fn dedicated_workers(self, n: u16) -> Self;   // P2, 0.5.0: an isolated worker set (admin listener)
}
```

A profile's share of the server. Unset values are unlimited within the server's own caps. Zero is refused (`RuleId::ZeroCap`). P0, Tier 2, `0.3.0` (`dedicated_workers` `0.5.0`).

### `ProfileTable`

```rust
#[derive(Debug)]
pub struct ProfileTable { /* Box<[Profile]> indexed by ProfileId */ }
impl ProfileTable {
    pub fn get(&self, id: ProfileId) -> Option<&Profile>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn iter(&self) -> impl Iterator<Item = &Profile>;
}
```

The compiled, immutable table, published through a [`Snapshot`](#snapshot) so a reload swaps it whole. Hooks read a profile by id in O(1). P0, Tier 3, `0.3.0`.

### `EngineChoice`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EngineChoice {
    #[default] Http1,
    FastCgi,                                       // P1, 0.4.0
    Http1OrH2ByAlpn,                               // P1, 0.4.0 (h2 with feature h2)
    Custom { alpn: &'static [&'static str] },      // a family given to bind_family; ALPN ids it serves
}
```

The engine a profile's connections start with, and the ALPN protocols offered on its TLS listeners. A profile without h2 never offers `h2`. An ALPN result with no compiled engine closes the connection (`EndReason::NoEngine`). ALPN choices on a listener without TLS are refused (`RuleId::AlpnWithoutTls`). P0 (`Http1`, `Custom`) `0.3.0`, P1 (`FastCgi`, `Http1OrH2ByAlpn`) `0.4.0`, Tier 2.

---

## The HTTP family

Feature `http1` (and `fastcgi`, which produces the same types). Every HTTP ingress (direct, PROXY, forwarded headers, Unix socket, FastCGI, and HTTP/2 later) produces these types, so a service never depends on how requests reach it.

### `HttpService`

```rust
pub trait HttpService: Send + Sync {
    const WORK: WorkClass = WorkClass::Quick;

    fn head(&self, head: &HttpHead<'_>, env: &mut Envelope) -> HeadAction {
        let _ = (head, env);
        HeadAction::Proceed
    }

    fn call(&self, request: &HttpRequest<'_>, env: &Envelope, reply: &mut HttpReply) -> Outcome;

    fn upgraded(&self, env: &Envelope, conn: UpgradedConn) {           // P1, 0.4.0
        let _ = (env, conn);
    }

    fn done(&self, env: &Envelope, finished: &Finished) {
        let _ = (env, finished);
    }
}
```

The contract between serve-net and an HTTP service, in the order serve-net calls it per request:

| Call | When | May | Must not |
|---|---|---|---|
| `head` | the head is complete; before admission and before any body byte | choose the lane (`env.set_lane`) and the body policy, refuse, ask for an upgrade, store one word (`env.set_user_word`) | block, read the body |
| (`Admit`, `Checkpoint`) | serve-net's admission and revocation checks | | |
| `call` | the body is complete (buffered policy) | answer, defer (`reply.defer()`), close | keep borrowed request data past return (use `request.detach_head()`) |
| `upgraded` | after a 101 the service asked for | own the new conversation | |
| `done` | exactly once per request that reached `head`, on every path, after the last reply byte was handed to net-io or the request ended any other way | return per-request resources, record metrics | fail (it returns `()`) |

A service used by a running server must also be `'static` (it moves to the worker threads); `bind` requires it. The traits themselves do not, so `testing::respond` can borrow a service (D-038). `WORK` says how the service behaves ([`WorkClass`](#workclass)); [`Dispatch`](#dispatch-inline) implementations use it. A service reports failure through the reply (a status) or `Outcome::Close`; a panic is handled per [`catch_service_panics`](#serverbuilder-configuration).

| Grade | Tier | Version | Cost |
|---|---|---|---|
| P0 | 2 | `0.3.0` (`upgraded` `0.4.0`) | one direct call per method with inline dispatch, about 1 ns [est.] |

```rust
use serve_net::{BodyPolicy, Envelope, Finished, HeadAction, HttpHead, HttpReply, HttpRequest, HttpService, Outcome, WorkClass};

struct Upload;
impl HttpService for Upload {
    const WORK: WorkClass = WorkClass::Io;

    fn head(&self, head: &HttpHead<'_>, _env: &mut Envelope) -> HeadAction {
        match (head.method(), head.path()) {
            (serve_net::Method::Post, "/upload") => HeadAction::ProceedWith(BodyPolicy::Buffer { max: 8 * 1024 * 1024 }),
            (_, "/upload") => HeadAction::refuse(405),
            _ => HeadAction::Proceed,
        }
    }

    fn call(&self, req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
        reply.text(201, &format!("stored {} bytes", req.body_bytes().len()));
        Outcome::Done
    }

    fn done(&self, env: &Envelope, finished: &Finished) {
        // Metrics: status, timings, bytes.
        let _ = (env, finished.status, finished.bytes_in);
    }
}
```

### `HttpHead`, `Fields`

```rust
#[derive(Clone, Copy)]
pub struct HttpHead<'a> { /* borrowed view */ }
impl<'a> HttpHead<'a> {
    pub fn method(&self) -> Method;
    pub fn method_str(&self) -> &'a str;
    pub fn target(&self) -> &'a str;                       // exactly as received
    pub fn path(&self) -> &'a str;                         // percent-encoding kept
    pub fn query(&self) -> Option<&'a str>;
    pub fn version(&self) -> Version;
    pub fn header(&self, name: &str) -> Option<&'a [u8]>;  // first field with this name, ASCII case-insensitive
    pub fn header_str(&self, name: &str) -> Option<&'a str>;
    pub fn header_all(&self, name: &str) -> impl Iterator<Item = &'a [u8]> + '_;
    pub fn headers(&self) -> Fields<'a>;                   // every field in arrival order
    pub fn authority(&self) -> Option<&'a str>;            // after ingress normalization
    pub fn client(&self) -> &ClientInfo;
    pub fn content_length(&self) -> Option<u64>;
    pub fn expects_continue(&self) -> bool;
    pub fn is_upgrade(&self) -> bool;                      // never true for h2c alone
}
impl fmt::Debug for HttpHead<'_>;                          // credential field values redacted

#[derive(Clone)]
pub struct Fields<'a> { /* iterator */ }
impl<'a> Iterator for Fields<'a> { type Item = (&'a str, &'a [u8]); }
impl ExactSizeIterator for Fields<'_>;
```

A request head, borrowed from the engine's buffer: every accessor is a slice of the bytes that arrived (no copy). The same view for HTTP/1.1 and FastCGI (FastCGI: method from `REQUEST_METHOD`, fields from `HTTP_*`, `CONTENT_TYPE`, `CONTENT_LENGTH`). `authority` is the normalized authority: the trusted proxy's `X-Forwarded-Host` when `trust_host` is on, otherwise the absolute-form authority or the `Host` field. Field lookups are a linear scan of at most `max_headers` entries (no hash map keyed by attacker data). P0, Tier 2, `0.3.0`; building the view costs nothing (borrowed spans).

```rust
let reply = serve_net::testing::respond(
    &serve_net::handler(|req, reply| {
        let head = req.head();
        assert_eq!(head.path(), "/search");
        assert_eq!(head.query(), Some("q=rust"));
        assert_eq!(head.header("accept"), Some(&b"text/html"[..]));
        assert_eq!(head.authority(), Some("example.com"));
        reply.text(200, "ok");
    }),
    b"GET /search?q=rust HTTP/1.1\r\nHost: example.com\r\nAccept: text/html\r\n\r\n",
);
assert_eq!(reply.status, 200);
```

### `HeadAction`

```rust
#[non_exhaustive]
#[derive(Debug)]
pub enum HeadAction {
    Proceed,                                          // the profile's body policy
    ProceedWith(BodyPolicy),
    Refuse { status: u16, retry_after: Option<Duration> },
    Upgrade(UpgradeTarget),                           // P1, 0.4.0
}
impl HeadAction {
    pub const fn refuse(status: u16) -> Self;         // Refuse { status, retry_after: None }
}
```

What `head` decided. `Refuse` answers with the engine's refusal bytes for that status (a fixed short body, the close signal, `Retry-After` when given) without reading the body; a status outside 400 to 599 becomes 500 and is reported. `Upgrade` for a protocol the request did not offer is a service bug: serve-net answers 500 and reports it. P0, Tier 2, `0.3.0`.

### `BodyPolicy`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyPolicy {
    Buffer { max: u64 },      // read the whole body before call (the default, max = Limits::max_body)
    Stream,                   // P2, 0.5.0: call at the head, body chunks as they arrive
    Discard { max: u64 },     // read and drop up to max, then the reply; beyond max, close without reading
}
```

| Policy | Behaviour | Grade | Version |
|---|---|---|---|
| `Buffer` | the body is read into a pooled buffer, bounded by `max` and the engine's own `max_body` (the smaller wins), under the request deadline and the minimum rate; a declared length over the bound is refused (413) before any body byte is read | P0 | `0.3.0` |
| `Stream` | `call` runs at the head. Reactor: repeated `call`s with `HttpBody::Chunk`, reads paused while the service holds a chunk (push). Blocking: a reader that parks for more (pull) | P2 | `0.5.0` |
| `Discard` | for answering without the body (a redirect after a large upload attempt): the body is read and dropped up to `max` so the connection stays usable; beyond `max` the connection closes after the reply | P0 | `0.3.0` |

### `HttpRequest`, `HttpBody`

```rust
pub struct HttpRequest<'a> { /* head view + body */ }
impl<'a> HttpRequest<'a> {
    pub fn head(&self) -> &HttpHead<'a>;
    pub fn method(&self) -> Method;                     // shortcuts to head()
    pub fn path(&self) -> &'a str;
    pub fn query(&self) -> Option<&'a str>;
    pub fn header(&self, name: &str) -> Option<&'a [u8]>;
    pub fn client(&self) -> &ClientInfo;
    pub fn body(&self) -> HttpBody<'a>;
    pub fn body_bytes(&self) -> &'a [u8];               // the buffered body, or empty
    pub fn detach_head(&self) -> DetachedHead;
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug)]
pub enum HttpBody<'a> {
    Empty,
    Full(&'a [u8]),
    Chunk { bytes: &'a [u8], last: bool },              // P2, 0.5.0, BodyPolicy::Stream
}
impl<'a> HttpBody<'a> { pub fn bytes(&self) -> &'a [u8]; }
```

The request a service answers. The same type for HTTP/1.1, FastCGI, and (later) HTTP/2. P0, Tier 1, `0.3.0`.

```rust
let reply = serve_net::testing::respond(
    &serve_net::handler(|req, reply| {
        assert_eq!(req.method(), serve_net::Method::Post);
        assert_eq!(req.body_bytes(), b"name=ada");
        reply.text(200, "saved");
    }),
    b"POST /form HTTP/1.1\r\nHost: a\r\nContent-Length: 8\r\n\r\nname=ada",
);
assert_eq!(reply.text(), "saved");
```

### `HttpReply`

```rust
pub type HttpReply = Reply<HttpFamily>;      // pooled: status, fields, body, and the link to its connection
impl Reply<HttpFamily> {
    pub fn status(&mut self, status: u16) -> &mut Self;                          // default 200
    pub fn header(&mut self, name: &str, value: impl AsRef<[u8]>) -> &mut Self;
    pub fn body(&mut self, bytes: &[u8]) -> &mut Self;                           // copied into the pooled buffer
    pub fn body_static(&mut self, bytes: &'static [u8]) -> &mut Self;            // no copy
    pub fn text(&mut self, status: u16, text: &str) -> &mut Self;                // text/plain; charset=utf-8
    pub fn html(&mut self, status: u16, html: &str) -> &mut Self;                // text/html; charset=utf-8
    pub fn close(&mut self) -> &mut Self;                                        // end the connection after it
    pub fn retry_after(&mut self, after: Duration) -> &mut Self;
    pub fn stream(&mut self) -> ReplyStream<'_>;                                 // P2, 0.5.0
}
impl fmt::Debug for Reply<HttpFamily>;                                           // set-cookie and credential values redacted
// From the generic Reply<F> (all families): defer(&mut self) -> ReplyToken<F>, see ReplyToken.
```

The reply a service writes. `HttpReply` is the HTTP family's form of the generic [`Reply`](#family-httpfamily), so deferral works the same for every family. Buffered: nothing reaches the network until `call` returns, so the connection gate's egress check and a `Checkpoint` denial see the whole reply before any byte leaves. A reply the service did not touch is `200` with an empty body. Invalid field names or values (a CR or LF in a value, a non-token name, a framing field such as `content-length`) are recorded and turn the reply into the 500 refusal when it is written, never into a malformed response. `Date` is added unless set; no `Server` field is added. The buffer is reused across requests (no allocation in steady state) and wiped on return when the profile wipes.

| Grade | Tier | Version | Cost |
|---|---|---|---|
| P0 | 1 | `0.3.0` (`stream` `0.5.0`) | encoding through http-net with borrowed fields, about 100-150 ns for five fields [est.]; one copy of the body unless static |

```rust
use std::time::Duration;
let reply = serve_net::testing::respond(
    &serve_net::handler(|_req, reply| {
        reply
            .status(503)
            .header("content-type", "application/json")
            .retry_after(Duration::from_secs(5))
            .body_static(br#"{"error":"maintenance"}"#);
    }),
    b"GET / HTTP/1.1\r\nHost: a\r\n\r\n",
);
assert_eq!(reply.status, 503);
assert_eq!(reply.header("retry-after"), Some(&b"5"[..]));
```

```rust
// A CR in a value is never written: the client gets the 500 refusal instead.
let reply = serve_net::testing::respond(
    &serve_net::handler(|_req, reply| { reply.header("x-name", "a\r\nb").text(200, "x"); }),
    b"GET / HTTP/1.1\r\nHost: a\r\n\r\n",
);
assert_eq!(reply.status, 500);
```

### `Outcome`

```rust
#[non_exhaustive]
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome { Done, Deferred, Close }
```

What `call` returns. `Done`: the reply is complete. `Deferred`: the service called `reply.defer()` and completes the reply later through the token. `Close`: send what the reply holds (nothing if it is untouched) and close the connection. After `defer()` the reply belongs to the token, so any outcome other than `Deferred` is treated as `Deferred` (and reported through the observer in debug builds); `Deferred` without a `defer()` answers the 500 refusal and is reported. P0, Tier 2, `0.3.0`.

### `ReplyToken`, `CompleteError`

```rust
impl<F: Family> Reply<F> {
    pub fn defer(&mut self) -> ReplyToken<F>;
}

pub struct ReplyToken<F: Family = HttpFamily> { /* worker, connection, generation, the reply */ }
impl<F: Family> ReplyToken<F> {
    pub fn reply(&mut self) -> &mut Reply<F>;
    pub fn complete(self) -> Result<(), CompleteError<F>>;
    pub fn is_cancelled(&self) -> bool;      // the deadline passed, the peer left, or the server is stopping
}
impl<F: Family> Drop for ReplyToken<F>;      // dropped without complete: the connection closes with the 500 refusal

#[non_exhaustive]
pub enum CompleteError<F: Family = HttpFamily> {
    Expired(ReplyToken<F>),                  // too late: the connection already got the timeout refusal
    Busy(ReplyToken<F>),                     // the worker's completion queue is full: retry, or drop the token
}
```

A reply finished on another thread (a database call, a password hash, a call to another service). `reply.defer()` moves the reply into a token; the service sends the token to whatever does the work and returns `Outcome::Deferred`; the other thread fills `token.reply()` and calls `complete()`. Meanwhile the connection is parked: no further requests are parsed, the deadline still runs (`reply_timeout`), and the worker serves its other connections. `complete` posts to the owning worker through net-io's completion queue and wakes it. The token is `Send`; `complete` consumes it, so completing twice cannot compile. The default type parameter exists with feature `http1`.

| Grade | Tier | Version | Cost |
|---|---|---|---|
| P0 | 3 | `0.3.0` (blocking driver), `0.5.0` (reactor) | one queue post and one wake, about 1-5 µs from `complete` to the reply being written [est.]; no allocation (the reply moves through a per-connection slot reused across requests) |

Errors: `Expired`, `Busy`, as above. A per-worker cap (`Limits::max_deferred_per_worker`) bounds parked requests; past it, a deferral is answered with the busy refusal.

```rust
use serve_net::{CompleteError, Envelope, HttpReply, HttpRequest, HttpService, Outcome, ReplyToken, WorkClass};
use std::sync::{mpsc, Mutex};

/// Sends each lookup to a pool of database threads.
struct Lookup { jobs: Mutex<mpsc::SyncSender<(String, ReplyToken)>> }

impl HttpService for Lookup {
    const WORK: WorkClass = WorkClass::Io;
    fn call(&self, req: &HttpRequest<'_>, _env: &Envelope, reply: &mut HttpReply) -> Outcome {
        let key = req.path().to_owned();
        let token = reply.defer();
        let queued = self.jobs.lock().map(|jobs| jobs.try_send((key, token)));
        match queued {
            Ok(Ok(())) => Outcome::Deferred,
            // A full queue hands the job back: answer it at once instead of waiting.
            Ok(Err(mpsc::TrySendError::Full((_, mut token)))) | Ok(Err(mpsc::TrySendError::Disconnected((_, mut token)))) => {
                token.reply().text(503, "busy");
                let _ = token.complete();
                Outcome::Deferred
            }
            Err(_) => Outcome::Deferred, // poisoned lock: the dropped token closes the connection with 500
        }
    }
}

/// One database thread.
fn database_thread(jobs: mpsc::Receiver<(String, ReplyToken)>) {
    for (key, mut token) in jobs {
        if token.is_cancelled() {
            continue;                                   // nobody is waiting any more
        }
        token.reply().text(200, &format!("value of {key}"));
        if let Err(CompleteError::Busy(again)) = token.complete() {
            drop(again);                                // closes that connection with 500
        }
    }
}
# let _ = database_thread;
```

### `Finished`, `RequestEnd`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug)]
pub struct Finished {
    pub status: Option<u16>,              // the status written, if any
    pub end: RequestEnd,
    pub bytes_in: u64,                    // head and body
    pub bytes_out: u64,                   // the reply as handed to net-io
    pub head_at: Instant,
    pub admitted_at: Option<Instant>,
    pub replied_at: Option<Instant>,
    pub head_reads: u16,                  // P2, 0.4.0: reads it took to complete the head
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestEnd { Replied, Refused(RefusalKind), Shed, Denied, TimedOut, PeerGone, Drained, Killed, ServicePanic, Dropped }
```

What happened to a request, given to `done` and `Observer::request_finished`. `Dropped` means a `ReplyToken` was dropped without `complete`. `head_reads` is a cheap tool signal: a browser sends its head in one or two reads, a slowloris tool in many (idea I-4). P0, Tier 2, `0.3.0`.

### `WorkClass`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WorkClass { #[default] Quick, Io, Cpu }
```

How a service behaves: `Quick` (microseconds, never blocks), `Io` (waits on something: a database, a file, a network call), `Cpu` (long computation: password hashing, image work). `Inline` dispatch ignores it; [`Offload`](#offload-offloadpool-offloadjob) runs `Quick` in place and sends `Io` and `Cpu` to a pool. In debug builds a `Quick` service that takes longer than 1 ms is reported through `Observer::service_misdeclared`. P0, Tier 3, `0.3.0`.

### `DetachedHead`

```rust
pub struct DetachedHead { /* pooled bytes and spans */ }
impl DetachedHead {
    pub fn method(&self) -> Method;
    pub fn method_str(&self) -> &str;
    pub fn target(&self) -> &str;
    pub fn path(&self) -> &str;
    pub fn query(&self) -> Option<&str>;
    pub fn version(&self) -> Version;
    pub fn header(&self, name: &str) -> Option<&[u8]>;
    pub fn header_all<'s>(&'s self, name: &'s str) -> impl Iterator<Item = &'s [u8]> + 's;
    pub fn headers(&self) -> Fields<'_>;
}
impl fmt::Debug for DetachedHead;   // credential values redacted
```

An owned, `Send` copy of a request head with the same accessors, for work that outlives `call`. Made from pooled vectors (one copy of the head, typically under 1 KiB, about 20-60 ns [est.]) and returned to its worker's pool on drop. The head is always copied, never swapped with the connection buffer, because that buffer still holds pipelined input and the body window. P0, Tier 3, `0.3.0`.

### Upgrades: `UpgradeTarget`, `UpgradedConn`

```rust
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum UpgradeTarget {
    WebSocket { limits: ws_net::Limits, profile: Option<ProfileId> },        // feature ws
    Custom { protocol: &'static str, profile: Option<ProfileId> },          // a custom family's engine
}

pub struct UpgradedConn { /* the connection, now owned by the new engine */ }
impl UpgradedConn {
    pub fn envelope(&self) -> &Envelope;
    pub fn into_websocket(self) -> Result<WebSocketConn, UpgradedConn>;     // feature ws
}
```

`head` returns `HeadAction::Upgrade(target)`; serve-net validates the handshake through the target engine (ws-net checks the WebSocket key and version and answers its own status on failure, before anything else is written), writes the 101, replaces the engine on the connection, feeds it the bytes that arrived with the request, switches deadlines and limits to the target profile, and calls `HttpService::upgraded`. `Upgrade: h2c` never reaches this path: it is not an upgrade offer, and the request is served as HTTP/1.1. A 2xx to `CONNECT` is refused unless a profile allows tunnels (after `1.0.0`). P1, Tier 2, `0.4.0`. `WebSocketConn` is specified with [`engine::WebSocket`](#enginewebsocket).

---

## Limits, keep-alive, workers

### `Limits`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits { /* private */ }
impl Limits {
    pub const fn new() -> Self;
    pub const fn head_timeout(self, d: Duration) -> Self;             // default 10 s; 100 ms to 300 s
    pub const fn request_timeout(self, d: Duration) -> Self;          // default 30 s; head_timeout to 1 h
    pub const fn reply_timeout(self, d: Duration) -> Self;            // default 30 s; deferred replies
    pub const fn write_stall(self, d: Duration) -> Self;              // default 30 s
    pub const fn min_rate(self, bytes_per_sec: u32, window: Duration) -> Self;   // default 256 B/s over 10 s; 0 disables (warning)
    pub const fn max_body(self, bytes: u64) -> Self;                  // default 1 MiB
    pub const fn max_pipelined_per_wake(self, n: u16) -> Self;        // default 16
    pub const fn max_pending_output(self, bytes: u32) -> Self;        // default 256 KiB
    pub const fn max_deferred_per_worker(self, n: u32) -> Self;       // default 4,096
    pub const fn max_connections(self, n: u32) -> Self;               // soft cap, server-wide
    pub const fn hard_cap_headroom(self, n: u32) -> Self;             // default max_connections / 8, at least 16
    pub const fn linger(self, total: Duration, bytes: u32) -> Self;   // default 2 s, 64 KiB
    pub const fn engine_limits(self, limits: http_net::Limits) -> Self;   // feature http1
}
impl Default for Limits;   // Limits::new()
```

serve-net's budgets: the request-level ones it enforces itself and the transport values it sets on net-io for each listener. A profile can override the server's `Limits` as a whole (`Profile::limits`). Values outside a range are refused at `build` (`RuleId::DeadlineRange`, `DeadlineOrder`, `ZeroCap`); every value is checked before anything is committed.

| Setter | Enforced by | Guards against |
|---|---|---|
| `head_timeout` | serve-net (exchange deadline): from accept for the first request, from the first byte for later ones; total, never extended by input | slowloris (T-1) |
| `request_timeout` | serve-net (exchange deadline) from the first head byte to the end of the body | slow bodies (T-2) |
| `reply_timeout` | serve-net (park deadline) | deferred replies that never come (T-26) |
| `write_stall` | net-io's write deadline | slow readers (T-3) |
| `min_rate` | net-io's meter, inbound while receiving and outbound while sending | trickles inside every per-read timeout |
| `max_body` | the engine (the smaller of this and the engine's limit) | large uploads |
| `max_pipelined_per_wake` | serve-net | one connection starving its worker (T-6) |
| `max_pending_output` | net-io's output budget and serve-net's reply queue | replies piling up (T-3, T-6) |
| `max_deferred_per_worker` | net-io's completion queue | parked-request floods (T-26) |
| `max_connections`, `hard_cap_headroom` | serve-net (soft cap, busy refusal) and net-io (hard cap, reset) | connection floods (T-5) |
| `linger` | net-io's graceful linger | a reset destroying the last reply |
| `engine_limits` | http-net (start line, header bytes and count, field line, trailers) | oversized heads |

**Connection caps.** Three soft caps are checked in order when a connection opens: global (`Limits::max_connections`, default: the blocking worker count, or 10,000 on the reactor), per listener (`ListenerSpec::max_connections`), and per profile (`Budget::max_connections`). The first that is full answers with the busy refusal (503, `Retry-After` with jitter, close) without reading the request. net-io's hard cap is set `hard_cap_headroom` above the global soft cap; beyond it net-io resets connections without involving serve-net, so a flood large enough to pass the headroom costs one reset per connection. Cost: two atomics per connection [est. 5-20 ns].

P0, Tier 2, `0.3.0`.

```rust
use serve_net::Limits;
use std::time::Duration;

let strict = Limits::new()
    .head_timeout(Duration::from_secs(5))
    .request_timeout(Duration::from_secs(15))
    .max_body(64 * 1024)
    .max_connections(2_000)
    .min_rate(512, Duration::from_secs(10));
assert_ne!(strict, Limits::new());
```

### `KeepAlive`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeepAlive { /* private */ }
impl KeepAlive {
    pub const fn new() -> Self;                              // the defaults below
    pub const fn off() -> Self;                              // one request per connection
    pub const fn idle(self, d: Duration) -> Self;            // default: 20 s direct, 60 s behind a front end
    pub const fn max_requests(self, n: u32) -> Self;         // default 1,000
    pub const fn max_age(self, d: Duration) -> Self;         // default 1 h
    pub const fn under_pressure(self, idle: Duration) -> Self;   // default 2 s; Duration::ZERO turns adaptation off
}
impl Default for KeepAlive;   // KeepAlive::new()
```

How long a connection stays open between requests.

- **Idle timeout.** Between requests a connection closes without a response after `idle`. The default depends on the listener's ingress: 20 s on direct listeners (browsers hold many idle connections; a blocking worker is held while idle) and 60 s behind a proxy or FastCGI front end, because the backend must outlive the front end's reuse window or the front end reuses a connection the backend is closing and answers 502 (behind Apache, keep `ProxyPass ... ttl=` below it).
- **Requests per connection** and **age**: the response that reaches `max_requests`, and the first response after `max_age`, carry the close signal; an idle connection past its age is closed.
- **Under pressure** (blocking driver, where every connection holds a thread): above 75% of threads busy the idle timeout of a connection returning to idle is `under_pressure`; above 90% every response carries the close signal. The shortened idle is net-io's per-connection idle override, set at each response.

P0, Tier 2, `0.3.0`.

```rust
use serve_net::KeepAlive;
use std::time::Duration;

// Behind Apache with ProxyPass ttl=50: keep the backend longer than the front end.
let behind_apache = KeepAlive::new().idle(Duration::from_secs(75));
let api = KeepAlive::new().max_requests(10_000).max_age(Duration::from_secs(600));
# let _ = (behind_apache, api, KeepAlive::off());
```

### `Workers`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Workers { /* driver kind, count */ }
impl Workers {
    pub fn auto() -> Self;                         // net-io's Auto: the reactor where available, else blocking
    pub fn blocking(threads: u16) -> Self;         // default 256 threads
    pub fn reactor(workers: u16) -> Self;          // P0, 0.5.0; default: logical CPUs
    pub fn uring(workers: u16) -> Self;            // P3, after 1.0.0 (Linux)
    pub fn kind(&self) -> net_io::DriverKind;
    pub fn count(&self) -> Option<u16>;            // None: the driver's default
}
impl Default for Workers;   // Workers::auto()
```

Which net-io driver runs the connections, and how many workers. Blocking: a bounded pool of connection threads (256 KiB stacks), one connection per thread for its life, best for tens to low thousands of connections and for a first deployment behind a reverse proxy. Reactor: one worker per core, each owning thousands of connections, idle connections holding no read buffer. A driver this build or platform does not provide is refused at `build` (`RuleId::DriverUnavailable`); a reload may not change the workers (`RuleId::ReloadWorkers`). P0, Tier 2, `0.3.0` (blocking, auto), `0.5.0` (reactor).

```rust
use serve_net::Workers;
let small = Workers::blocking(32);
assert_eq!(small.count(), Some(32));
# let _ = Workers::auto();
```

### Defaults and reasons

| Budget | serve-net | http-net `blocking` | hyper `http1::Builder` | actix-web | nginx |
|---|---|---|---|---|---|
| head deadline | 10 s total, from accept for the first request | inside the 30 s request timeout | 30 s (`header_read_timeout`) | 5 s (`client_request_timeout`) | 60 s (`client_header_timeout`) |
| request deadline | 30 s | 30 s | none | 5 s | `client_body_timeout` 60 s between reads |
| keep-alive idle | 20 s direct, 60 s behind a front end; 2 s under pressure (blocking) | 10 s | none | 5 s | 75 s |
| requests per connection | 1,000 | none | none | none | 1,000 |
| connection age | 1 h | none | none | none | 1 h (`keepalive_time`) |
| write stall | 30 s plus 256 B/s over 10 s | 10 s | none | none | 60 s (`send_timeout`, between writes) |
| header bytes, count | 16 KiB, 100 (http-net) | same | about 400 KB, 100 | 32 fields | 8 KiB per line, 4 buffers |
| body | 1 MiB | 1 MiB | none | per extractor | 1 MiB |
| connections | blocking: 256 threads; reactor: 10,000 | 64 | none | 25,000 per worker | `worker_connections` |
| drain deadline | 30 s | none (waits) | the caller's | 30 s | `worker_shutdown_timeout`, unset |
| lingering close | 2 s, 64 KiB | 1 s, 256 KiB | none | 1 s | 30 s total, 5 s per read |
| accept backlog | 1,024 | OS default | the caller's | 1,024 | 511 on Linux |

The head deadline is short because a legitimate client sends a head in one or two packets, and a long head deadline is exactly the budget slowloris needs. The idle timeout behind a front end is long because the front end decides when to reuse a connection.

The transport values serve-net sets on net-io for each listener: `idle_timeout` (keep-alive idle), `write_timeout` (`write_stall`), `min_rate`, `output_budget` (`max_pending_output`), `completion_queue` (`max_deferred_per_worker`), `graceful_linger` (`linger`), `max_connections` and `max_per_listener` (soft caps plus headroom), `backlog`. Everything else (`handshake_timeout`, `read_timeout`, `park_timeout`, `max_per_source`, `read_class`, `buffer_budget`, `tick`, ...) keeps net-io's defaults unless `transport_limits` sets it.

---

## Hooks

The seams a wrapper uses to put its own policy around serve-net. Every hook is a trait with a provided default; the defaults are real, not empty (`InFlightCap` is real admission control, `Inline` really calls the service). Hooks run on the worker that owns the connection, so a hook must be `Send + Sync` and fast; slow work belongs behind `Dispatch`.

**Hooks backed by remote state.** A deployment may back a hook by shared or replicated state (a cluster-wide in-flight budget, a shared revocation list). A hook must answer from local state when the remote is unreachable: `Checkpoint` fails closed (deny), `Admit` chooses (usually its local share), and neither may block the worker waiting for the network.

### `Hooks`, `HookSet`, `DefaultHooks`

```rust
pub trait Hooks: Send + Sync + 'static {
    type Admit: Admit;
    type Dispatch: Dispatch;
    type Checkpoint: Checkpoint;
    type Observer: Observer;
    type Gate: net_io::Gate;
    type Events: net_io::EventSink;
    type Control: net_io::ControlLine;
    type Tls: net_io::TlsAcceptor;
}

pub struct HookSet<A, D, K, O, G, E, C, T> { /* the values */ }
impl<A, D, K, O, G, E, C, T> Hooks for HookSet<A, D, K, O, G, E, C, T> where /* each bound */;

pub type DefaultHooks = HookSet<InFlightCap, Inline, NoCheckpoint, NoObserver, AllowAll, NoopSink, NoControl, net_io::NoTls>;
```

The bundle of hook types a builder carries. Programs never name these types: the [hook setters](#serverbuilder-hooks) change one at a time. P0, Tier 3, `0.3.0`.

### `Admit`, `Admitted`, `Shed`

```rust
pub trait Admit: Send + Sync + 'static {
    fn prepare(&mut self, workers: u16, profiles: &ProfileTable) {
        let _ = (workers, profiles);
    }
    fn admit(&self, worker: WorkerId, env: &Envelope) -> Result<Admitted, Shed>;
    fn release(&self, worker: WorkerId, env: &Envelope, admitted: Admitted);
}

#[must_use]
#[derive(Debug, PartialEq, Eq)]
pub struct Admitted { pub token: u64 }        // opaque to serve-net: a slot index, a permit id

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shed { pub retry_after: Option<Duration>, pub close: bool }
```

Request admission: "can this worker take this request now?". `admit` runs after `head` and before any body byte; `release` runs after `done`, exactly once per `Admitted`, on every path. Both run on the worker that owns the connection, and a connection never moves between workers, so state indexed by `worker` is written by one thread only: a per-worker counter needs no lock and no read-modify-write. `prepare` runs once in `bind`, before any worker starts, to size per-worker state. `Shed` produces the busy refusal (503) with `retry_after` (jittered by serve-net when `None`), and `close: true` ends the connection after it.

A connection gate answers a different question ("is this source behaving?") at accept; `Admit` answers "can we take more work?" per request. P0, Tier 3, `0.3.0`; the cost is the implementation's.

```rust
use serve_net::{Admit, Admitted, Envelope, Shed, WorkerId};
use std::sync::atomic::{AtomicU32, Ordering};

/// A global cap on concurrent uploads, on top of the default per-worker cap.
struct UploadPermits { free: AtomicU32 }
impl Admit for UploadPermits {
    fn admit(&self, _worker: WorkerId, _env: &Envelope) -> Result<Admitted, Shed> {
        let mut n = self.free.load(Ordering::Relaxed);
        loop {
            if n == 0 {
                return Err(Shed { retry_after: None, close: false });
            }
            match self.free.compare_exchange_weak(n, n - 1, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => return Ok(Admitted { token: 0 }),
                Err(now) => n = now,
            }
        }
    }
    fn release(&self, _worker: WorkerId, _env: &Envelope, _admitted: Admitted) {
        self.free.fetch_add(1, Ordering::AcqRel);
    }
}
```

### `InFlightCap`

```rust
#[derive(Clone, Debug)]
pub struct InFlightCap { /* per-worker, per-profile counters */ }
impl InFlightCap {
    pub fn new() -> Self;                                     // per_worker 1,024 (reactor), 1 (blocking)
    pub fn per_worker(self, n: u32) -> Self;
    pub fn retry_after(self, base: Duration) -> Self;         // default 1 s; jitter in [0, base)
    pub fn reserve(self, lane: Lane, percent: u8) -> Self;    // P2, 0.5.0
}
impl Default for InFlightCap;
impl Admit for InFlightCap;
```

The default admission control: a real cap on requests in flight per worker and per profile (`Budget::max_in_flight`, split into per-worker shares, rounded up, at least 1). Over the cap the request gets the busy refusal with `Retry-After` = `base` plus a jitter in `[0, base)` seconds, so refused clients do not all come back in the same second. Counters are per worker, cache-line padded, and written only by their worker with plain relaxed loads and stores. On the blocking driver each worker serves one connection at a time, so the per-worker cap is 1 by construction and the profile cap does the work.

`reserve(lane, percent)` keeps a share of each worker's cap for one lane: a request takes from its own lane's reserve first, then from the shared pool; lanes without a reserve use only the shared pool, so a flood of ordinary traffic can never take the slots of health checks or administrators. Reserves summing above 100% are refused at `build`.

| Grade | Tier | Version | Cost |
|---|---|---|---|
| P0 | 2 | `0.3.0` (`reserve` `0.5.0`) | take and release under 5 ns [est.]; a refusal about 1-2 µs including the write [est.] |

```rust
use serve_net::{InFlightCap, Lane, ListenerSpec, Server};
use std::time::Duration;

let built = Server::builder()
    .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
    .admit(InFlightCap::new().per_worker(512).retry_after(Duration::from_secs(2)))
    .build()?;
# let _ = (built, Lane(0));
# Ok::<(), serve_net::ConfigErrors>(())
```

### `Dispatch`, `Inline`

```rust
pub trait Dispatch: Send + Sync + 'static {
    fn run<F: Family, S: Service<F>>(
        &self,
        service: &S,
        request: &F::Request<'_>,
        env: &Envelope,
        reply: &mut Reply<F>,
    ) -> Outcome;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Inline;
impl Dispatch for Inline;   // calls service.call(request, env, reply)
```

The one call between "request complete" and the service, so a deployment can move work off the worker without touching any service. `Inline` calls the service in place; it inlines to a direct call. Every `HttpService` is a `Service<HttpFamily>`, so one `run` serves both. P0, Tier 3, `0.3.0`; `Inline` costs nothing beyond the call.

### `Offload`, `OffloadPool`, `OffloadJob`

```rust
pub trait OffloadPool: Send + Sync + 'static {
    fn try_submit(&self, class: WorkClass, job: OffloadJob) -> Result<(), OffloadJob>;   // bounded; never blocks
}

pub struct OffloadJob { /* detached head, body, envelope copy, reply token, service handle */ }
impl OffloadJob {
    pub fn class(&self) -> WorkClass;
    pub fn run(self);            // on the pool's thread: calls the service and completes the reply
}

pub struct Offload<P: OffloadPool> { /* the pool */ }
impl<P: OffloadPool> Offload<P> { pub fn new(pool: P) -> Self; }
impl<P: OffloadPool> Dispatch for Offload<P>;
```

`Offload` runs `Quick` services inline and submits `Io` and `Cpu` services to the pool as a job holding a detached head, the body, a copy of the envelope, and a reply token; the worker goes straight back to its other connections. A full pool hands the job back and serve-net answers the busy refusal: the queue is bounded, never unbounded. On the blocking driver `Offload` behaves as `Inline` (each thread is its connection's own) and `build` warns. P1, Tier 3, `0.5.0`; about 2-10 µs per offloaded request for hand-off and completion [est.].

### `Checkpoint`, `CheckpointAt`, `Verdict`, `NoCheckpoint`

```rust
pub trait Checkpoint: Send + Sync + 'static {
    fn check(&self, env: &Envelope, at: CheckpointAt) -> Verdict;
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointAt { Admitted, BeforeDispatch, BeforeReply }

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict { Pass, Deny { status: u16 }, Kill }

#[derive(Clone, Copy, Debug, Default)]
pub struct NoCheckpoint;   // always Pass; compiles to nothing
```

Revocation during a request: a ban, a revoked session, or a lockdown that arrives while a request is in flight. `Deny` answers the engine's refusal for the status (generic body, close signal) and closes; `Kill` closes abortively with nothing written. At `BeforeReply` the reply has not left yet, so a denial replaces it whole. A checkpoint backed by remote state fails closed when the remote is unreachable. The owner's net-io gate still checks every write on top. P0, Tier 3, `0.3.0`; `NoCheckpoint` costs nothing; a one-atomic implementation about 1 ns per checkpoint [est.].

```rust
use serve_net::{Checkpoint, CheckpointAt, Envelope, Verdict};
use std::sync::atomic::{AtomicBool, Ordering};

struct Lockout { on: AtomicBool }
impl Checkpoint for Lockout {
    fn check(&self, _env: &Envelope, _at: CheckpointAt) -> Verdict {
        if self.on.load(Ordering::Acquire) { Verdict::Deny { status: 503 } } else { Verdict::Pass }
    }
}
```

### `Observer`, `NoObserver`

```rust
pub trait Observer: Send + Sync + 'static {
    fn request_head(&self, env: &Envelope) {}
    fn request_admitted(&self, env: &Envelope) {}
    fn request_shed(&self, env: &Envelope, shed: &Shed) {}
    fn request_finished(&self, env: &Envelope, finished: &Finished) {}
    fn connection_ended(&self, connection: ConnectionId, end: EndReason, requests: u32) {}
    fn capacity_refused(&self, listener: &str, profile: ProfileId) {}
    fn drain_started(&self, open_connections: u32) {}
    fn drain_finished(&self, report: &DrainReport) {}
    fn reloaded(&self, summary: &ReloadSummary) {}                  // 0.4.0
    fn reload_refused(&self, errors: &ConfigErrors) {}              // 0.4.0
    fn public_bind(&self, listener: &str, reason: &'static str) {}
    fn notify_failed(&self, error: &io::Error) {}                   // 0.4.0
    fn service_misdeclared(&self, env: &Envelope, took: Duration) {}
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoObserver;
impl Observer for NoObserver;
```

Request-level events for logs, metrics, and detection. Every method has an empty default, and serve-net is generic over the observer, so `NoObserver` compiles to nothing and a real observer is one static call per event. Events carry ids, enums, and numbers, never request bytes. Transport events (accepted, refused at accept, accept errors, kills, egress denials, closes with net-io's reason) belong to net-io and go to the owner's [event sink](#serverbuilder-hooks). An observer runs on worker threads: it copies what it needs and returns. P0, Tier 3, `0.3.0`.

```rust
use serve_net::{Envelope, Finished, Observer};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
struct Latency { slow: AtomicU64 }
impl Observer for Latency {
    fn request_finished(&self, _env: &Envelope, finished: &Finished) {
        if let Some(done) = finished.replied_at {
            if done.duration_since(finished.head_at).as_millis() > 100 {
                self.slow.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
```

### `Notify`, `NoNotify`

```rust
pub trait Notify: Send + Sync + 'static {
    fn ready(&self) {}
    fn reloading(&self, monotonic_usec: u64) {}
    fn stopping(&self) {}
    fn status(&self, line: &str) {}
    fn watchdog(&self) {}
    fn extend_timeout(&self, by: Duration) {}
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoNotify;
impl Notify for NoNotify;
```

Readiness for a service manager or a supervisor: `ready` once every listener accepts (after `open`, and after a reload), `reloading` before a reload swaps anything, `stopping` when a drain starts, `extend_timeout` during a long drain. `monotonic_usec` is the raw monotonic clock in microseconds, which systemd requires with `RELOADING=1` and which `std::time::Instant` does not expose, so the owner supplies it. A Windows service status adapter maps the same moments to `SERVICE_START_PENDING`, `SERVICE_RUNNING`, and `SERVICE_STOP_PENDING`; it needs FFI and lives outside serve-net. P1, Tier 3, `0.4.0`.

### `SystemdNotify`

```rust
pub struct SystemdNotify { /* socket address, datagram socket */ }
impl SystemdNotify {
    pub fn from_env() -> Option<Self>;                     // None when NOTIFY_SOCKET is unset, or not on Linux
    pub fn watchdog_interval() -> Option<Duration>;        // from WATCHDOG_USEC (and WATCHDOG_PID when set)
}
impl Notify for SystemdNotify;
```

systemd's `sd_notify` in safe `std` code: one `SOCK_DGRAM` datagram per call to `NOTIFY_SOCKET` (a path, or `@name` for the abstract namespace), with fixed assignments only (`READY=1`, `RELOADING=1` and `MONOTONIC_USEC=`, `STOPPING=1`, `STATUS=` with control characters removed and at most 256 bytes, `WATCHDOG=1`, `EXTEND_TIMEOUT_USEC=`). The variable is read once at construction; request data is never sent. Failures go to `Observer::notify_failed` and never stop the server: a missing manager must not stop service. Works with `Type=notify` and `Type=notify-reload` (systemd 253 and later). P1, Tier 2, `0.4.0`, Linux.

```rust,ignore
// 0.4.0
let mut builder = serve_net::Server::builder().listener(serve_net::ListenerSpec::inherited("web"));
if let Some(notify) = serve_net::SystemdNotify::from_env() {
    builder = builder.notify(notify);
}
```

### `WatchdogPinger`

```rust
pub struct WatchdogPinger { /* a parked thread */ }
impl WatchdogPinger {
    pub fn start(server: &Running, notify: Arc<dyn Notify>, interval: Duration) -> Self;
    pub fn stop(self);
}
```

A thread that wakes at half the watchdog interval and calls `Notify::watchdog()` only if every worker made progress or is idle since the last ping, so a stuck worker stops the pings and the service manager restarts the process. Not started when `WATCHDOG_PID` names another process. P2, Tier 2, `0.5.0`.

---

## The control line

serve-net uses net-io's control line rather than a second control type: one line, one epoch, read by net-io once per worker loop iteration and before each accept batch, and by serve-net at its request checkpoints. The owner passes it once with [`ServerBuilder::control`](#serverbuilder-hooks); serve-net gives the same line to net-io, merged with its own drain state so that [`DrainHandle`](#runningdrain-runningwait-drainhandle) works whatever line the owner uses (the stricter state wins).

```rust
// Re-exported from net-io:
pub trait ControlLine: Send + Sync + 'static {
    fn epoch(&self) -> u64;
    fn state(&self) -> ControlState;
    fn deadline(&self) -> Option<Instant>;
}
#[non_exhaustive]
pub enum ControlState { Normal, Shed { from_lane: Lane }, Lockdown, Draining, Stopping }
pub struct SimpleControl;   // set(state, deadline)
pub struct NoControl;       // epoch() is always 0
```

serve-net reads the line at four checkpoints per request: when the head is complete, before dispatch, before the reply is written, and when the connection returns to idle. A checkpoint is one acquire load of the epoch compared with the value the worker saw last; only when it moved does serve-net read the state.

| State | net-io | serve-net |
|---|---|---|
| `Normal` | accepts | admits per `Admit` |
| `Shed { from_lane }` | refuses new connections in `from_lane` and less important lanes | refuses requests in those lanes with the busy refusal; every response carries the close signal |
| `Lockdown` | refuses new connections on listeners not marked for lockdown (serve-net marks the listeners of critical profiles) | refuses requests on profiles not marked `critical` |
| `Draining` | closes the door and drains with the line's deadline | close signal on every response; `wait` returns the report |
| `Stopping` | kills every connection | `done` for every open request with `RequestEnd::Killed` |

P0, Tier 2, `0.3.0`; at most 2 ns per checkpoint when nothing changed.

```rust,no_run
use serve_net::{ControlState, Lane, ListenerSpec, Server, SimpleControl};
use std::sync::Arc;

let control = Arc::new(SimpleControl::new());
let server = Server::builder()
    .listener(ListenerSpec::tcp("web", "0.0.0.0:8080"))
    .control(control.clone())
    .serve(serve_net::handler(|_req, reply| { reply.text(200, "ok"); }))?;

// Memory above the high watermark: keep lanes 0 and 1, refuse the rest.
control.set(ControlState::Shed { from_lane: Lane(2) }, None);
server.server_handle().wake_all();
# Ok::<(), serve_net::ServeError>(())
```

---

## Health and statistics

### `Health`, `HealthIssue`

```rust
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health { Ok, Degraded(Vec<HealthIssue>), Failing(Vec<HealthIssue>) }

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HealthIssue {
    ListenerAcceptFailing { listener: String },
    WorkerStalled { index: u16 },
    Overloaded { shed_permille: u16 },
    Draining,
    NotifyFailing,
}
impl Health { pub fn is_ok(&self) -> bool; }
```

Computed on call from: listeners accepting (net-io's accept-error events within `accept_failure_window`), workers alive (each worker's progress counter moved since the last call, or the worker is idle), overload (refusals above 20% over the last interval), and the drain state. `Failing` when no listener accepts or every worker is stalled; `Degraded` for any other issue. P1, Tier 1, `0.3.0`.

```rust,no_run
# let server = serve_net::http("127.0.0.1:0", |_req, reply| { reply.text(200, "ok"); })?;
match server.health() {
    serve_net::Health::Ok => {}
    other => eprintln!("health: {other:?}"),
}
# Ok::<(), serve_net::ServeError>(())
```

### `Stats`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub requests: u64,
    pub replied: u64,
    pub refused: u64,               // every refusal serve-net wrote (timeouts, limits, malformed, ingress)
    pub shed: u64,                  // busy refusals from admission and soft caps
    pub denied: u64,                // Checkpoint denials and kills
    pub timed_out: u64,
    pub deferred: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub transport: net_io::Stats,   // connections accepted, refused, open, closed, ...
}
impl Stats {
    pub fn refused_for(&self, kind: RefusalKind) -> u64;
}
```

Totals since start. Counters are per worker, written only by their worker, and summed on call, so the hot path pays one local increment per counter. P1, Tier 2, `0.3.0`.

---

## Errors

### `ServeError`

```rust
#[non_exhaustive]
#[derive(Debug)]
pub enum ServeError {
    Config(ConfigErrors),
    Bind { listener: String, source: net_io::NetError },
    Start(net_io::NetError),
    Inherited { listener: String, reason: &'static str },          // 0.4.0
    HandOff { listener: String, reason: &'static str },            // 0.4.0
    Draining,
    Reload(ConfigErrors),                                          // 0.4.0
    ReloadBind { listener: String, source: net_io::NetError },     // 0.4.0
}
impl fmt::Display for ServeError;
impl std::error::Error for ServeError;    // source(): the NetError or the ConfigErrors
impl From<ConfigErrors> for ServeError;   // Config(..)
```

| Variant | When | What to do |
|---|---|---|
| `Config` | `build` (or `serve`, or a Tier 1 function) found configuration errors | read every entry; each names the rule, the listener or profile, and a hint |
| `Bind` | a listener could not be bound | the source says why: in use (stop the other process or hand the socket over), permission (bind before dropping privileges, or use a high port behind a proxy), not on this host |
| `Start` | net-io could not start its workers or open the door | the source says why (descriptor limit, driver unavailable) |
| `Inherited` | an inherited socket is missing or of the wrong kind | check the unit's `FileDescriptorName=`, or set `fallback_bind` |
| `HandOff` | a handed-off socket is not listening or of the wrong kind | check the supervisor's hand-off |
| `Draining` | the operation needs a server that is not draining | none |
| `Reload` | a reload's configuration is invalid; nothing changed | fix the entries and reload again |
| `ReloadBind` | a listener added by a reload could not be bound; nothing changed | as `Bind` |

P0, Tier 1, `0.3.0`.

```rust,no_run
match serve_net::http("127.0.0.1:80", |_req, reply| { reply.text(200, "ok"); }) {
    Ok(server) => { server.wait(); }
    Err(serve_net::ServeError::Bind { listener, source }) => eprintln!("{listener}: {source}"),
    Err(e) => eprintln!("{e}"),
}
```

### `ConfigErrors`, `ConfigError`

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigErrors { /* Vec<ConfigError>, never empty, sorted by subject */ }
impl ConfigErrors {
    pub fn iter(&self) -> impl Iterator<Item = &ConfigError>;
    pub fn len(&self) -> usize;
    pub fn has(&self, rule: RuleId) -> bool;
}
impl IntoIterator for ConfigErrors;
impl fmt::Display for ConfigErrors;   // one line per error: subject, message, hint
impl std::error::Error for ConfigErrors;

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError { pub rule: RuleId, pub subject: Subject, pub message: &'static str, pub hint: &'static str }
impl fmt::Display for ConfigError;
```

Every configuration error found, not only the first, so a configuration is fixed in one pass. Messages and hints are static text: they never quote configuration values that might be secret, and never request data. The same input always gives the same errors in the same order. P0, Tier 2, `0.3.0`.

```rust
use serve_net::{Limits, ListenerSpec, RuleId, Server};
use std::time::Duration;

let errors = Server::builder()
    .listener(ListenerSpec::tcp("web", "127.0.0.1:0"))
    .limits(Limits::new().head_timeout(Duration::from_secs(60)).request_timeout(Duration::from_secs(30)))
    .build()
    .unwrap_err();
assert!(errors.has(RuleId::DeadlineOrder));
println!("{errors}");   // server: request_timeout is shorter than head_timeout (raise request_timeout ...)
```

### `RuleId`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuleId {
    PublicBind, ForwardedOnDirect, FastCgiTrust, TrustWildcard, TrustTooBroad, TwoClientSources,
    TrustEmpty, TrustParse, WorldWritableSocket, PeerCredentialsUnavailable, DuplicateName, BadName,
    ProfileIdReserved, ProfileGap, UnknownProfile, DeadlineOrder, DeadlineRange, ZeroCap, DuplicateBind,
    DriverUnavailable, ReloadWorkers, DrainRange, AlpnWithoutTls, TlsOnFastCgi, SchemeTrustWithoutSource,
    AddressResolve, TlsWithoutAcceptor, BadHeaderName, IngressNeedsHttp, ReserveOverflow, TooMany,
    // warnings
    DeclaredSchemeHttp, MinRateOff, PanicCatchInAbortBuild, OffloadOnBlocking, ShortIdleBehindProxy,
    LockdownWithoutCritical, NoListeners,
}
impl RuleId { pub const fn as_str(self) -> &'static str; }   // stable snake_case name for logs
```

| Rule | Refuses | Override |
|---|---|---|
| `PublicBind` | a TCP listener with `Proxy` or `FastCgi` ingress on an unspecified (`0.0.0.0`, `::`) or public address. Allowed without override: loopback, private ranges (`10/8`, `172.16/12`, `192.168/16`, `fc00::/7`), link-local, Unix paths, descriptor 0, inherited and handed-off sockets | `allow_public_bind(reason)` |
| `ForwardedOnDirect` | forwarded-header trust on a `Direct` listener | none |
| `FastCgiTrust` | a FastCGI TCP listener with an empty trust list | none |
| `TrustWildcard` | a trust entry of prefix length 0 | none |
| `TrustTooBroad` | IPv4 entries shorter than /8, IPv6 entries shorter than /32 | `allow_broad_trust(reason)` |
| `TwoClientSources` | PROXY and forwarded headers both as the client source | none |
| `TrustEmpty` | a proxy listener with no trusted entries | none |
| `TrustParse` | a trust entry in text (Tier 1) that is not an address or CIDR, or has host bits set | none |
| `WorldWritableSocket` | a Unix socket mode writable by others | `allow_world_writable(reason)` |
| `PeerCredentialsUnavailable` | a uid or gid allowlist where the platform has no peer credentials | `UnixPeerTrust::AnyLocalPeer` |
| `DuplicateName`, `BadName` | two listeners with one name; a name outside 1 to 64 bytes of `[a-z0-9._-]` | none |
| `ProfileIdReserved`, `ProfileGap`, `UnknownProfile` | `ProfileId(255)`; ids not dense from 0 or repeated; a listener naming a missing profile | none |
| `DeadlineOrder`, `DeadlineRange` | `request_timeout` below `head_timeout`; keep-alive idle 0 with keep-alive on; a value outside its range | none |
| `ZeroCap` | a connection or in-flight cap of 0 | none |
| `DuplicateBind` | two listeners on one address or path | none |
| `DriverUnavailable`, `ReloadWorkers` | a driver this build or platform lacks; a reload that changes `Workers` | restart instead |
| `DrainRange` | a drain deadline of 0 or above 1 h | none |
| `AlpnWithoutTls`, `TlsOnFastCgi`, `TlsWithoutAcceptor` | ALPN without TLS; TLS on FastCGI; TLS with no acceptor | none |
| `SchemeTrustWithoutSource` | `trust_scheme` or `trust_host` where no source can carry them | none |
| `AddressResolve` | an address that resolved to nothing | none |
| `BadHeaderName` | a `SingleAddress` field name that is not a token | none |
| `IngressNeedsHttp` | forwarded headers or FastCGI on a listener whose profile serves a custom family | none |
| `ReserveOverflow` | lane reserves summing above 100% | none |
| `TooMany` | more than 64 listeners or 255 profiles | none |

The last group are warnings, kept in `Built::warnings()`: a proxy listener left at `Scheme::Http` with `trust_scheme` off; `min_rate` 0; `catch_service_panics` in an abort build; `Offload` on the blocking driver; keep-alive idle under 5 s behind a proxy (front-end reuse races); `Lockdown`-relevant configuration with no critical profile; a configuration with no listener. P0, Tier 2, `0.3.0`.

### `Subject`

```rust
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Subject { Server, Listener(String), Profile(ProfileId) }
impl fmt::Display for Subject;   // "server", "listener web", "profile 1"
```

P0, Tier 2, `0.3.0`.

### `EndReason`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EndReason {
    Transport(CloseReason),        // net-io decided: peer closed, idle, read or write timeout, slow peer, kill, ...
    HeadTimeout, RequestTimeout, ReplyTimeout,
    ClientClose, RequestLimit, MaxAge, Drained,
    ProtocolError, IngressRefused, CapacityRefused, Shed, Denied, CheckpointKill,
    ServiceClosed, ServicePanic, ReplyDropped, NoEngine,
}
impl EndReason {
    pub const fn as_str(self) -> &'static str;   // stable snake_case name for logs
    pub fn is_abortive(self) -> bool;
}
```

Every connection ends with exactly one reason, reported once to `Observer::connection_ended`.

| Reason | Cause | What an operator can change |
|---|---|---|
| `Transport(reason)` | net-io closed it (see net-io's `CloseReason`) | the matching net-io limit |
| `HeadTimeout` | no complete head within `head_timeout` | raise it only for very slow links; usually slowloris |
| `RequestTimeout` | the body did not finish within `request_timeout` | raise it for large uploads, or use a profile with a longer one |
| `ReplyTimeout` | a deferred reply did not come within `reply_timeout` | the service's slow dependency |
| `ClientClose` | the client asked to close (`Connection: close`, HTTP/1.0) | none |
| `RequestLimit`, `MaxAge` | `max_requests` or `max_age` reached | `KeepAlive` |
| `Drained` | finished after a drain started | none |
| `ProtocolError` | the engine refused a malformed request | none; the client sent it |
| `IngressRefused` | a forwarded header or FastCGI parameter was missing or malformed, or a peer was not on the list | the front end's configuration |
| `CapacityRefused`, `Shed` | a soft cap or admission refused | raise the caps, add workers, or accept the shed |
| `Denied`, `CheckpointKill` | `Checkpoint` denied or killed | the checkpoint's policy |
| `ServiceClosed`, `ServicePanic`, `ReplyDropped` | the service returned `Close`, panicked, or dropped a token | the service |
| `NoEngine` | ALPN chose a protocol with no engine | the profile's engines |

P0, Tier 2, `0.3.0`.

---

## Engines and families (Tier 3)

Module `serve_net::engine`. For programs that serve a protocol other than HTTP (a database wire format, an RPC protocol) with the same listeners, deadlines, admission, and drain, and for adapters of further engines.

### `Engine`, `EngineEvent`, `EngineProgress`

```rust
pub trait Engine: Send + 'static {
    type Head<'a>: Copy where Self: 'a;

    fn feed(&mut self, bytes: &[u8]) -> usize;                 // copies what fits; returns the count taken
    fn feed_eof(&mut self);
    fn poll(&mut self) -> Result<EngineEvent<'_>, EngineError>;
    fn head(&self) -> Option<Self::Head<'_>>;                  // the current request's head
    fn declared_length(&self) -> Option<u64>;                  // the current request's declared body length
    fn expects_continue(&self) -> bool { false }
    fn head_len(&self) -> usize { 0 }                          // bytes of the current head, for accounting
    fn keep_alive(&self) -> bool;                              // can the connection carry another request
    fn progress(&self) -> EngineProgress;
    fn send_continue(&mut self, out: &mut OutBuf) -> Result<(), EngineError> { let _ = out; Ok(()) }
    fn refuse(&mut self, refusal: &Refusal, out: &mut OutBuf);  // never fails: an engine that cannot write closes
    fn close_after_reply(&mut self);
    fn requests_served(&self) -> u64;
    fn remaining_input(&self) -> &[u8];                        // after an upgrade
    fn reset(&mut self);                                       // reuse from a pool for a new connection
    fn shrink_idle(&mut self) -> bool { false }                // release memory while idle
}

#[non_exhaustive]
#[derive(Debug)]
pub enum EngineEvent<'a> { Head, Body(&'a [u8]), BodyEnd, NeedInput, AwaitingReply, Upgraded, Closed }

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineProgress { Idle, Head, Body, AwaitingReply, WritingReply, Upgraded, Closed }
```

A sans-IO protocol engine: bytes in, events out; replies in, bytes out. `EngineEvent::Head` carries no value: the head is read with `head()`, so the event's borrow of the engine ends before serve-net reads it (D-036). serve-net does every read and write: it feeds the engine from net-io's input buffer, polls until `NeedInput` (or `AwaitingReply`, `Upgraded`, `Closed`), and writes what the engine encoded. `progress` drives the deadlines (`Head` starts the head deadline, `Body` the body under the request deadline). The shape follows http-net's `ServerConn`, ws-net's `Connection`, and fcgi-net's responder, which all feed and poll. P0, Tier 3, `0.3.0`.

### `EngineError`, `Refusal`, `RefusalKind`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineError { pub kind: RefusalKind, pub status: u16 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal { pub kind: RefusalKind, pub status: u16, pub retry_after: Option<Duration> }

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefusalKind { Malformed, TooLarge, HeadTimeout, BodyTimeout, ReplyTimeout, Busy, Draining, Denied, Ingress, Internal }
impl RefusalKind { pub const fn as_str(self) -> &'static str; }
```

A refusal serve-net asks the engine to write: the engine formats it for its protocol. HTTP writes the status with a fixed short body (for example `400 Bad Request`), `connection: close`, and `Retry-After` when given; nothing names the internal reason, and the bytes are prebuilt per worker so a refusal allocates nothing. FastCGI writes the same as CGI-style output and ends the request. An `EngineError` from `poll` is a peer error: serve-net writes one refusal with its status and closes. P0, Tier 3, `0.3.0`.

### `OutBuf`

```rust
pub struct OutBuf { /* pooled Vec<u8> */ }
impl OutBuf {
    pub fn push(&mut self, bytes: &[u8]);
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn as_slice(&self) -> &[u8];
}
impl http_net::Output for OutBuf;   // feature http1: engines encode straight into it
```

Where an engine encodes a reply. A per-worker pooled buffer that grows to `max_pending_output` and shrinks to 16 KiB on return, zeroed on return when the profile wipes. serve-net pushes its whole content into net-io's output before writing, so the egress check sees the whole reply. P0, Tier 3, `0.3.0`.

### `Family`, `HttpFamily`

```rust
pub trait Family: Send + Sync + 'static {
    type Engine: Engine;
    type Head<'a>;                      // the head view a service sees
    type Request<'a>;                   // the request a service answers
    type Parts: Default + Send + 'static;   // the reply data a service fills

    fn engine(&self, profile: &Profile, limits: &Limits) -> Self::Engine;
    fn head<'a>(&self, raw: <Self::Engine as Engine>::Head<'a>, cx: &HeadContext<'_>)
        -> Result<(Self::Head<'a>, ClientInfo), EngineError>;           // the view and the request's client
    fn request<'a>(&self, head: Self::Head<'a>, body: &'a [u8]) -> Self::Request<'a>;
    fn new_parts(&self) -> Self::Parts;
    fn clear_parts(&self, parts: &mut Self::Parts, wipe: bool);
    fn respond(&self, engine: &mut Self::Engine, parts: &mut Self::Parts, unix_secs: u64, out: &mut OutBuf)
        -> Result<u16, EngineError>;                                    // the status written
}

pub struct HeadContext<'c> { /* the connection's client and the listener's ingress rules */ }
impl HeadContext<'_> {
    pub fn client(&self) -> &ClientInfo;
    pub fn resolved(&self) -> Option<&ClientInfo>;   // set when the view is built again for the call (D-040)
}

pub struct Reply<F: Family> { /* F::Parts and the link to its connection */ }
impl<F: Family> Reply<F> {
    pub fn parts(&self) -> &F::Parts;
    pub fn parts_mut(&mut self) -> &mut F::Parts;
    pub fn defer(&mut self) -> ReplyToken<F>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HttpFamily;   // feature http1: Head = HttpHead, Request = HttpRequest, Parts = HttpParts
impl Family for HttpFamily;
pub struct HttpParts { /* opaque: services use HttpReply's methods */ }
```

A family is a set of engines that produce the same request view; that is what makes "one request regardless of ingress" a type fact. The HTTP family (http1, FastCGI, h2) is built in. A custom family brings its own engine, head, request, and reply types; its listeners use the socket-level ingress modes (direct, PROXY, Unix), since forwarded headers and FastCGI parameters are HTTP's (`RuleId::IngressNeedsHttp`). A service fills a `Reply<F>`: the family's reply data plus the link serve-net needs to defer it. Replies are pooled per worker: `clear_parts` readies one for reuse. P0, Tier 3, `0.3.0`.

### `Service`

```rust
pub trait Service<F: Family>: Send + Sync {
    const WORK: WorkClass = WorkClass::Quick;
    fn head(&self, head: &F::Head<'_>, env: &mut Envelope) -> HeadAction { let _ = (head, env); HeadAction::Proceed }
    fn call(&self, request: &F::Request<'_>, env: &Envelope, reply: &mut Reply<F>) -> Outcome;
    fn done(&self, env: &Envelope, finished: &Finished) { let _ = (env, finished); }
}
impl<S: HttpService> Service<HttpFamily> for S;
```

The generic form of [`HttpService`](#httpservice): the same calls in the same order, over a family's types. Every `HttpService` is a `Service<HttpFamily>`. P0, Tier 3, `0.3.0`.

```rust,ignore
// A line protocol: each request is one line, each reply one line.
struct Lines;
impl serve_net::engine::Family for Lines { /* engine: a line splitter over a fixed buffer */ }
struct Echo;
impl serve_net::engine::Service<Lines> for Echo {
    fn call(&self, line: &&[u8], _env: &serve_net::Envelope, reply: &mut serve_net::engine::Reply<Lines>) -> serve_net::Outcome {
        reply.parts_mut().extend_from_slice(line);   // Lines::Parts = Vec<u8>
        serve_net::Outcome::Done
    }
}
let built = serve_net::Server::builder()
    .listener(serve_net::ListenerSpec::tcp("lines", "127.0.0.1:7000").profile(serve_net::ProfileId(0)))
    .profile(serve_net::Profile::new(serve_net::ProfileId(0)).engine(serve_net::EngineChoice::Custom { alpn: &[] }))
    .build()?;
let running = built.bind_family(Lines, Echo)?.open()?;
```

### `NoCustom`

```rust
pub enum NoCustom {}   // uninhabited
impl Engine for NoCustom;
```

The custom engine slot of a server that serves no custom family. Being uninhabited, the slot's `Custom` variant costs nothing. P0, Tier 3, `0.3.0`.

### `engine::Http1`

The HTTP/1.1 engine: an adapter over `http_net::ServerConn` with the profile's `http_net::Limits` (from `Limits::engine_limits`). Adds `Date` from a per-worker cache (formatted once per second) unless the reply set it; never adds `Server`. Maps every `HttpError` to a refusal with the error's status. Turns on `ServerConn::set_wipe` when the profile wipes, and releases its buffer while idle (`shrink_idle`). P0, Tier 3, `0.3.0`, feature `http1`.

### `engine::FastCgi`

The FastCGI responder: an adapter over fcgi-net. One request at a time per connection; answers `FCGI_GET_VALUES`; an `FCGI_ABORT_REQUEST` ends the request with `RequestEnd::PeerGone` and cancels a pending `ReplyToken`. P1, Tier 3, `0.4.0`, feature `fastcgi`.

### `engine::WebSocket`

```rust
pub struct WebSocketConn { /* the ws-net connection and its socket */ }
```

The WebSocket engine after an upgrade: an adapter over `ws_net::Connection::server()` with the target profile's limits; ping handling stays in ws-net; a drain closes with 1001 ("going away"). `WebSocketConn` (from `UpgradedConn::into_websocket`) sends and receives messages through the worker. P1, Tier 3, `0.4.0`, feature `ws`.

### `engine::H2`

The HTTP/2 engine over h2-net: one connection, many streams; each stream becomes an `HttpRequest` with its own `Envelope` (`RequestSeq` is the stream id); flow control and settings limits from the profile. The engine trait, `Envelope`, and `ReplyToken` already allow several requests in flight per connection. P3, Tier 3, after `1.0.0`, or earlier as a minor version when h2-net reaches its 0.3. Feature `h2`.

---

## Snapshots

### `Snapshot`

```rust
pub struct Snapshot<T> { /* version: AtomicU64, current: Mutex<Arc<T>> */ }
impl<T: Send + Sync> Snapshot<T> {
    pub fn new(value: T) -> Self;
    pub fn publish(&self, value: T);          // rare: a reload
    pub fn load(&self) -> Arc<T>;             // takes the lock; not for hot paths
    pub fn reader(&self) -> SnapshotReader<T>;
    pub fn version(&self) -> u64;
}
```

A table that can be swapped live: readers see a whole old value or a whole new one, never a mix. serve-net reads its profile table, per-listener ingress and trust, and limits through snapshots, so a reload never restarts anything. A writer that panics cannot leave the value half-written (the value is a whole `Arc`), so a poisoned lock is recovered, never unwrapped. Exposed for wrappers that keep their own live tables the same way. P0, Tier 3, `0.3.0`; `publish` and `load` about 20-50 ns [est.].

### `SnapshotReader`

```rust
pub struct SnapshotReader<T> { /* cached Arc and version */ }
impl<T: Send + Sync> SnapshotReader<T> {
    pub fn get(&mut self, source: &Snapshot<T>) -> &T;   // one acquire load when nothing changed
}
```

One per worker. `get` compares the source's version with the cached one; only when it moved does it take the lock and clone the new `Arc`. The old value is freed when the last reader drops it. P0, Tier 3, `0.3.0`; under 2 ns when nothing changed [est.].

```rust
use serve_net::Snapshot;

let table = Snapshot::new(vec!["a", "b"]);
let mut reader = table.reader();
assert_eq!(reader.get(&table).len(), 2);
table.publish(vec!["a", "b", "c"]);
assert_eq!(reader.get(&table).len(), 3);
```

---

## Testing services

```rust
pub mod testing {
    pub fn respond<S: HttpService>(service: &S, request: &[u8]) -> TestReply;

    #[must_use]
    pub struct TestRequest<'a> { /* raw bytes, client, scheme, profile */ }
    impl<'a> TestRequest<'a> {
        pub fn new(request: &'a [u8]) -> Self;
        pub fn client(self, addr: SocketAddr) -> Self;      // default 127.0.0.1:40000
        pub fn scheme(self, scheme: Scheme) -> Self;
        pub fn profile(self, profile: Profile) -> Self;
        pub fn send<S: HttpService>(self, service: &S) -> TestReply;
    }

    pub struct TestConnection<S: HttpService> { /* one simulated keep-alive connection */ }
    impl<S: HttpService> TestConnection<S> {
        pub fn new(service: S) -> Self;
        pub fn with_profile(service: S, profile: Profile) -> Self;
        pub fn behind(service: S, source: ProxySource, profile: Profile) -> Self;   // from a proxy at 127.0.0.1
        pub fn send(&mut self, bytes: &[u8]) -> Vec<TestReply>;
        pub fn send_into(&mut self, bytes: &[u8], out: &mut Vec<u8>);              // no allocation once warm
        pub fn is_open(&self) -> bool;
    }

    #[non_exhaustive]
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct TestReply { pub status: u16, pub headers: Vec<(String, Vec<u8>)>, pub body: Vec<u8>, pub closes: bool }
    impl TestReply {
        pub fn header(&self, name: &str) -> Option<&[u8]>;
        pub fn text(&self) -> &str;                          // the body as UTF-8, "" when it is not
    }
}
```

Calls a service with raw request bytes and returns what it answered, without sockets or threads: the request goes through the real HTTP engine, the real head decision, body policy, and reply encoding, with the default hooks. For unit tests of services built on serve-net. A malformed request returns the refusal serve-net would send. P1, Tier 2, `0.3.0`, feature `http1`.

```rust
use serve_net::testing::{respond, TestRequest};

let site = serve_net::handler(|req, reply| {
    if req.client().addr().is_some_and(|a| a.is_loopback()) {
        reply.text(200, "local");
    } else {
        reply.text(403, "no");
    }
});
assert_eq!(respond(&site, b"GET / HTTP/1.1\r\nHost: a\r\n\r\n").text(), "local");
let remote = TestRequest::new(b"GET / HTTP/1.1\r\nHost: a\r\n\r\n").client("198.51.100.4:5000".parse().unwrap());
assert_eq!(remote.send(&site).status, 403);
// Malformed input never reaches the service.
assert_eq!(respond(&site, b"GET / HTTP/1.1\r\n\r\n").status, 400);
```

---

## Thread safety and sizes

| Type | `Send` | `Sync` | Size target |
|---|---|---|---|
| `Running`, `Bound`, `DrainHandle` | yes | yes | an `Arc` and a few words |
| `ServerBuilder`, `Built`, `ListenerSpec`, `Profile`, `Limits`, `KeepAlive`, `Workers` | yes | yes | cold path |
| `ClientInfo` | yes | yes | at most 64 bytes [est.], `Copy` |
| `Envelope` | yes | yes | about 96 bytes [est.] |
| `HttpHead`, `HttpRequest` | yes | yes | a few references (borrowed views) |
| `HttpReply`, `ReplyToken`, `DetachedHead` | yes | yes | pooled buffers |
| `OutBuf` | yes | no (worker-local) | |
| `Snapshot<T>` | yes (`T: Send + Sync`) | yes | |
| `SnapshotReader<T>` | yes | no (one per worker) | |
| `EndReason`, `RuleId`, `RefusalKind`, `Scheme`, `IngressKind`, `ProfileId` | yes | yes | at most 16 bytes, `Copy` |

serve-net keeps no global mutable state and no request state in thread-locals: everything is per connection, per worker, or in the shared state of one `Running`.

---

## Recipes

| Task | Recipe |
|---|---|
| Hello world in a browser | [`serve_net::http`](#http), open `http://127.0.0.1:8080` |
| Behind Apache or nginx on the same host | [`serve_net::http_behind_proxy`](#http_behind_proxy) |
| Behind a proxy that terminates TLS | `ListenerSpec::scheme(Scheme::Https)`, or `ProxySource::trust_scheme(true)` when it sends `X-Forwarded-Proto` |
| Bind a privileged port, drop privileges, then accept | `build`, `bind`, drop privileges, `open` ([`Built::bind`](#builtbind-bound)) |
| Graceful shutdown on a signal | a `DrainHandle` moved to the thread that sees the signal; `Running::wait` in `main` |
| An admin listener that survives floods | a second profile with `Lane(1)`, `critical(true)`, a small budget; `InFlightCap::reserve` (0.5) |
| Refuse requests while overloaded | `ControlState::Shed { from_lane }` on the control line |
| Revoke a request in flight | a [`Checkpoint`](#checkpoint-checkpointat-verdict-nocheckpoint) |
| Slow work without holding a worker | `WorkClass::Io` or `Cpu`, `reply.defer()`, complete the token from the other thread ([`ReplyToken`](#replytoken-completeerror)); or the [`Offload`](#offload-offloadpool-offloadjob) dispatcher (0.5) |
| Per-request records owned by a wrapper | take a slot in `Admit`, store its index with `Envelope::set_user_word`, return it in `done` |
| Unit-test a service | [`serve_net::testing`](#testing-services) |
| Deterministic timeout tests | `ServerBuilder::clock` with `net_io::ManualClock` |
| Restart without dropping connections | the supervisor hands the sockets to the new process (`ListenerSpec::handed_off`, 0.4); the old one calls `close_door` and `drain` |
| systemd | `ListenerSpec::inherited`, `SystemdNotify` (0.4) |
| A non-HTTP protocol | a [`Family`](#family-httpfamily) and a [`Service`](#service), `bind_family` |

---

## Names changed at the design lock

The specification sketch was reconciled with net-io's 0.2 design lock and http-net 0.6. Each change has a decision in [`../dev/PLAN.md`](../dev/PLAN.md) section 16.

| Sketch | Now | Decision |
|---|---|---|
| `Built::bind()`, `Bound::open(service)` | `Built::bind(service)`, `Bound::open()`, plus `ServerBuilder::serve(service)` | D-008 |
| serve-net's `Control` trait, `ControlLine` struct, `Directive { Normal, Shed, Restrict, Drain, Stop }` | net-io's `ControlLine` trait and `ControlState { Normal, Shed, Lockdown, Draining, Stopping }`, re-exported | D-009 |
| `LaneId` with `CRITICAL`, `NORMAL`, `BULK` | `net_io::Lane`, re-exported as `Lane` (default request lane `Lane(4)`) | D-010 |
| `Hooks` with six types | eight types: net-io's `Gate`, `EventSink`, `ControlLine`, `TlsAcceptor` join serve-net's `Admit`, `Dispatch`, `Checkpoint`, `Observer`; `Notify` boxed | D-011, D-012 |
| `Observer::listener_opened`, `listener_closed`, `accept_error`, `connection_refused` | net-io's transport events through the owner's event sink | D-012 |
| `AcceptPolicy` | removed (net-io's accept loop); `accept_failure_window` on the builder | D-015 |
| `CloseMode { Release, KeepForHandOff }`, `close_door(mode)` | `close_door()` | D-016 |
| `Limits::min_body_rate`, `min_write_rate` | one `Limits::min_rate` (net-io's meter covers both directions) | D-014 |
| `Workers::stack_size`, `timer_tick`, `names`, `DriverChoice` | net-io's (`transport_limits` for the tick); `Workers::kind()` returns `net_io::DriverKind` | D-014 |
| `Admit::Worker`, `admit(&self, &mut Worker, env)` | `admit(&self, WorkerId, env)`, `prepare(&mut self, workers, profiles)` | D-022 |
| `InFlightCap::with_reserved`, `shed_floor` | `reserve(lane, percent)`; the shed floor comes from `ControlState::Shed` | D-022, D-009 |
| `Running<S, H>`, `Running::control()` | `Running` (not generic); the owner keeps its own `Arc` to the line | D-026 |
| `Running::drain(&self, ..)` | `Running::drain(self, ..)` | D-027 |
| `Ingress::Direct { tls: Option<TlsConfig> }` | `Ingress::Direct`, `Ingress::DirectTls` with `ServerBuilder::tls(acceptor)` | net-io's TLS seam |
| `ProxySource` enum with struct variants | `ProxySource` struct with constructors and setters | D-029 |
| `ClientInfo::peer() -> net_io::PeerAddr` | `peer() -> Option<SocketAddr>`, `peer_credentials()` | D-028 |
| closure blanket impl of `HttpService` | `serve_net::handler(closure)`; Tier 1 functions take closures directly | D-023 |
| `HeadAction::Proceed { body }`, `proceed()` | `Proceed`, `ProceedWith(policy)`, `refuse(status)` | readability |
| `Outcome::Deferred(ReplyToken)` | `reply.defer()` returns the token; `Outcome::Deferred` carries nothing | D-034 |
| `Family::Reply`, `HttpReply` as its own struct | `Family::Parts`; `Reply<F>` wraps it with the link to the connection; `HttpReply = Reply<HttpFamily>` | D-034 |
| `Counters` observer, `stats() -> Option<Stats>` | `stats() -> Stats`, always available | D-033 |
| `ServerConfig: Clone`, `ListenerSpec: Clone` | not `Clone` (a handed-off socket is move-only) | D-035 |
| `Profile::wipe_buffers` default off | default on | D-021 |
| `Control::register_worker` | `ServerHandle::wake_all` (net-io) | D-009 |
