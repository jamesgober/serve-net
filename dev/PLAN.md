# serve-net - Project Plan

> The design, strategy, and decision record for `serve-net`. A living document: every design decision, idea, or open question about serve-net is written here or in [`ROADMAP.md`](./ROADMAP.md) before it is acted on.
>
> **Read order:** [`../REPS.md`](../REPS.md) &rarr; [`DIRECTIVES.md`](./DIRECTIVES.md) &rarr; **this file** &rarr; [`ROADMAP.md`](./ROADMAP.md) &rarr; [`../docs/API.md`](../docs/API.md).
>
> **Status:** `0.2.0`, design lock. The public API is written in `docs/API.md`; this file holds the design behind it and every decision taken to get there (section 16).
>
> Numbers marked **[est.]** are estimates from typical hardware behaviour. A benchmark replaces each one at `0.6.0`, and `docs/PERFORMANCE.md` then holds the measured value with method and hardware.

---

## Contents

1. [Purpose](#1-purpose)
2. [What serve-net is not](#2-what-serve-net-is-not)
3. [Position in the stack](#3-position-in-the-stack)
4. [Scope](#4-scope)
5. [Requirements](#5-requirements)
6. [Enterprise and distributed](#6-enterprise-and-distributed)
7. [Standards and approved exceptions](#7-standards-and-approved-exceptions)
8. [Design](#8-design)
9. [Security](#9-security)
10. [Performance](#10-performance)
11. [Testing](#11-testing)
12. [What serve-net needs from other crates](#12-what-serve-net-needs-from-other-crates)
13. [Ideas and experiments](#13-ideas-and-experiments)
14. [Open decisions](#14-open-decisions)
15. [Superseded plan text](#15-superseded-plan-text)
16. [Decision log](#16-decision-log)

---

## 1. Purpose

serve-net is the engine a network service runs on. It takes listeners from net-io, learns the real client behind any front end, runs a sans-IO protocol engine over each connection, and hands one normalized request at a time to a service through one small trait. It owns everything between "a request is arriving" and "the service has it": request deadlines, keep-alive, request limits, admission, the order of the hand-off, drain, readiness, and the switch to another protocol engine after an upgrade.

It is generic. The same engine serves HTTP/1.1 through http-net, FastCGI through fcgi-net, WebSocket after an upgrade through ws-net, HTTP/2 through h2-net when that exists, and any other request-and-reply wire protocol whose engine implements serve-net's `Engine` trait (a database wire format, for example).

It is a library. It never owns `main()`. Once opened it runs live work (listeners, workers, connections) until its owner drains it.

The rule that decides what belongs here: **serve-net decides when and whether a request reaches a service, never what the service does with it.** Everything that knows a route, a credential, a contract, or a ban sits above serve-net (the service, or a wrapper built on serve-net's hooks) or beside it (a connection gate installed on net-io).

---

## 2. What serve-net is not

| Not | Because | Lives in |
|---|---|---|
| a web framework | no routing, handlers, middleware, or templates | the service; `web-net` for public users |
| a security pipeline | the order of inspection, authentication, and authorization belongs to the application that wraps serve-net | a wrapper built on serve-net's hooks |
| a transport | sockets, socket options, drivers, worker threads, the accept loop, PROXY parsing, descriptor passing, transport deadlines, the egress check, the TLS seam | `net-io` |
| a protocol parser | turning bytes into requests is a sans-IO engine's job | `http-net`, `fcgi-net`, `ws-net`, `h2-net`, a service's own engine |
| a TLS implementation | TLS is never written in-house | `tls-net` (over rustls) through net-io's TLS seam |
| a process supervisor | restarts, crash loops, memory watch, holding listening sockets during a restart | a supervisor process, systemd, the Windows service host |
| a signal handler | `std` has no safe signal API; signals belong to the owner's process kernel | the owner's code |
| a CGI host | a process start per request costs 1-10 ms [est.] and loses every in-memory defence; FastCGI covers shared hosting | none (dropped, D-006) |
| a rate limiter or ban list | per-source policy | a connection gate (bouncer-io) through net-io's gate seam |

---

## 3. Position in the stack

### 3.1 Who uses it

| User | How |
|---|---|
| a private application wrapper | implements `HttpService` with its own request pipeline, `Admit` over its own per-request slot pool, `Checkpoint` over its revocation tickets, `Dispatch` with its offload pool, `Observer` into its metrics, `Notify` towards its supervisor or systemd; owns the net-io control line and passes it in; builds serve-net's profile table from its own configuration |
| a database network layer | a non-HTTP engine in the slot (`Engine`, `Family`, `Service<F>`); profiles, deadlines, and drain unchanged |
| public users | Tier 1: `serve_net::http(addr, handler)`; Tier 2: `Server::builder()`; Tier 3: custom engines and hooks |
| `web-net` (public framework, planned) | runs inside serve-net as an `HttpService` |

### 3.2 What it uses

| Crate | Why | Required |
|---|---|---|
| `net-io` | the server: listeners, workers per driver, the door, sessions, transport limits and deadlines, caps, the gate seam, the control line, the completer, kill, drain, events, PROXY, the TLS seam, inherited and handed-off listeners, `TrustedProxies` | yes |
| `http-net` | HTTP/1.1 engine (`ServerConn`), the request view, response encoding | feature `http1` (default) |
| `fcgi-net` | FastCGI responder engine and the CGI meta-variable mapping | feature `fastcgi` |
| `ws-net` | WebSocket engine after an upgrade | feature `ws` |
| `h2-net` | HTTP/2 engine | feature `h2` (after 1.0 unless h2-net reaches 0.3 sooner) |

TLS reaches serve-net only through net-io's seam: the owner passes a `net_io::TlsAcceptor` (tls-net provides one). serve-net reads only "TLS on" and the ALPN result.

No third-party runtime dependency. No async runtime.

### 3.3 Dependency direction

```
 owner (application, wrapper, supervisor glue)
   |  installs: a net-io Gate (bouncer-io), a net-io EventSink, a net-io ControlLine,
   |            serve-net hooks (Admit, Dispatch, Checkpoint, Observer, Notify)
   v
 serve-net ---------> http-net, fcgi-net, ws-net, h2-net   (engines, sans-IO)
   |
   v
 net-io               (transport, server, sessions, gate seam, control line)
```

serve-net never depends on bouncer-io. A connection gate reaches every connection through net-io's gate (installed by the owner through serve-net's builder) and every request through serve-net's `Checkpoint` hook. serve-net stays usable without a gate, and dependencies point one way.

### 3.4 The boot and shutdown contract

serve-net separates the steps an owner orders during boot and shutdown:

| Owner step | serve-net call | Effect |
|---|---|---|
| configuration | `Server::builder()` ... `.build()` | every rule validated, every error collected; nothing bound, no thread started |
| bind | `Built::bind(service)` | listeners bound, inherited, or received; workers started with the door closed; nothing accepted. The owner may drop privileges here |
| open | `Bound::open()` | the door opens; `Notify::ready()` once every listener accepts |
| shutdown step 1 | `Running::close_door()` | stop accepting on every listener; listening sockets stay open, so a process that received them keeps the backlog |
| shutdown step 2 | `Running::drain(deadline)` | in-flight work finishes until the deadline, then forced close; `Notify::stopping()` |

Listeners open last and close first, because the owner calls `open` after everything else is ready and `close_door` before anything else stops. The service is given at `bind` rather than at `open` because net-io needs the session factory when it builds the server (D-008).

---

## 4. Scope

### 4.1 In scope

| Area | Covers |
|---|---|
| Listeners | named TCP and Unix listeners through net-io: bound by address or path, inherited (systemd `LISTEN_FDS`, mod_fcgid descriptor 0), or handed off by a supervisor; each with one ingress mode, one profile, trusted proxies where the mode needs them, and a public-bind policy |
| Ingress modes | direct (plain or TLS); reverse proxy over TCP or a Unix socket with PROXY protocol v1/v2 (parsed by net-io) or trusted forwarded headers (`Forwarded`, `X-Forwarded-For`, one named single-address header; scheme and host when enabled); FastCGI including the mod_fcgid descriptor-0 start |
| Normalization | one `ClientInfo` and one request view per protocol family (`HttpRequest` for every HTTP ingress) regardless of how the request arrived |
| Per-connection engine | a net-io `Session` that runs the protocol engine: request deadlines, keep-alive, request limits, pipelining order, refusals, the upgrade switch |
| Protocol engine slot | the `Engine` and `Family` traits; adapters for http-net, fcgi-net, ws-net (h2-net later); custom engines; ALPN selection after TLS |
| Hand-off | `HttpService` and the generic `Service<F>`: head decision, call, deferred replies, upgrade, completion |
| Seams | request state is `Send`; one `Dispatch` call before every service call (`Inline` default); `WorkClass` on every service; profiles, limits, and trust lists read through `Snapshot<T>` |
| Admission and lanes | soft connection caps (global, listener, profile) answered with a busy refusal; the `Admit` hook after the head with the default `InFlightCap`; a `Lane` on every request; reserved per-lane capacity |
| Control | the owner's net-io control line read at request checkpoints (one atomic load when unchanged) |
| Profiles | listener -> `ProfileId`; per-profile engine, lane, budget, limits, keep-alive, critical flag, opaque tag |
| Drain and restarts | close the door, drain with a deadline, forced close; socket hand-off restarts; systemd socket activation |
| Hot reload | profiles, limits, and trust lists swapped through snapshots; listeners added and removed; all-or-nothing validation |
| Readiness | the `Notify` hook; systemd `sd_notify` in safe `std` code |
| Observability | the `Observer` hook (request level, no-op default compiled away), the owner's net-io event sink (transport level), `Counters`, `health()`, `stats()` |
| Startup validation | every configuration error collected and reported at once; insecure configurations refused (section 9.3) |

### 4.2 Out of scope, and where each item lives

| Item | Lives in |
|---|---|
| inspection, authentication, authorization, per-route lanes, tenant fairness | the application or its wrapper, on serve-net's hooks |
| bans, per-source rates, revocation | a connection gate through net-io's gate seam and serve-net's `Checkpoint` |
| accept loops, worker threads, socket options, transport deadlines and minimum rates, PROXY parsing, descriptor passing, `LISTEN_FDS` parsing, the egress check | net-io |
| TLS | tls-net over net-io's TLS seam |
| CGI | dropped; the CGI meta-variable mapping stays in fcgi-net for FastCGI |
| HTTP/2 | h2-net (planned); the slot and the ALPN path are designed now |
| HTTP/3, QUIC | quic-net and h3-net (later); UDP, not a stream listener |
| outbound proxying, load balancing | proxy-net (later) |
| signals, process supervision, watchdog restarts | the owner, a supervisor, systemd, the Windows service host |
| the Windows service control dispatcher (needs FFI) | a crate with confined `unsafe` (sys-lib, OD-6); serve-net calls `Notify` at the right moments |
| compression, static files, routing, cookies | the service or web-net |
| logging | the owner, through `Observer` and the event sink; serve-net never prints |

---

## 5. Requirements

Each requirement is numbered so it can be checked off and tested; test names refer to these ids. "Default" values are final as of this design lock; the reasons are in section 8 and the comparison with other servers in `docs/API.md`.

### 5.1 Functional requirements

**Listeners**

| Id | Requirement |
|---|---|
| FR-1 | Bind TCP listeners (IPv4 and IPv6) by address through net-io, with backlog and net-io listen options passed through |
| FR-2 | Bind Unix-domain listeners by path through net-io, with mode (default `0o660`), owner and group, and net-io's stale-socket rule |
| FR-3 | Accept inherited listeners by name from systemd socket activation (net-io parses the variables). A spec naming an inherited socket that is not present is a startup error unless a fallback address is set |
| FR-4 | Accept the mod_fcgid start: a listening socket on descriptor 0, detected by net-io |
| FR-5 | Accept listening sockets handed off by a supervisor and match them to listener specs by name; a spec with no socket is a startup error |
| FR-6 | Every listener has a unique name (1 to 64 bytes of `[a-z0-9._-]`), one ingress mode, one profile, and trusted proxies where the mode needs them |
| FR-7 | Binding is separate from opening. After `bind` nothing is accepted (the OS backlog may queue connections); the owner can drop privileges between the two |
| FR-8 | `close_door` stops accepting on every listener within one worker loop iteration and keeps the sockets for a hand-off; `drain` closes them at the end |
| FR-9 | Transient accept errors back off without spinning (net-io); a listener failing for longer than `accept_failure_window` (default 30 s) makes health `Degraded`; never a panic |

**Ingress and normalization**

| Id | Requirement |
|---|---|
| FR-10 | Direct: the client is the socket peer (IPv4-mapped IPv6 normalized); the scheme is `https` when TLS is on, else `http` |
| FR-11 | PROXY protocol: net-io parses v1 and v2 from trusted peers only and closes connections from untrusted peers or with a missing or malformed header. serve-net takes the client from net-io's real peer; a v2 `LOCAL` command means the proxy itself, flagged by `ClientInfo::is_proxy_local()` |
| FR-12 | Forwarded headers: exactly one configured source (`Forwarded` per RFC 7239, `X-Forwarded-For`, or one named single-address header). Applied only when the socket peer is trusted. Walked right to left; trusted entries are skipped; the first untrusted entry is the client; if every walked entry is trusted, the leftmost is the client. At most `max_hops` entries (default 16) are walked; more is a refusal. A malformed entry is a refusal (400) |
| FR-13 | A trusted peer that sends no forwarded header is refused (400) by default; `MissingHeader::UsePeer` treats the peer as the client |
| FR-14 | Scheme and authority come from the forwarded headers only when `trust_scheme` / `trust_host` is set and the peer is trusted; `proto` must be `http` or `https`, `host` a valid RFC 3986 authority; anything else is a refusal |
| FR-15 | Unix-socket ingress: the peer is trusted by peer credentials (uid and gid allowlist) or by an explicit `AnyLocalPeer`; then PROXY or forwarded headers as on TCP |
| FR-16 | FastCGI ingress: client from `REMOTE_ADDR` / `REMOTE_PORT`, scheme from `HTTPS` / `REQUEST_SCHEME`, authority from `HTTP_HOST` or `SERVER_NAME` and `SERVER_PORT`; trusted only when the FastCGI peer is trusted; `FCGI_WEB_SERVER_ADDRS` further restricts peers when set |
| FR-17 | For every HTTP-family ingress the service sees the same `HttpRequest` and the same `ClientInfo` for equivalent input |
| FR-18 | Forwarded headers are never trusted on a direct listener; such a configuration is refused |
| FR-19 | Client addresses are parsed into `IpAddr` and `u16`, never kept as raw strings; IPv6 zone identifiers are refused; IPv4-mapped IPv6 is normalized to IPv4 |

**Per-connection engine**

| Id | Requirement |
|---|---|
| FR-20 | Head deadline: from accept (first request) or from the first byte of a later request, to a complete head. Default 10 s. On expiry: the engine's timeout refusal (408) if no byte of a response was sent, then close |
| FR-21 | Request deadline: from the first byte of the head to the end of the request body. Default 30 s |
| FR-22 | Minimum transfer rate while a request is received or a reply is sent: default 256 B/s over 10 s (net-io's meter); below it the connection is closed |
| FR-23 | Write progress: output that makes no progress for `write_stall` (default 30 s) closes the connection abortively (slow-read defence, net-io's write deadline) |
| FR-24 | Keep-alive idle timeout between requests: default 20 s on direct listeners, 60 s behind a front end (OD-9); on expiry the connection closes without a response |
| FR-25 | Requests per connection: default 1,000; the last allowed response carries the close signal |
| FR-26 | Connection age: default 1 h; the first response after the age carries the close signal; an idle connection past its age is closed |
| FR-27 | Pipelined requests are answered in order. At most `max_pipelined_per_wake` (default 16) requests are processed per input event; pending output is bounded by `max_pending_output` (default 256 KiB): input is not processed until it drains |
| FR-28 | `Expect: 100-continue` is answered only after `Service::head` accepted the request and `Admit` admitted it; otherwise the final status is sent and the connection closes |
| FR-29 | A response sent before the request body was read closes the connection afterwards |
| FR-30 | Lingering close after a closing response with unread input, bounded by `linger` (default 2 s, 64 KiB, net-io's graceful linger) |
| FR-31 | Every connection end carries one `EndReason`, reported to the `Observer` |
| FR-32 | Upgrade hand-off: when the service accepts an upgrade, the engine writes the switch (101), serve-net replaces the engine (ws-net or a custom engine), feeds the bytes that arrived with the request, applies the target profile's limits, and keeps the connection on its worker. `Upgrade: h2c` is never honoured |
| FR-33 | After TLS, ALPN selects the engine (`http/1.1`; `h2` with feature `h2`); no ALPN result uses the profile's engine; an ALPN result with no compiled engine closes the connection |
| FR-34 | The HTTP adapter adds a `Date` field from a per-worker cached value unless the service set one; no `Server` field is added |

**Hand-off to the service**

| Id | Requirement |
|---|---|
| FR-35 | `head` is called once per request when the head is complete, before admission and before any body byte is read; it returns proceed (with a body policy), refuse (status), or upgrade |
| FR-36 | Body policies: `Buffer { max }` (default; the whole body is read under the deadlines before `call`), `Stream` (chunks pushed as they arrive), `Discard { max }` |
| FR-37 | `call` receives the request, the `Envelope`, and the reply; it returns `Done`, `Deferred(ReplyToken)`, or `Close` |
| FR-38 | Deferred replies: a `ReplyToken` completes from any thread; the completion is posted to the owning worker through net-io's completer; the connection is parked meanwhile; deadlines still apply; a per-worker cap bounds parked requests |
| FR-39 | `done` is called exactly once per request that reached `head`, with the outcome and timings |
| FR-40 | Refusals produced by serve-net use the engine's generic refusal bytes: status, a fixed body that never names the reason, and the close signal |

**Seams**

| Id | Requirement |
|---|---|
| FR-41 | `Envelope`, `ReplyToken`, `ClientInfo`, and `DetachedHead` are `Send`, checked at compile time; serve-net keeps no request state in thread-locals |
| FR-42 | One `Dispatch` call sits between "request complete" and the service call; the default `Inline` calls the service directly |
| FR-43 | Every service declares a `WorkClass` (`Quick` default, `Io`, `Cpu`); `Dispatch` implementations use it; `Quick` always runs in place |
| FR-44 | Profiles, limits, and trust lists are read through `Snapshot<T>`; a reader sees a whole old or a whole new table; the refresh check is one atomic load when nothing changed |

**Admission and lanes**

| Id | Requirement |
|---|---|
| FR-45 | Soft connection caps (global, per listener, per profile) are checked when net-io hands serve-net a connection; over a cap the busy refusal (503 with `Retry-After`) is written without reading the request, then the connection closes. net-io's hard caps sit above them and reset connections beyond (D-013) |
| FR-46 | Request admission runs after the head is complete and `head` has classified the request, before any body byte is read: control checkpoint, `head`, `Admit::admit`, `Checkpoint`, 100-continue, body, `Dispatch`, call |
| FR-47 | The default `Admit` is `InFlightCap`: a real per-worker and per-profile in-flight cap (reactor 1,024 per worker; blocking: one per thread). A refusal is the busy refusal with `Retry-After` = base plus jitter in `[0, base)` (default base 1 s) |
| FR-48 | Every request carries a `Lane` from its profile; `head` may change it. Reserved per-lane capacity inside `InFlightCap`: lanes without a reservation never take reserved slots |
| FR-49 | Under the control state `Shed { from_lane }`, admission refuses requests in that lane and every less important one, and new responses carry the close signal |

**Control**

| Id | Requirement |
|---|---|
| FR-50 | The owner's control line is read at request checkpoints: at head complete, before dispatch, before a reply is written, and when a connection returns to idle (net-io reads it before each accept batch). When unchanged, a checkpoint is one atomic load and compare |
| FR-51 | `Normal`; `Shed` (FR-49); `Lockdown` (only profiles marked critical admit requests); `Draining` (as FR-52); `Stopping` (abortive close of everything now) |

**Drain and restarts**

| Id | Requirement |
|---|---|
| FR-52 | `drain(deadline)`: close the door; idle keep-alive connections close at once; responses in flight carry the close signal; deferred and streaming requests continue until the deadline; then remaining connections are closed abortively; a `DrainReport` gives counts. A later shorter deadline wins, a longer one is ignored |
| FR-53 | Socket hand-off restart: the new process receives the sockets and opens; the old process closes its door and drains. The old process never closes a listening socket in a way that drops connections queued in the shared backlog |
| FR-54 | Zero connections refused or reset during a hand-off restart under steady load (restart test T-RST) |

**Hot reload**

| Id | Requirement |
|---|---|
| FR-55 | `reload(config)`: full validation first; listeners diffed by name; new listeners bound before anything changes; any failure rolls back and the old configuration stays; then snapshots are published; removed listeners close their door and drain with the drain deadline |
| FR-56 | Connection-level limits apply to connections accepted after the swap; request-level limits apply to requests whose head completes after the swap; work in flight keeps the table it started with |
| FR-57 | `Notify::reloading(monotonic_usec)` before and `Notify::ready()` after a reload |

**Workers**

| Id | Requirement |
|---|---|
| FR-58 | Blocking driver: a bounded number of connection threads (net-io). Under pressure keep-alive is shortened (above `under_pressure_at`, default 0.75 of threads busy) and above 0.9 every response carries the close signal |
| FR-59 | Reactor driver (net-io 0.5): one worker per core, shared-nothing, zero CPU when idle |
| FR-60 | io_uring driver when net-io provides it |
| FR-61 | A panic in a service call: with `panic = "unwind"` and `catch_service_panics` on, the panic is caught, the request answered with the 500 refusal, the connection closed, and `done` called; with `panic = "abort"` the process aborts and the supervisor restarts it |

**Readiness, health, observability**

| Id | Requirement |
|---|---|
| FR-62 | `Notify`: `ready`, `reloading`, `stopping`, `status`, `watchdog`, `extend_timeout`. `SystemdNotify` sends one `SOCK_DGRAM` datagram per call to `NOTIFY_SOCKET` (a path, or `@` for the Linux abstract namespace) using only safe `std`; an unset variable makes every call a no-op |
| FR-63 | Watchdog pinger: when `WATCHDOG_USEC` is set (and `WATCHDOG_PID`, if set, matches), ping at half the interval and only if every worker progressed or is idle |
| FR-64 | `Observer` events: request head, admitted, shed, finished (with timings); connection ended (`EndReason`, request count); drain progress; reload result; public-bind exceptions. The default observer is a no-op compiled away |
| FR-65 | `health()` returns `Ok`, `Degraded`, or `Failing` with reasons; `stats()` returns a counters snapshot when `Counters` is installed |

**Validation**

| Id | Requirement |
|---|---|
| FR-66 | `build()` collects every configuration error into `ConfigErrors`, each naming the listener or profile and saying what to change |
| FR-67 | Insecure configurations are refused (section 9.3) |
| FR-68 | The same validation runs on reload; an invalid reload changes nothing |

**Tier 1**

| Id | Requirement |
|---|---|
| FR-69 | `serve_net::http(addr, handler)`, `serve_net::http_behind_proxy(addr, trusted, handler)`, and `serve_net::fastcgi(listen, handler)` start a working server in one call with safe defaults and return a `Running` |

### 5.2 Non-functional requirements

| Id | Requirement | How checked |
|---|---|---|
| NFR-1 | serve-net's own work per keep-alive HTTP request (excluding engine parse and encode, the service, and system calls) at most 150 ns p50 on the reactor driver | criterion with a null engine and null service |
| NFR-2 | No allocation per request in steady state, proved with a counting allocator | 100k requests after warm-up |
| NFR-3 | Memory per idle keep-alive connection on the reactor at most 4 KiB, excluding kernel socket buffers | 10k and 100k idle connections, RSS delta |
| NFR-4 | Zero CPU when idle: no periodic tick without connections; with idle connections, wake-ups only at deadlines | wake-up count over 60 s idle |
| NFR-5 | Throughput at least 20% above the best Rust competitor (hyper 1.x with hyper-util on tokio) on plaintext keep-alive and pipelined workloads, at equal or stronger strictness, reactor driver, reference hardware. Measured and published at 0.6 | benchmark suite (section 10) |
| NFR-6 | Under 10x offered overload with admission on, admitted requests keep p99 within 2x of the unloaded p99; a refusal costs at most 2 µs of server CPU [est.] | overload test at a constant rate |
| NFR-7 | Drain: idle connections closed at once; drain returns within deadline plus one timer tick | drain tests with a manual clock and with sockets |
| NFR-8 | Hand-off restart: zero refused or reset connections under steady load | T-RST |
| NFR-9 | Reload: new tables visible at the next checkpoint; zero connections dropped | reload test under load |
| NFR-10 | Memory bounded by configuration; nothing grows with input | soak, counting allocator |
| NFR-11 | A control checkpoint costs at most 2 ns when nothing changed | criterion |
| NFR-12 | `InFlightCap` take and release at most 5 ns uncontended [est.] | criterion |
| NFR-13 | No panics in library code; every error a `Result`; error enums `#[non_exhaustive]` | clippy, review |
| NFR-14 | Linux, macOS, Windows first class on stable and MSRV; FreeBSD in CI once net-io's reactor runs there | CI |
| NFR-15 | MSRV 1.85 (edition 2024) | CI |
| NFR-16 | `#![forbid(unsafe_code)]`; every OS call that needs `unsafe` lives in net-io | crate attribute |
| NFR-17 | Every deadline is tested with a manual clock | unit tests |
| NFR-18 | 24 h soak with mixed hostile and normal peers: RSS growth under 1% after warm-up, descriptor count flat | soak job before 0.9 |
| NFR-19 | Opening and closing 100k connections returns the descriptor count to baseline | leak test |
| NFR-20 | Validation of 64 listeners and 255 profiles in at most 1 ms [est.] | criterion |
| NFR-21 | `READY=1` sent within 1 ms after the last listener starts accepting [est.] | integration test with a fake notify socket |
| NFR-22 | Public types `Send` and `Sync` where that makes sense, checked at compile time | static assertions |
| NFR-23 | Every public item documented with a runnable example | `deny(missing_docs)`, doctests |
| NFR-24 | At most 2 system calls per keep-alive request on the reactor (one receive, one send) excluding wake-ups | syscall count |

---

## 6. Enterprise and distributed

serve-net holds per-process runtime state (open connections, in-flight counts, the drain state) and no durable state. It is therefore a stateful crate whose state is local by nature: a connection belongs to one process. What "distributed by default" means for it:

| Point | How serve-net meets it |
|---|---|
| Correct under concurrency and load | per-worker state with single-writer counters; shared state only in `Snapshot<T>`, the control line, and the soft-cap atomics; every queue bounded; soak and overload tests before 1.0 |
| Observable | `Observer` (request level) and the owner's net-io event sink (transport level), both free when unused; `health()` and `stats()`; stable `EndReason` and `RuleId` names for logs |
| Operable | every limit configurable with a safe default; behaviour under overload defined (busy refusal with jittered `Retry-After`, never a fall-over); capacity numbers documented per driver |
| Secure | fail-closed ingress; insecure configurations refused at build and reload; buffers wiped on return by default; nothing secret in errors or events |
| Long-lived | semantic versioning, deprecation before removal, MSRV policy, `docs/STABILITY-1.0.md` at 1.0 |
| State behind traits | admission (`Admit`), revocation (`Checkpoint`), dispatch (`Dispatch`), readiness (`Notify`), and the control line are traits, so a deployment can back them by shared or replicated state (a cluster-wide in-flight budget, a shared revocation list, a cluster control plane driving the control line) without changing serve-net |
| Many nodes | serve-net makes no single-process assumption beyond its own connections: the same configuration runs on every node, profiles and trust lists come from the owner (any configuration source), and restarts hand sockets over without dropping connections |
| Clock skew | serve-net uses only the monotonic clock for deadlines and never compares times across nodes |
| Idempotence | reload is idempotent (the same configuration twice changes nothing the second time); drain with the same or a longer deadline is a no-op |
| Partition | each node keeps protecting itself locally: caps, admission, deadlines, and validation never wait on another node. A hook backed by remote state must answer from local state when the remote is unreachable (fail closed for `Checkpoint`, the hook's choice for `Admit`), which `docs/API.md` states for each hook |
| Multi-node tests | the restart test runs two processes sharing one socket; a two-node reload test with a shared control line runs before 1.0 |

---

## 7. Standards and approved exceptions

The portfolio standard applies in full (REPS and `dev/DIRECTIVES.md`), with these recorded adaptations:

| Item | Adaptation | Reason |
|---|---|---|
| REPS "MUST use tokio" | not followed: no async runtime | the clause is a known mistake in REPS; the portfolio's rule is no async runtime and sans-IO cores (approved exception, D-007) |
| 20% faster than the best Rust competitor | measured on the reactor driver (net-io 0.5 and later); the blocking driver is compared with thread-per-connection servers (http-net's `blocking::Server`) | a thread per connection cannot win idle-heavy workloads against an event loop; the blocking driver is the portable first-run driver |
| no secrets of its own | `wipe_buffers` per profile zeroes serve-net's pooled buffers on return, on by default (D-021) | request and reply buffers may hold the service's secrets |
| persistence discipline | serve-net persists nothing; the "kill mid-operation" rule applies to restarts: the hand-off test kills the old process at every step | |
| `no_std` | not applicable: serve-net runs threads and clocks. The `forwarded` module has no OS dependency and is tested without sockets so it could move to a `no_std` crate | |
| FreeBSD | joins CI when net-io's kqueue reactor does | serve-net reaches OS event mechanisms only through net-io |
| release profile `panic = "abort"` | kept; `catch_service_panics` is effective only in builds that choose `unwind`, and `build` warns otherwise | |
| public files | name no private product; hooks are described generically | serve-net is public |

---

## 8. Design

### 8.1 Module map

```
src/
  lib.rs            crate docs, Tier-1 functions, re-exports, VERSION
  error.rs          ServeError, ConfigErrors, ConfigError, ConfigWarning, RuleId, Subject, EndReason
  config/
    mod.rs          ServerBuilder, Built, Bound, validation driver (collects every error)
    listener.rs     ListenerSpec, ListenerSource, UnixMode
    ingress.rs      Ingress, ProxySource, ForwardedSource, MissingHeader, UnknownClient, UnixPeerTrust, FastCgiIngress
    limits.rs       Limits, KeepAlive (serve-net's budgets; engine and transport limits stay in their crates)
    profile.rs      Profile, ProfileId, ProfileTable, Budget
    workers.rs      Workers
    validate.rs     one function per rule of section 9.3, each unit tested
  forwarded.rs      sans-IO parsers: Forwarded (RFC 7239), X-Forwarded-For, single-address header,
                    X-Forwarded-Proto, X-Forwarded-Host; the right-to-left trusted walk
  client.rs         ClientInfo, Scheme, IngressKind; normalization from net-io facts and engine facts
  envelope.rs       Envelope, RequestSeq
  engine/
    mod.rs          Engine, EngineEvent, EngineError, Refusal, RefusalKind, OutBuf, Family, Service, NoCustom
    slot.rs         the Slot enum (static dispatch)
    http1.rs        adapter over http_net::ServerConn          (feature http1)
    fastcgi.rs      adapter over fcgi-net                      (feature fastcgi)
    websocket.rs    adapter over ws-net                        (feature ws)
    alpn.rs         EngineChoice, ALPN result -> engine
  http.rs           HttpService, HttpHead, HttpRequest, HttpBody, HttpReply, HeadAction, BodyPolicy, Fields
  service.rs        Outcome, Finished, RequestEnd, WorkClass
  reply.rs          Reply<F>, ReplyToken, CompleteError, the deferred-reply slot
  detached.rs       DetachedHead
  conn/
    mod.rs          the connection state machine (section 8.4), driver independent: events in, actions out
    deadline.rs     deadline arithmetic (saturating)
    body.rs         body buffering, discard with bound
    session.rs      the net-io Session and SessionFactory over the state machine
  hooks/
    mod.rs          Hooks, HookSet, DefaultHooks
    admit.rs        Admit, Admitted, Shed, InFlightCap
    dispatch.rs     Dispatch, Inline, Offload, OffloadPool
    checkpoint.rs   Checkpoint, CheckpointAt, Verdict, NoCheckpoint
    observe.rs      Observer, NoObserver, Stats (per-worker request counters)
    notify.rs       Notify, NoNotify, SystemdNotify, WatchdogPinger
  snapshot.rs       Snapshot<T>, SnapshotReader<T>
  run.rs            Running, DrainHandle, DrainReport, Health, ListenerInfo, reload
  testing.rs        testing::respond and friends: call a service with raw request bytes, no sockets
```

Every module is `pub(crate)` unless `docs/API.md` lists its items.

### 8.2 What net-io does and what serve-net does

net-io's 0.2 design lock gives its `Server` the accept loop, the worker pools, the door, the transport deadlines, caps, kill, and drain mechanics, and drives a sans-IO `Session` per connection. serve-net therefore builds on net-io's server instead of running loops of its own (D-007):

| Concern | net-io | serve-net |
|---|---|---|
| binding, inheriting, adopting listeners | `ListenerSpec`, `ServerBuilder::build` | names, ingress, profile, validation; translates its specs into net-io's |
| worker threads, accept, accept backoff, accept batches | yes | chooses the driver and worker count |
| the door | `open_door`, `close_door` | calls them in the boot and shutdown order |
| gate admission and egress check | `Gate` | passes the owner's gate through |
| PROXY header, TLS handshake | yes | reads the results from `ConnectionInfo` |
| idle, read, write deadlines, minimum rate, park deadline | yes, per listener | sets them per listener from the profile; sets the exchange deadline per request |
| hard connection caps (global, listener, per source) | yes, reset | sets them above serve-net's soft caps |
| soft connection caps (global, listener, profile) with a busy refusal | | yes (D-013) |
| reading and writing bytes, output budget, vectored writes | yes | pushes whole replies into `Output` |
| protocol engine, keep-alive, pipelining, request deadlines, admission, the hand-off order | | yes |
| deferred work | `Step::Park`, `Completer`, `on_wake` | `ReplyToken` on top |
| the control line | reads it per loop iteration and per accept; refuses new connections under `Shed` and `Lockdown`; drains and stops | reads the same line at request checkpoints |
| drain | closes idle connections, tells sessions, kills at the deadline | sets the close signal on in-flight replies, counts requests |
| events | transport events to the owner's sink | request events to `Observer`; health from both |

### 8.3 Data flow of one HTTP request

```
 net-io accept -> net-io hard caps -> Gate::admit (owner's gate) --refuse--> reset, nothing read
       |
 net-io handshake: PROXY header (trusted peers only) -> Gate::readmit -> TLS
       |
 SessionFactory::open -> serve-net soft caps (global, listener, profile) --over--> busy refusal, close
       |
 Session::on_open: engine chosen (profile engine, or ALPN result); head deadline armed from accept
       |
 +---- keep-alive loop --------------------------------------------------------------------------+
 | on_data(input) -> engine.feed -> engine.poll                                                  |
 |   NeedInput       -> keep the exchange deadline, Step::Read                                   |
 |   Head(head)      -> CHECKPOINT A: control line                                               |
 |      -> ingress normalization: ClientInfo (forwarded headers, FastCGI params) --fail--> 400   |
 |      -> Envelope { connection, seq, profile, tag, lane, client, arrived, deadline }           |
 |      -> Service::head -> Proceed(policy) | Refuse | Upgrade                                   |
 |      -> Admit::admit --shed--> busy refusal, close signal, done(Shed)                         |
 |      -> Checkpoint(Admitted) --deny--> refusal                                                 |
 |      -> 100-continue if expected -> body per policy (request deadline, minimum rate)          |
 |   RequestEnd      -> CHECKPOINT B: control line -> Checkpoint(BeforeDispatch)                  |
 |                   -> Dispatch::run -> Service::call                                           |
 |      -> Done      -> CHECKPOINT C: control line, Checkpoint(BeforeReply) -> engine encodes     |
 |                      into OutBuf -> pushed whole into net-io Output -> Step::Write            |
 |      -> Deferred  -> Step::Park { until: reply deadline } ... on_wake -> as Done              |
 |   on_writable     -> done(), Admit::release -> next request (pipelined input) or idle         |
 |   idle            -> CHECKPOINT D: control line; close if draining, past age, or over count    |
 +-----------------------------------------------------------------------------------------------+
       |
 on_close(reason) -> EndReason -> Observer -> engine and buffers back to the worker pools
```

Order rules that make classes of mistakes impossible:

1. No request byte beyond the head is processed before `Admit` admitted the request. A slow or half-sent head never holds an admission slot.
2. `Service::call` never runs for a request that failed ingress normalization, was refused by `head`, was not admitted, or was denied by `Checkpoint`.
3. A refusal skips every later step and goes straight to the engine's refusal bytes and the close signal.
4. `done` runs exactly once for every request that reached `head`, on every path (drain, timeout, peer close, kill, `Stopping`).
5. A reply is pushed into net-io's output whole before the session returns `Step::Write`, so the gate's egress check and a `Checkpoint` denial can never split a reply.

### 8.4 The connection state machine

The state machine lives in `conn/` and is written against events and actions, not against net-io: its inputs are "bytes arrived", "output flushed", "deadline passed", "woken with a completion", "peer closed", and "closing with reason"; its outputs are "read", "write these bytes", "park until", "set deadline", "close", "abort". The net-io session is a thin translation (`conn/session.rs`). Tests drive the machine with scripted events and a manual clock, so every deadline and ordering rule is deterministic and runs without sockets.

| State | Entered when | Deadline (net-io mechanism) | Leaves to |
|---|---|---|---|
| `AwaitHead` | first request: on open; later requests: after the previous reply | first request: `head_timeout` from accept (exchange deadline); later: keep-alive idle (net-io idle timeout, phase `Idle`) | `ReadHead` on first byte; closed on deadline (quiet) |
| `ReadHead` | first byte of a head | `head_timeout` from that byte (exchange deadline, total, never extended by input) | `HeadDone`, `Refusing` |
| `HeadDone` | engine reported a head | `request_timeout` from the first head byte | `Refusing`, `ReadBody`, `Dispatching`, `Upgrading` |
| `ReadBody` | body policy `Buffer`, `Stream`, or `Discard` | `request_timeout`; net-io's minimum inbound rate | `Dispatching`, `Refusing` |
| `Dispatching` | request complete (buffered) or head (streaming) | `request_timeout` | `Writing`, `Parked`, `Refusing` |
| `Parked` | `Outcome::Deferred` | `reply_timeout` (net-io park deadline) | `Writing`, `Refusing` (503, or 504 per engine) |
| `Writing` | reply pushed | `write_stall` (net-io write deadline) and the minimum outbound rate | `AwaitHead`, `Lingering`, closed |
| `Upgrading` | the service accepted an upgrade | `write_stall` for the 101 | `Upgraded` |
| `Upgraded` | new engine in the slot | the new engine's profile limits | closed |
| `Refusing` | any refusal | `write_stall` | `Lingering`, closed |
| `Lingering` | closing reply sent with unread input | net-io graceful linger | closed |

Drain overlays every state: `AwaitHead` closes at once (net-io closes idle connections); every other state continues with the close signal set; at the drain deadline net-io kills what remains.

At most one serve-net deadline exists per connection at a time (the exchange or park deadline), on net-io's per-worker timing wheel, so timer memory is bounded by the connection cap.

### 8.5 Ingress normalization

#### 8.5.1 `ClientInfo`

| Field | Direct | PROXY | Forwarded headers | Unix socket | FastCGI |
|---|---|---|---|---|---|
| address | socket peer | PROXY source (v2 `LOCAL`: socket peer) | walk result (8.5.2) | as the inner mode | `REMOTE_ADDR` |
| port | socket peer port | PROXY source port | `Forwarded for=` port if present, else 0 | inner mode | `REMOTE_PORT`, else 0 |
| scheme | TLS on: `https`, else `http` | `PP2_TYPE_SSL` when `trust_scheme`, else the listener's declared scheme | `X-Forwarded-Proto` / `proto=` when `trust_scheme`, else declared | inner mode | `HTTPS=on` or `REQUEST_SCHEME`, else declared |
| authority | the request's `Host` or absolute-form authority | same (`PP2_TYPE_AUTHORITY` when `trust_host`) | `X-Forwarded-Host` / `host=` when `trust_host`, else the request's | same | `HTTP_HOST`, else `SERVER_NAME:SERVER_PORT` |
| hops | 0 | 1 | trusted entries skipped plus 1 | inner mode | 1 |
| ingress | `Direct` / `DirectTls` | `ProxyProtocol` | `Forwarded` | `Unix` | `FastCgi` |
| peer | socket peer, always kept | same | same | peer credentials | same |

The declared scheme is a per-listener setting (`ListenerSpec::scheme(Scheme::Https)`) for a front end that terminates TLS and sends nothing about it; it defaults to `Http`, and `build` warns when a proxy listener leaves it at `Http` with `trust_scheme` off.

#### 8.5.2 The trusted walk

Input: the socket peer `p`, the trusted list `T`, the configured header `H`, `max_hops`.

```
 if p not in T                         -> client = p; header ignored; hops = 0
 values = every field line of H, joined in order (RFC 9110 section 5.3 list semantics)
 if values is empty                    -> MissingHeader: Refuse (400, default) | UsePeer
 entries = split on ',' (Forwarded: outside quoted strings), trim OWS, empty elements skipped
 walked = 0
 for e in entries, right to left:
     walked += 1; if walked > max_hops -> refuse (400)
     addr = parse(e)                    -> malformed: refuse (400)
         X-Forwarded-For: an IPv4 or IPv6 literal, optional brackets for IPv6, no port
         Forwarded for=: token or quoted string; "[v6]:port", "v4:port", "unknown",
                         obfuscated "_name" (RFC 7239 section 6.3) -> stop: client unknown
     if addr in T                       -> continue
     else                               -> client = addr; done
 all entries trusted                   -> client = leftmost entry
```

- A walk that stops at `unknown` or an obfuscated identifier gives `ClientInfo::addr() == None` and `is_unknown() == true`; `UnknownClient::Refuse` turns that into a refusal.
- IPv4-mapped IPv6 entries are normalized before the trust test, so `::ffff:10.0.0.1` cannot pass as untrusted or escape a ban.
- Zone identifiers (`fe80::1%eth0`) are refused: they name a local interface, never a remote client.
- Work is linear in the header length and bounded by `max_hops` and the engine's header budget. The parser never allocates: it walks the engine's borrowed field bytes from the end.
- The walk is written against a small internal trust predicate (`contains(IpAddr) -> bool`), implemented for `net_io::TrustedProxies`, so the module has no OS dependency and is fuzzed without sockets.

#### 8.5.3 FastCGI mapping

fcgi-net maps the parameters to an HTTP-shaped request (method, target from `REQUEST_URI` or `SCRIPT_NAME` + `PATH_INFO` + `QUERY_STRING`, fields from `HTTP_*`, `CONTENT_TYPE`, `CONTENT_LENGTH`). serve-net's FastCGI adapter presents that as the same `HttpRequest` the http1 adapter produces and takes the client facts from 8.5.1. `REMOTE_ADDR` is believed only because the listener accepts only trusted front ends (rule V-3). Under mod_fcgid one process gets one request at a time, so serve-net answers `FCGI_GET_VALUES` with `FCGI_MPXS_CONNS=0` and `FCGI_MAX_REQS` equal to the profile's in-flight cap; multiplexed FastCGI connections are after 1.0.

### 8.6 The engine slot

**Families.** A family is a set of engines that produce the same request view. The HTTP family (http1, fastcgi, h2) produces `HttpRequest`; that is what makes "one normalized request regardless of ingress" a type fact. A custom family (a database wire protocol) defines its own request and reply types.

**Static dispatch.** The slot is an enum with one variant per compiled-in engine plus one generic variant (`Custom(C)`, where `C = NoCustom` is uninhabited unless the owner adds a family). A `match` on a value fixed per connection is predicted by the CPU (about 1 ns [est.]); no trait objects on the hot path. An upgrade replaces the variant in place.

**Input.** net-io reads into its pooled `Input` buffer and calls the session with a borrowed view. Engines that own a buffer (http-net's `ServerConn`, ws-net, fcgi-net) are fed from that view and the session consumes what they took, so each byte is copied once (about 10-30 ns per KiB [est.]). Removing that copy needs an engine that parses from a caller's buffer; it is idea I-3 (section 13), measured at 0.6.

**Request heads.** The request view borrows from the engine's buffer. Inline dispatch (the default) needs nothing more. Work that must outlive the borrow (deferred or offloaded) takes a `DetachedHead`: an owned, `Send` copy of the head bytes and spans made from pooled vectors through http-net's `copy_into` (one copy of the head, typically under 1 KiB, about 20-60 ns [est.]). The head is copied, not swapped, because the connection buffer still holds pipelined input and the body window (D-024). Bodies and WebSocket messages that outlive the call use net-io's O(1) `Input::swap` where the engine allows it.

**Output.** Engines encode into an `OutBuf`: a pooled, per-worker byte buffer bounded by `max_pending_output`. The session pushes the whole reply into net-io's `Output` before returning `Step::Write` (rule 5 of 8.3). Static bodies go straight to `Output::push_static` with no copy; a full body above 4 KiB goes after the head with `respond_head_only`, so it is copied once, into net-io's pool. An `OutBuf` is zeroed on return when the profile wipes buffers.

**Upgrades.**

```
 Service::head returns HeadAction::Upgrade(target)
   -> serve-net validates the handshake through the target engine (ws-net) before anything is written
   -> engine writes the switch response (101)
   -> remaining_input(): bytes of the new protocol that already arrived
   -> slot replaced (ws-net engine with the target's limits), leftover bytes fed first
   -> deadlines and limits switch to the target profile
   -> HttpService::upgraded(envelope, conn) owns the conversation
```

`Upgrade: h2c` is never honoured; the request is served as HTTP/1.1 (RFC 9110 lets a server ignore `Upgrade`; RFC 9113 section 3.1 deprecates the h2c token; h2c upgrades forwarded by proxies are a known access-control bypass). A 2xx to `CONNECT` is refused unless a profile allows tunnels (after 1.0).

**ALPN.** With TLS, net-io copies the negotiated protocol into `ConnectionInfo::tls`. serve-net maps `http/1.1` to http1, `h2` to h2 (feature `h2`), anything else to the profile's custom engine if it declared that protocol id; otherwise the connection closes (`EndReason::NoEngine`). The protocols offered to the TLS layer come from the profile's engines, so a profile without h2 never offers `h2`.

**Several requests per connection.** HTTP/2 multiplexes; FastCGI can. `Envelope` and `ReplyToken` therefore never assume one request per connection: a request is identified by connection id plus `RequestSeq`.

### 8.7 The hand-off

The whole contract between serve-net and what runs above it is five calls per request, in this order:

| Call | When | May | Must not |
|---|---|---|---|
| `head(&head, &mut Envelope) -> HeadAction` | head complete, before admission and body | choose the lane and body policy, refuse, ask for an upgrade, store one word in the envelope (`set_user_word`) | block, read the body |
| `Admit::admit` (serve-net) | after `head` | admit or shed | |
| `Checkpoint::check` (serve-net) | after admission, before dispatch, before the reply | deny or kill | |
| `call(&request, &Envelope, &mut HttpReply) -> Outcome` | body complete (or head, for streaming) | answer, defer, close | keep borrowed request data past return (use `detach_head`) |
| `done(&Envelope, &Finished)` | exactly once, after the last reply byte was handed to net-io or the request ended any other way | return per-request resources, record metrics | fail (it returns `()`) |

Plus `upgraded(&Envelope, UpgradedConn)` for upgrades. A wrapper maps its own pipeline onto these: take its per-request slot in `Admit`, link it through the envelope's user word, run its pipeline in `call`, return and wipe the slot in `done`.

### 8.8 Admission and lanes

**Connection level, two tiers (D-013).** serve-net's soft caps (global, per listener, per profile) are checked in `SessionFactory::open`. Over a soft cap the session writes the busy refusal (503 with a jittered `Retry-After` and the close signal) from `on_open`, before reading anything, and closes. net-io's hard caps are set above the soft caps by a headroom (default one eighth of the soft cap, at least 16); beyond them net-io resets the connection without involving serve-net. Normal overload therefore gets a clean answer a proxy can relay, and a flood large enough to pass the headroom costs one reset per connection. Counters: one `AtomicU32` per profile and one global, one add at open and one subtract at close (two atomics per connection, never per request).

**Request level.** `Admit::admit(&self, worker, &Envelope) -> Result<Admitted, Shed>` runs after `head`; `release(worker, Admitted)` runs after `done` on every path. Both run on the worker that owns the connection (net-io calls a session only on its worker), so per-worker state written by one worker needs no read-modify-write: `InFlightCap` keeps one cache-line-padded counter per worker per profile, read and written with plain relaxed loads and stores (no lock, no contended atomic). Per-profile caps are per-worker shares (`cap / workers`, rounded up, at least 1). On the blocking driver each worker serves one connection at a time, so the per-worker cap is 1 by construction.

**Lanes.** `net_io::Lane(u8)`, lower is more important (D-010). The profile gives the default; `head` may change it. `InFlightCap::reserve(lane, percent)` reserves a share of each worker's cap for one lane; a request takes from its own lane's reserve first, then the shared pool; lanes without a reserve use only the shared pool, so they can never take reserved slots. Under `Shed { from_lane }` requests in that lane and every less important one are refused.

**Retry-After jitter.** Each busy refusal adds a jitter in `[0, base)` seconds from a per-worker xorshift state seeded at start (not secret; it only spreads retries). Base 1 s.

### 8.9 The control line

serve-net uses net-io's `ControlLine` trait and `ControlState` rather than a second control type (D-009): one line, one epoch, read by net-io per loop iteration and by serve-net at its request checkpoints. The owner passes it once (`ServerBuilder::control(Arc<C>)`); serve-net hands the same `Arc` to net-io. A checkpoint is `epoch()` (one acquire load) compared with the epoch the worker saw last; only when it moved does serve-net read `state()`.

| State | net-io does | serve-net does |
|---|---|---|
| `Normal` | accepts | admits per `Admit` |
| `Shed { from_lane }` | refuses new connections in those lanes; `pressure()` reports 1.0 | refuses requests in those lanes (busy refusal); every response carries the close signal |
| `Lockdown` | refuses new connections on listeners not marked `lockdown_allowed` (serve-net marks critical profiles' listeners) | refuses requests on profiles not marked critical |
| `Draining` | closes the door and drains with the line's deadline | close signal on every response; `DrainReport` counts |
| `Stopping` | kills every connection | `done` for every open request with `RequestEnd::Killed` |

serve-net does not hand the owner's line to net-io as it is: it hands a merged line, the owner's line plus serve-net's own drain state (D-032). The merged epoch is the sum of both epochs (each only grows), the state is the stricter of the two, the deadline the earlier. `DrainHandle::drain(deadline)` sets serve-net's half to `Draining` with that deadline and wakes every worker, so net-io drains with exactly that deadline and `Running::wait` returns its report, whatever line the owner uses (including `NoControl`, whose epoch is a constant 0, so the merged read is still one load). Two acquire loads cost about 1 ns on x86-64 [est.], inside NFR-11.

### 8.10 Deadlines and limits, mapped

| serve-net value | Default | Implemented by |
|---|---|---|
| `head_timeout` | 10 s, 100 ms to 300 s | exchange deadline from accept (first request) or first byte (later requests) |
| `request_timeout` | 30 s, at least `head_timeout`, at most 1 h | exchange deadline from the first head byte to the end of the body |
| `reply_timeout` | 30 s | `Step::Park { until }` |
| keep-alive idle | 20 s direct, 60 s behind a front end | net-io `idle_timeout` per listener |
| `write_stall` | 30 s | net-io `write_timeout` per listener |
| `min_rate` | 256 B/s over 10 s | net-io `MinRate` per listener (inbound while receiving, outbound while sending) |
| `max_pending_output` | 256 KiB | net-io `output_budget` per listener, plus serve-net's own bound on queued pipelined replies |
| `max_deferred_per_worker` | 4,096 | net-io `completion_queue` |
| `linger` | 2 s, 64 KiB | net-io `graceful_linger` |
| soft `max_connections` | blocking: worker count; reactor: 10,000 | serve-net (8.8); net-io's cap set above it |
| `max_body` | 1 MiB | the http1 engine's `max_body` (the smaller of the two wins) |
| engine limits | http-net defaults | `Limits::engine_limits` |
| any other transport limit | net-io defaults per driver | `ServerBuilder::transport_limits`, `ListenerSpec::transport_limits` |

On a request deadline the session writes the engine's timeout refusal (408) if no byte of a response has left, then closes; net-io's own deadlines close abortively with their own reasons. The head deadline is armed from accept for the first request, so a client that connects and sends its first byte late is closed earlier (idea I-1, in effect from 0.3).

**Keep-alive under pressure (blocking driver).** Above `under_pressure_at` of busy threads (default 0.75) the idle timeout for a connection returning to idle is shortened to `under_pressure` (default 2 s); above 0.9 every response carries the close signal. The shortened idle uses net-io's `SessionContext::set_idle_timeout`, set at each response (`None` restores the listener's value below 0.75).

### 8.11 Workers

`Workers::blocking(threads)` and `Workers::reactor(workers)` choose net-io's driver and worker count; `Workers::auto()` picks net-io's `Auto` (the reactor where net-io provides it, from net-io 0.5; the blocking pool before). Blocking: a connection holds a thread for its life, which is why keep-alive shortens under pressure. Reactor: one worker per core, each owning its connections; deferred completions through net-io's per-worker completer.

### 8.12 Memory

| Object | Pool | Bound | Held by an idle connection? |
|---|---|---|---|
| net-io read buffer | net-io, per worker | net-io read classes (16 KiB, growing to 64 KiB) | reactor: no; blocking: yes (the thread's) |
| engine state (`ServerConn`) | per worker free list | the engine's limits (64 KiB buffer with http-net defaults) | no: `shrink_idle` releases the buffer while idle |
| `OutBuf` | per worker | grows to `max_pending_output`, shrinks to 16 KiB on return | no |
| body buffer (`Buffer` policy) | per worker | the request's `max_body` | no |
| detached heads | per worker | the engine's head budget | only while deferred |
| connection record | inside the session | fixed (about 300 bytes [est.]) | yes |
| deferred-reply slot | one per connection that ever deferred, reused | fixed | yes, after the first deferral |

`wipe_buffers` (per profile, on by default) zeroes the used length of every serve-net buffer returned to a pool and turns on http-net's request wipe, matching net-io's own default (D-021). Cost about 50-100 ns per 2 KiB [est.].

### 8.13 Drain, the door, and restarts

**close_door.** net-io's `close_door` stops accepting and keeps the listening sockets open until shutdown, so connections queued in the backlog stay for a process that holds the same socket. There is no separate "keep for hand-off" mode (D-016).

**Hand-off restart.**

```
 supervisor holds :443 (one socket)
 1. supervisor starts NEW with the socket (descriptor passing, net-io)
 2. NEW: build -> bind (ListenerSpec::handed_off) -> open -> Notify::ready -> supervisor
 3. supervisor tells OLD to retire (its own channel; serve-net is not involved)
 4. OLD: close_door -> drain(deadline) -> Notify::stopping
 5. OLD: in-flight requests finish; idle keep-alive connections close (clients reconnect to NEW)
 6. OLD exits; the supervisor keeps the socket
```

Between 2 and 4 both processes accept from the same backlog; each connection belongs to exactly one of them. serve-net never uses separate `SO_REUSEPORT` sockets for restarts: on Linux each has its own accept queue, and connections queued on a closed one are reset [verified by the restart test before 0.4 ships].

**systemd without a supervisor.** Socket activation gives the same shared-socket property across restarts; `FDSTORE=1` keeps sockets in the manager during a restart (after 1.0; needs descriptor passing over the notify socket, which is net-io's).

**drain(deadline).** serve-net sets its drain flag and calls net-io's `drain`: the door closes, idle connections close at once, sessions see `draining()` and set the close signal, net-io kills what remains at the deadline. The `DrainReport` combines net-io's connection counts with serve-net's request counts.

### 8.14 Hot reload

```
 reload(next):
   1. validate(next) with every rule of 9.3           fail: Err(ConfigErrors), nothing changed
   2. diff listeners by name: added, removed, changed (address or source changed = removed + added;
      ingress, trust, profile, limits changed = in place through snapshots)
   3. bind every added listener (door closed)          fail: close only the new ones, Err
   4. Notify::reloading(monotonic_usec)
   5. publish snapshots: profile table, per-listener runtime (ingress, trust, profile id), limits
   6. open the door on added listeners
   7. removed listeners: close their door; their connections drain with the drain deadline
   8. Notify::ready(); Observer::reloaded(summary)
```

Snapshots are cached per worker (`SnapshotReader`) and refreshed at the next checkpoint. Steps 3, 6, and 7 need net-io to add and remove listeners on a running server, which its 0.2 API does not offer (requested, section 12); changes to profiles, limits, and trust lists need nothing from net-io.

### 8.15 Profiles

A profile is the answer to "connections on this listener use this engine, this lane, this budget, these limits". serve-net's `Profile` holds only what serve-net uses, plus an opaque `tag: u8` that a wrapper reads back from the envelope.

| Field | Use |
|---|---|
| `id: ProfileId(u8)` | dense, `0..count`, at most 255 profiles (`ProfileId::MAX` reserved) |
| `tag: u8` | opaque; copied into every envelope |
| `engine: EngineChoice` | default engine and ALPN offer list |
| `lane: Lane` | default lane |
| `budget: Budget` | soft `max_connections`, `max_in_flight`, dedicated workers (0.5) |
| `limits`, `keep_alive` | overrides of the server values |
| `critical: bool` | admitted under `Lockdown`; its listeners marked `lockdown_allowed` |
| `wipe_buffers: bool` | zero buffers on return (default on) |
| `body: BodyPolicy` | default body policy before `head` changes it |

A single-service install has one profile (created with defaults if none is added), or two (a public and an admin listener).

### 8.16 Readiness

`SystemdNotify` reads `NOTIFY_SOCKET` once at construction and sends one datagram of newline-separated assignments per call (`READY=1`, `RELOADING=1` with `MONOTONIC_USEC=`, `STOPPING=1`, `STATUS=`, `WATCHDOG=1`, `EXTEND_TIMEOUT_USEC=`). Path sockets use `UnixDatagram::send_to`; `@name` uses `SocketAddrExt::from_abstract_name` and `send_to_addr` (stable since Rust 1.70). The manager reads sender credentials itself; serve-net sends no ancillary data. Failures go to the observer and never stop the server. On Windows and macOS `from_env()` returns `None`; the Windows service status is the same moments through the `Notify` trait, implemented outside serve-net because it needs FFI (OD-6).

`MONOTONIC_USEC` must be the raw `CLOCK_MONOTONIC` value, which `std::time::Instant` does not expose, so `Notify::reloading` takes it as a parameter supplied by the owner (OD-7); a source in net-io or sys-lib is requested.

### 8.17 Observability

`Observer` has one method per request-level event, each with an empty default body, and serve-net is generic over it, so `NoObserver` inlines to nothing. Transport events (listener opened, accepted, refused, accept errors, descriptor pressure, kills, egress denials, closes) are net-io's and go to the owner's net-io event sink, which serve-net passes through and taps for health (D-012). `Running::stats()` is always available (D-033): serve-net keeps a few request counters per worker, cache-line padded and written only by their worker with relaxed stores, summed on request, so the hot path pays one local increment per counter (about 1 ns [est.]).

`health()` is computed on request from: listeners accepting (net-io accept-error and listener-failed events within `accept_failure_window`), workers alive (each worker's progress counter moved since the last call, or the worker is idle), overload (refusal rate over the last interval above 0.2), and drain state.

### 8.18 Errors

- `ServeError` (start-up and run-time): `Config(ConfigErrors)`, `Bind { listener, source: net_io::NetError }`, `Start(net_io::NetError)`, `Inherited { listener, reason }`, `HandOff { listener, reason }`, `Draining`, `Reload(ConfigErrors)`, `ReloadBind { listener, source }`. `#[non_exhaustive]`, `std::error::Error` with sources, `Display` names the listener and what to do.
- `ConfigErrors`: every `ConfigError { rule: RuleId, subject: Subject, message, hint }` found, sorted by subject; never empty.
- `EndReason` for connection ends: serve-net's own reasons, and `Transport(net_io::CloseReason)` for every end net-io decided.
- No error or event carries request bytes or header values (they may hold credentials): they name the rule and the listener or profile only.

---

## 9. Security

### 9.1 Trust boundaries

| Boundary | Untrusted side | serve-net's job |
|---|---|---|
| the network peer | every byte, every timing, every connection count | request deadlines, request limits, soft caps, admission, refusal (transport deadlines and hard caps are net-io's) |
| the front end (proxy, FastCGI process manager) | trusted only when its address or credentials are on the list | believe its client facts only then; refuse to listen where untrusted parties can reach a trusting listener |
| the service | trusted code that may be slow, panic, or forget work | deadlines on deferred replies, bounded queues, `done` on every path, panic policy |
| the owner's configuration | may be wrong | refuse insecure and inconsistent configurations at build and reload |
| other processes on the host | may connect to Unix sockets or the notify socket | socket modes, peer credentials; notify is send-only |

### 9.2 Threats

Each test is an attack test that must fail to get through. Costs are per request unless stated; [est.] until measured.

| Id | Threat | Defence | Test | Cost |
|---|---|---|---|---|
| T-1 | Slowloris: a head sent one byte at a time | total head deadline from the first byte (from accept for the first request), never extended by input; net-io's minimum rate | `test_head_trickle_one_byte_per_second_closed_at_deadline` | one exchange deadline per request (wheel insert about 20 ns [est.]) |
| T-2 | Slow body: a large declared body sent slowly | request deadline plus the minimum rate | `test_body_below_min_rate_closed` | one meter update per read (net-io) |
| T-3 | Slow read: replies pile up while the client does not read | net-io write deadline and outbound minimum rate; `max_pending_output` stops input processing | `test_client_never_reads_connection_killed_after_stall` | one meter update per write (net-io) |
| T-4 | Idle keep-alive hoarding | idle timeout; adaptive keep-alive on the blocking driver; idle connections hold no buffers on the reactor; caps | `test_idle_flood_blocking_pool_shortens_keep_alive` | none per request |
| T-5 | Connection flood | the owner's gate, net-io's hard caps, serve-net's soft caps with a busy refusal written without reading | `test_connection_cap_plus_one_gets_503_without_read` | two atomics per connection |
| T-6 | Pipelining flood | `max_pipelined_per_wake`, `max_pending_output` back-pressure, requests per connection | `test_10k_pipelined_requests_bounded_memory_and_fair` | none |
| T-7 | Request smuggling through framing ambiguity | http-net refuses every ambiguous message; after any parse error one refusal and close; serve-net never re-frames or forwards | http-net's smuggling corpus run end to end through serve-net | none beyond http-net |
| T-8 | Desync after an early response with an unread body | the connection closes after a response sent before the body was read | `test_early_reply_unread_body_closes` | none |
| T-9 | Spoofed client address through forwarded headers | headers used only from trusted peers; right-to-left walk; first untrusted entry wins | `test_xff_from_untrusted_peer_ignored`, `test_xff_left_injection_not_used` | walk under 100 ns for 3 entries [est.] |
| T-10 | Spoofed client address through PROXY | net-io accepts PROXY only from trusted peers and requires it from them; serve-net checks the listener's own list | `test_proxy_listener_untrusted_peer_closed_before_read` | once per connection |
| T-11 | Trust list too broad | refused at build (V-4, V-5) | validation tests | none at run time |
| T-12 | Proxy or FastCGI port reachable from the internet | refused at build unless loopback, private, or local; override needs a reason and is reported (V-1) | `test_build_proxy_listener_public_bind_refused` | none |
| T-13 | Forwarded header trusted on a direct listener | refused at build (V-2) | validation test | none |
| T-14 | Header-list parsing DoS | the engine's header budget bounds bytes; `max_hops` bounds the walk; linear scan from the end | fuzz target `forwarded`; `test_xff_1000_entries_refused_at_hop_limit` | bounded |
| T-15 | IPv4-mapped IPv6 to dodge a trust rule or a ban | normalization before every trust test and in `ClientInfo` | `test_mapped_v6_trusted_as_v4` | a few ns |
| T-16 | Zone identifiers or malformed literals in headers | refused (400) | fuzz and unit tests | none |
| T-17 | Scheme or host spoofing | taken only from trusted peers and only when enabled; values validated | `test_xfp_from_untrusted_ignored`, `test_xfh_invalid_authority_refused` | a few ns |
| T-18 | Unix-socket hijack by another local user | mode `0o660` default, world-writable refused (V-9), peer credential allowlist | `test_unix_peer_wrong_uid_closed` | once per connection |
| T-19 | FastCGI `REMOTE_ADDR` forgery | FastCGI listeners accept only trusted peers; `FCGI_WEB_SERVER_ADDRS` honoured | `test_fastcgi_untrusted_peer_closed` | once per connection |
| T-20 | h2c upgrade smuggling through a proxy | `Upgrade: h2c` never honoured; only upgrades the service accepted in `head` | `test_upgrade_h2c_served_as_http11` | none |
| T-21 | Upgrade to an unexpected protocol or an invalid WebSocket handshake | the service must accept the target; ws-net validates the handshake before 101 | `test_ws_bad_key_refused_400` | none |
| T-22 | `Expect: 100-continue` used to commit the server to a huge body | 100 sent only after the head decision and admission; declared length checked first | `test_expect_continue_shed_gets_503_no_100` | none |
| T-23 | Admission bypass: a slow head holds a slot | the slot is taken only after the head is complete | `test_partial_head_holds_no_admission_slot` | none |
| T-24 | Overload collapse | `InFlightCap`, busy refusal in microseconds, jittered `Retry-After`, keep-alive closed under shed | overload test (NFR-6) | about 1-2 µs per refusal [est.] |
| T-25 | Low-priority flood starves health checks and administrators | reserved lanes; critical profiles under `Lockdown`; dedicated workers per profile | `test_bulk_flood_critical_lane_still_admitted` | an array index |
| T-26 | Deferred-reply leak | `reply_timeout`; `max_deferred_per_worker`; a dropped token closes the connection with 500 | `test_token_dropped_connection_closed_500`, `test_deferred_cap_plus_one_refused` | none |
| T-27 | Offload queue growth | bounded `OffloadPool::try_submit`; full means busy refusal | `test_offload_full_refused_503` | none |
| T-28 | Revoked request still answered | `Checkpoint` after admission, before dispatch, and before the reply; the owner's gate before every write | `test_checkpoint_deny_before_reply_no_bytes_of_reply` | `NoCheckpoint`: zero |
| T-29 | Drain abuse: a request kept open to block shutdown | drain deadline then abortive close | `test_drain_deadline_kills_slow_request` | none |
| T-30 | Restart window refuses or resets connections | shared-socket hand-off; no separate reuse-port sockets across processes | T-RST: steady load through a restart, zero errors; kill at every step | none per request |
| T-31 | Reload to an insecure configuration | the same validation on reload; all-or-nothing | `test_reload_public_bind_refused_old_config_kept` | none |
| T-32 | Descriptor exhaustion makes the accept loop spin | net-io's accept backoff; health `Degraded` from its events | `test_emfile_accept_backs_off_no_spin` (Linux) | none |
| T-33 | Memory growth from many connections or large replies | per-connection bounds, pooled buffers shrink on return, caps; nothing sized by input | soak (NFR-18), counting allocator | none |
| T-34 | Information leak in refusals | fixed generic refusal bodies; no `Server` field; errors carry no request bytes | snapshot tests of every refusal's bytes | none |
| T-35 | Log or metrics injection through client-supplied address strings | addresses parsed to `IpAddr`; observers never receive raw header bytes | review plus a test with control bytes in a forwarded header | none |
| T-36 | Secrets left in pooled buffers | `wipe_buffers` on by default | `test_wipe_buffers_pool_holds_only_zeroes` | 50-100 ns per 2 KiB [est.] |
| T-37 | A panicking service takes down a worker silently | `catch_service_panics` (unwind builds) or abort and restart; net-io closes a panicking session; health reports stalled workers | `test_panic_caught_500_and_done_called` | `catch_unwind` about 1-2 ns without a panic [est.] |
| T-38 | Notify-socket misuse | `NOTIFY_SOCKET` read once from the environment; only fixed assignments sent, never request data | unit test of the datagram bytes | none |
| T-39 | Hand-off socket substitution | net-io checks the sender's credentials and verifies every received socket; serve-net checks kind and name | `test_handed_off_wrong_kind_refused` | start-up only |
| T-40 | Timer flood | one serve-net deadline per connection on net-io's wheel, bounded by the cap | wheel property test (net-io), connection test | O(1) |

### 9.3 Validation rules (build and reload)

| Rule | Refuses | Override |
|---|---|---|
| V-1 `PublicBind` | a TCP listener with `Proxy` or `FastCgi` ingress bound to an unspecified (`0.0.0.0`, `::`) or public address. Allowed without override: loopback (`127.0.0.0/8`, `::1`), private ranges (`10/8`, `172.16/12`, `192.168/16`, `fc00::/7`), link-local, Unix paths, descriptor 0, inherited and handed-off sockets | `allow_public_bind(reason)` |
| V-2 `ForwardedOnDirect` | forwarded-header trust on a `Direct` listener | none |
| V-3 `FastCgiTrust` | a FastCGI TCP listener with an empty trust list | none |
| V-4 `TrustWildcard` | any trust entry of prefix length 0 | none |
| V-5 `TrustTooBroad` | IPv4 entries shorter than /8, IPv6 entries shorter than /32 | `allow_broad_trust(reason)` |
| V-6 `TwoClientSources` | PROXY and forwarded headers both as the client-address source | none |
| V-7 `TrustEmpty` | a proxy listener with no trusted entries | none |
| V-8 `TrustParse` | a trust entry that is not an address or CIDR, or has host bits set (Tier 1 text only; typed lists are checked by net-io) | none |
| V-9 `WorldWritableSocket` | a Unix socket mode with write permission for others | `allow_world_writable(reason)` |
| V-10 `PeerCredentialsUnavailable` | a uid or gid allowlist on a platform without peer credentials | use `AnyLocalPeer` explicitly |
| V-11 `DuplicateName` | two listeners with one name | none |
| V-12 `BadName` | a name outside 1 to 64 bytes of `[a-z0-9._-]` | none |
| V-13 `ProfileIdReserved`, `ProfileGap` | `ProfileId(255)`; ids not dense from 0; two profiles with one id | none |
| V-14 `UnknownProfile` | a listener naming a profile that does not exist | none |
| V-15 `DeadlineOrder` | `request_timeout` below `head_timeout`; keep-alive idle 0 with keep-alive on | none |
| V-16 `DeadlineRange` | any value outside its documented range | none |
| V-17 `ZeroCap` | any connection or in-flight cap of 0 | none |
| V-18 `DuplicateBind` | two listeners on the same address or path | none |
| V-19 `DriverUnavailable` | a driver this build or platform does not provide | none |
| V-20 `ReloadWorkers` | a reload that changes `Workers` | restart instead |
| V-21 `DrainRange` | drain deadline 0 or above 1 h | none |
| V-22 `AlpnWithoutTls` | ALPN engine choice on a listener without TLS | none |
| V-23 `TlsOnFastCgi` | TLS on a FastCGI listener (front ends do not speak TLS to the application) | none |
| V-24 `SchemeTrustWithoutSource` | `trust_scheme` or `trust_host` with no source that can carry them | none |
| V-25 `AddressResolve` | a listener address that resolved to nothing | none |
| V-26 `TlsWithoutAcceptor` | a TLS listener when no TLS acceptor was given | none |

Warnings, which do not refuse: a proxy listener with `Scheme::Http` declared and `trust_scheme` off; `min_rate` 0; `catch_service_panics` in an abort build; `Offload` on the blocking driver; keep-alive idle under 5 s on a proxy listener (front-end reuse races); `Lockdown` configured with no critical profile; a removed last listener on reload.

### 9.4 What serve-net does not defend

- Floods larger than the link: upstream filtering is needed.
- Bytes already written cannot be recalled; buffered replies (the default) make the egress check see the whole reply first.
- A trusted front end that lies: serve-net believes trusted peers by definition.
- Application-level abuse (credential stuffing, scraping): the gate and the application.
- Kernel-level SYN floods: the OS and net-io (backlog, SYN cookies).

---

## 10. Performance

### 10.1 Targets

| Id | Measure | Target | Driver |
|---|---|---|---|
| P-1 | serve-net's own work per keep-alive request (null engine, null service, no system calls) | at most 150 ns p50 | reactor |
| P-2 | plaintext, keep-alive, one request per round trip, 256 connections | at least 1.2x hyper 1.x with hyper-util on tokio (multi-thread), same box, same strictness | reactor |
| P-3 | plaintext, pipelined 16 | at least 1.2x hyper | reactor |
| P-4 | connection churn (a new connection per request) | at least 1.2x hyper | reactor |
| P-5 | idle memory per keep-alive connection (100k idle) | at most 4 KiB excluding kernel buffers | reactor |
| P-6 | refusal cost under overload | at most 2 µs CPU per busy refusal [est.] | both |
| P-7 | admitted p99 under 10x overload | within 2x of unloaded p99 | reactor |
| P-8 | control checkpoint | at most 2 ns | both |
| P-9 | trusted walk, 3-entry `X-Forwarded-For` | at most 100 ns [est.] | both |
| P-10 | restart hand-off | 0 refused, 0 reset under steady load | both |
| P-11 | reload visible | next checkpoint; 0 dropped | both |
| P-12 | blocking driver plaintext keep-alive | at least equal to http-net `blocking::Server` with every serve-net defence on | blocking |
| P-13 | idle CPU | 0 wake-ups per second with no connections | both |
| P-14 | allocations per request after warm-up | 0 | both |

### 10.2 Competitors

hyper 1.x with hyper-util (the 20% reference; configure equal timeouts, since hyper has no idle timeout by default), actix-web 4.x (minimal handler), may-minihttp (reported separately as an upper bound: it does not apply serve-net's validation), nginx (`return 200`, the C reference for keep-alive and idle memory), h2o, and Pingora (restart behaviour and idle memory only). Every run: 30 s warm-up, 60 s measure, five runs, median and spread, p50 to p99.9 from a coordinated-omission-corrected load generator, server and load generator on separate machines or separate NUMA nodes. Laptop numbers only compare serve-net builds with each other.

### 10.3 Cost of each defence

| Defence | Cost |
|---|---|
| request deadlines | one exchange-deadline update per request on net-io's wheel (about 20 ns [est.]) |
| minimum rates, write deadline | net-io's (under 50 ns per check [est.]) |
| soft connection caps | two atomics per connection (5-20 ns [est.]) |
| `InFlightCap` | under 5 ns [est.] |
| control checkpoints (4 per request) | about 4 ns total when unchanged [est.] |
| `Checkpoint` with `NoCheckpoint` | 0 |
| trusted walk | under 100 ns for 3 entries [est.]; 0 on direct and PROXY listeners |
| `Date` field | under 5 ns (cached per second) [est.] |
| `wipe_buffers` | 50-100 ns per 2 KiB [est.] |
| buffered replies | one copy of the body into net-io's pool unless static |
| feeding the engine from net-io's buffer | one copy of each input byte, about 10-30 ns per KiB [est.] (idea I-3) |

A hot-path change runs the criterion benchmarks against the stored baseline; a regression over 5% blocks it.

---

## 11. Testing

| Layer | What | Where |
|---|---|---|
| unit | every function, every `Ok` and `Err` path, every validation rule, every `EndReason` | bottom of each file |
| simulated connection | the state machine driven with scripted events and a manual clock: every deadline, rate, ordering rule, and invariant, without sockets | `src/conn/` tests, `tests/sim_*.rs` |
| loopback integration | real sockets per ingress mode on Linux, macOS, Windows | `tests/ingress_*.rs` |
| interop | behind Apache `mod_proxy_http` (TCP and `unix:`), `mod_proxy_fcgi`, `mod_fcgid`, nginx `proxy_pass` and `fastcgi_pass`, nginx `stream` with `proxy_protocol on`, HAProxy `send-proxy` and `send-proxy-v2` | Linux CI containers, from 0.4 |
| property | the invariants below | proptest |
| fuzz | `forwarded_xff`, `forwarded_rfc7239`, `forwarded_proto_host`, `fastcgi_mapping`, `conn_sim`, `config` | cargo-fuzz, corpus outside the repository |
| concurrency | loom models: `Snapshot` publish against readers, the deferred-reply slot against the deadline, soft-cap counters at the boundary, drain against the last connection closing, two concurrent drains | `tests/loom_*.rs` |
| allocation | counting allocator for NFR-2 and P-14 | `tests/alloc.rs` |
| restart and drain | the T-RST harness: two processes, one socket, steady load, kill at every step | `tests/restart.rs` |
| soak and leak | 24 h mixed load; 100k open and close | scheduled job before 0.9 |

**Ingress equivalence (FR-17).** One table of request cases (method, target, fields, body, client address and port, scheme, authority) is rendered into the wire form of each ingress (direct; PROXY v1 and v2; `X-Forwarded-For` behind one and two trusted hops; `Forwarded`; Unix socket with PROXY and with headers; FastCGI parameters as Apache and nginx produce them) and fed through serve-net with a recording service. The recorded request (method, path, query, fields excluding hop-by-hop and forwarding fields, body) and `ClientInfo` must be identical across ingresses for every case.

**Invariants.**

1. `done` is called exactly once for every request that reached `head`.
2. No service call without an admitted head and a `Pass` from `Checkpoint`.
3. No body byte is processed before admission.
4. Replies are written in request order on a connection.
5. Pending output never exceeds `max_pending_output` plus one reply.
6. Every connection ends with exactly one `EndReason`.
7. After a parse error, no later request on that connection reaches the service.
8. Admission counters return to zero when every connection is closed.
9. Deadlines are never extended by input.
10. With the default hooks, the event order is identical across the blocking and simulated reactor runs for the same script.

The complete edge-case list (listeners, ingress, deadlines, keep-alive, admission, hand-off, upgrades, drain, platforms, resources) is kept with the tests that implement it; every case named in the threats above has a test.

---

## 12. What serve-net needs from other crates

| From | Needs | Needed by | Status (2026-10-23) |
|---|---|---|---|
| net-io 0.3 | TCP listeners, the blocking driver, sessions, `Completer`, the control line, events, `Limits`, `Cidr` | serve-net 0.3 | net-io 0.2 design lock published; 0.3 in progress |
| net-io 0.3 | `TrustedProxies` in 0.3 rather than 0.4 (forwarded headers are a 0.3 ingress) | serve-net 0.3 | agreed: ships in net-io 0.3.0 (net-io D-081) |
| net-io | a per-connection idle timeout override in `SessionContext` (keep-alive shortened under pressure, FR-58) | serve-net 0.3 | agreed: `set_idle_timeout(Option<Duration>)` ships in net-io 0.3.0 |
| net-io | adding and removing listeners on a running server (reload, FR-55) | serve-net 0.4 | agreed for net-io 0.4.0 |
| net-io | per-listener trusted proxies for PROXY (today one list per server; serve-net checks the listener's own list itself) | serve-net 0.4 | agreed for net-io 0.4.0 |
| net-io or sys-lib | the raw monotonic clock value for `RELOADING=1` (OD-7) | serve-net 0.4 | agreed for net-io 0.4.0 |
| net-io 0.4 | PROXY, Unix sockets with peer credentials, the TLS seam with ALPN, inherited listeners, descriptor 0, hand-off | serve-net 0.4 | planned in net-io |
| net-io 0.5 | the reactor, Windows AF_UNIX | serve-net 0.5 | planned in net-io |
| http-net | reply reuse without allocation (OD-3) | NFR-2 | done: `Response::clear` (0.5), `write_head` with borrowed fields and the `Output` trait (0.5), `respond_head_only` (0.6) |
| http-net | a mode that parses from a caller's buffer (idea I-3) | 0.6 measurement | to request after measuring |
| fcgi-net 0.3 | the responder engine with the same feed and poll shape, the `cgi` mapping, configurable `GET_VALUES` answers | serve-net 0.4 | planned in fcgi-net |
| ws-net | `Connection::server()` with limits and the feed, poll, output shape | serve-net 0.4 | exists in 0.2 |
| tls-net | an acceptor on net-io's TLS seam | serve-net 0.4 (direct TLS) | planned |
| sys-lib | Windows service control (FFI) behind a `Notify` adapter | 0.9 | planned |

---

## 13. Ideas and experiments

At least one new defence idea per version, written as hypothesis, method, and expected cost.

| Id | Version | Hypothesis | Method | Expected cost |
|---|---|---|---|---|
| I-1 | 0.3 | Most slowloris tools send their first head byte late; a head deadline from accept (not from the first byte) for the first request closes them earlier | designed in; measure against a slowloris tool | none |
| I-2 | 0.3 | A two-tier cap (soft cap answered with 503, hard cap reset by net-io) gives proxies a clean answer under normal overload and keeps floods cheap | overload test with a proxy in front: count 502s versus 503s and server CPU per refused connection | two atomics per connection |
| I-3 | 0.6 | Feeding the engine from net-io's buffer costs one copy per byte; an engine that parses from the caller's buffer removes it and halves read memory per connection | benchmark B-5 with and without the copy (prototype on http-net's stateless parsers) | none if it works; an http-net API addition |
| I-4 | 0.4 | A client that sends a full head in one packet behaves differently from a tool; a "head arrived in N reads" count in `Finished` gives a gate a cheap strike signal | count reads per head; correlate with tool traffic | one integer per request |
| I-5 | 0.5 | Under shed, closing keep-alive is gentler than refusing heads; `Connection: close` alone may restore p99 before any 503 | overload experiments with both orders | none |
| I-6 | 0.6 | A per-worker "consecutive refusals" damper (shorter idle, smaller pipelining budget while refusing) keeps CPU for admitted work | A/B on the overload benchmark | a few ns per refusal |
| I-7 | 0.7 | An accept-time tarpit for peers over a strike threshold costs the attacker more than the server | delayed refusal through net-io's timing wheel, bounded count | one timer per tarpitted connection |
| I-8 | 0.8 | Front-end misconfiguration can be detected at run time (a trusted proxy that never sends the forwarded header, or sends private client addresses for every request) | observer event when ratios cross thresholds | sampled, off the hot path |
| I-9 | 0.9 | Kill-at-every-step restart testing finds hand-off windows the design missed | fault injection in the T-RST harness | test only |

---

## 14. Open decisions

All technical decisions are taken (section 16). None is open for James.

| Id | Question | Answer |
|---|---|---|
| O-4 | Approve `docs/API.md` as the 0.2 design lock | approved by James on 2026-10-29: "I approve ... serve-net's design lock PR #1 including publishing serve-net 0.2.0" |
| O-5 | Publish 0.2.0 to crates.io to claim the name (it was never published) | approved by James on 2026-10-29 in the same words; published with the 0.2.0 release |

The earlier O-1 (name), O-2 (core contract with the system kernel), and O-3 (publish 0.1.0) are closed: O-1 by use, O-2 by D-006 (serve-net has no core of its own; its owner calls the boot and shutdown steps of 3.4), O-3 by O-5.

---

## 15. Superseded plan text

The 0.1 plan described serve-net as a bundle of net-io, bouncer-io, and a protocol engine with a fixed security pipeline (inspection, authentication, identity checkpoint, authorization, body inspection, application, export inspection, egress), a CGI start mode, and a service core that registers with the system kernel. Those moved: the pipeline belongs to the application that wraps serve-net and is built on serve-net's hooks; the connection gate is installed on net-io by the owner; CGI is dropped; the owner calls serve-net's boot and shutdown steps directly. D-002, D-003, and D-005 are superseded by D-006.

---

## 16. Decision log

| Id | Date | Decision | Reason |
|---|---|---|---|
| D-001 | 2026-10-13 | serve-net is a generic engine with a protocol slot, not an HTTP-only server | one infrastructure for web, databases, and other services |
| D-002 | 2026-10-13 | Fixed pipeline slots; adapters fill them. **Superseded by D-006** | |
| D-003 | 2026-10-13 | bouncer-io is consulted, not flowed through. **Superseded by D-006** (a gate is installed on net-io; serve-net offers `Checkpoint`) | |
| D-004 | 2026-10-13 | All ingress modes normalize to one request type | nothing above serve-net depends on deployment |
| D-005 | 2026-10-13 | serve-net has a core that joins the system core. **Superseded by D-006** | |
| D-006 | 2026-10-23 | serve-net is the public, generic engine wrapped by its owners: no fixed security pipeline (it moves to the wrapping application, on serve-net's hooks), no dependency on bouncer-io, no CGI mode (persistent processes only; FastCGI covers shared hosting), no service core (the owner calls build, bind, open, close_door, drain) | the security order is the application's design; a per-request process loses every in-memory defence and costs 1-10 ms per request [est.]; one-way dependencies |
| D-007 | 2026-10-23 | serve-net runs on net-io's `Server`: its per-connection engine is a net-io `Session` built by a `SessionFactory`; net-io owns accept, workers, the door, transport deadlines, hard caps, kill, and drain mechanics; serve-net owns the protocol engine, request deadlines (through the exchange and park deadlines), admission, and the hand-off. No async runtime (approved exception to the REPS tokio clause) | net-io's 0.2 lock already provides the loops and their defences; running a second set of loops would duplicate them and split the drain and kill paths |
| D-008 | 2026-10-23 | The service is given at bind: `build() -> Built`, `Built::bind(service) -> Bound` (listeners bound, workers started, door closed), `Bound::open() -> Running`. Tier 2 shortcut `ServerBuilder::serve(service)` does all three | net-io needs the session factory (which holds the service) when it builds the server; the privilege-drop window between bind and open is kept |
| D-009 | 2026-10-23 | serve-net uses net-io's `ControlLine` trait and `ControlState` instead of its own `Control` trait and `Directive`; the owner passes one `Arc` and serve-net gives the same line to net-io. `Restrict` is net-io's `Lockdown`; `Drain` is `Draining`; `Stop` is `Stopping` | one control line and one epoch for the process; no mirroring between two types |
| D-010 | 2026-10-23 | Lanes are `net_io::Lane(u8)` (lower is more important), re-exported; no separate `LaneId` | the same lane value is compared by net-io at accept and by serve-net at admission |
| D-011 | 2026-10-23 | The owner's net-io `Gate` is passed through `ServerBuilder::gate`; the default is net-io's `AllowAll`, named in the documentation of every entry point that uses it. A denial is answered by the session's `on_denied` with the engine's refusal for the code | the gate is a transport concern; serve-net only formats the protocol's refusal |
| D-012 | 2026-10-23 | Transport events are net-io's (`EventSink`, passed through `ServerBuilder::events`, tapped by serve-net for health). serve-net's `Observer` carries request-level events only: listener, accept-error, and connection-refused events of the earlier sketch are dropped from it | one source per event; no duplicate counts |
| D-013 | 2026-10-23 | Two-tier connection caps: serve-net's soft caps (global, listener, profile) answer with the busy refusal before reading; net-io's hard caps sit above them by a headroom (default one eighth, at least 16) and reset | a 503 with `Retry-After` is relayed by proxies as 503 (a reset becomes 502); floods past the headroom stay cheap (idea I-2) |
| D-014 | 2026-10-23 | serve-net's `Limits` holds protocol budgets and the transport values it maps onto net-io per listener (8.10); every other transport limit is set with `ServerBuilder::transport_limits` / `ListenerSpec::transport_limits` taking `net_io::Limits`. `Limits` stays `Copy` with `const` setters | one place per limit; no second copy of net-io's table |
| D-015 | 2026-10-23 | No `AcceptPolicy`: accept batches and backoff are net-io's; `accept_failure_window` stays in serve-net for health | net-io implements the accept loop |
| D-016 | 2026-10-23 | No `CloseMode`: `close_door()` takes no argument | net-io's `close_door` keeps listening sockets open until shutdown and hand-off uses duplicated sockets, so "release" and "keep for hand-off" behave the same |
| D-017 | 2026-10-23 | Per-request deadlines use net-io's exchange deadline; deferred replies use `Step::Park { until }`; keep-alive idle uses net-io's per-listener idle timeout; on a request deadline the session writes the 408 refusal and closes gracefully | the deadlines run on net-io's timing wheel, one per connection, with no second timer system |
| D-018 | 2026-10-23 | Keep-alive idle defaults by ingress: 20 s on direct listeners, 60 s behind a proxy or FastCGI front end (OD-9) | the backend must outlive the front end's reuse window, or the front end reuses a closing connection and answers 502; browsers hold many idle connections to a direct server |
| D-019 | 2026-10-23 | Replies are encoded into a pooled `OutBuf` and pushed whole into net-io's `Output` before `Step::Write`; static bodies with `push_static`; large full bodies after the head with `respond_head_only` | the egress check and `Checkpoint` see the whole reply; one copy of a body at most |
| D-020 | 2026-10-23 | `ReplyToken` completes through net-io's `Completer` (`post(connection, token, value)`), with the reply moved into a per-connection slot reused across requests; `Busy` when the completer is full; `Expired` when the generation moved | the completer carries a `u64`, not the reply; one slot per connection keeps steady state free of allocation |
| D-021 | 2026-10-23 | `wipe_buffers` is on by default (per profile), and turns on http-net's request wipe | matches net-io's default (its D-027); request buffers may hold credentials |
| D-022 | 2026-10-23 | `Admit` takes `&self` and the worker id (`admit(&self, worker, env)`), with `prepare(&mut self, workers)` before the server starts; `InFlightCap` keeps single-writer per-worker counters | net-io calls a session only on its worker, so per-worker state needs neither a lock nor a contended atomic (NFR-12); a `&mut Worker` argument would need a lock per call |
| D-023 | 2026-10-23 | Tier 1 entry points take a closure `Fn(&HttpRequest<'_>, &mut HttpReply)` so its argument types are inferred; a type implementing `HttpService` goes through `Server::builder()...serve(service)` | a closure passed to a generic `S: HttpService` parameter cannot infer higher-ranked argument types, which would force annotations on every Tier 1 user |
| D-024 | 2026-10-23 | Detached heads are copied (http-net `copy_into` into pooled vectors), never swapped (OD-2) | the connection buffer still holds pipelined input and the body window; about 20-60 ns per head [est.] |
| D-025 | 2026-10-23 | The engine trait is feed-based (`feed`, `feed_eof`, `poll`), matching http-net, ws-net, and fcgi-net; serve-net feeds it from net-io's input view | every engine in the stack has this shape; a borrowed-input mode is idea I-3 |
| D-026 | 2026-10-23 | `Running` and `Bound` are not generic; the hook types live inside the session factory | the type a program holds stays `Running` (net-io made the same choice for `Server`) |
| D-027 | 2026-10-23 | `Running::drain(self, deadline)` consumes the server; `DrainHandle::drain` starts a drain from any thread and `Running::wait` returns its report; dropping `Running` without a drain closes everything at once (OD-21) | a `Drop` must not block for seconds; a consumed server cannot be used after its drain |
| D-028 | 2026-10-23 | `ClientInfo::peer()` returns `Option<SocketAddr>` (the socket peer when it is an IP socket); Unix peers are described by `peer_credentials()` | keeps `ClientInfo` `Copy` and under 64 bytes; net-io's `Endpoint` holds 108-byte path arrays |
| D-029 | 2026-10-23 | `ProxySource` is a struct with constructors (`forwarded_for`, `headers`, `proxy_protocol`) and setters (`trust_scheme`, `trust_host`, `max_hops`, `missing`, `unknown`), not an enum with struct variants | a `#[non_exhaustive]` enum with struct variants cannot be built outside the crate; constructors read as one line |
| D-030 | 2026-10-23 | A `testing` module calls a service with raw request bytes and returns the reply, without sockets | services built on serve-net need unit tests that run the real view and reply types |
| D-031 | 2026-10-23 | Open decisions of the specification answered: OD-1 public and generic (D-006); OD-2 copy (D-024); OD-3 resolved by http-net 0.5 and 0.6; OD-4 push on the reactor, a pull reader on the blocking driver; OD-5 a trait (net-io's, D-009); OD-6 Windows service control in sys-lib behind a `Notify` adapter; OD-7 `MONOTONIC_USEC` as a parameter; OD-8 no signals in serve-net (examples show the owner's stop channel; revisit at 0.9); OD-9 ingress-dependent idle (D-018); OD-10 refuse a trusted peer without the forwarded header by default; OD-11 abort in release builds, `catch_service_panics` for unwind builds; OD-12 per-worker reuse-port sockets when net-io offers them, decided by benchmark at 0.6; OD-13 net-io's `Clock` and `ManualClock` (no clock of serve-net's own); OD-14 lanes are net-io's `Lane` (D-010); OD-15 FastCGI multiplexing after 1.0; OD-16 blocking default 256 threads; OD-17 publish 0.2.0 after approval (O-5); OD-18 503 with `Retry-After` over the soft cap (D-013); OD-19 ignore `Upgrade: h2c`; OD-20 serve-net's own wipe (D-021); OD-21 immediate close on drop (D-027); OD-22 `Workers::auto()` follows net-io's `Auto`; OD-23 directives rewritten as hook-order invariants | each recommendation was checked against net-io's 0.2 lock and http-net 0.6 |
| D-032 | 2026-10-23 | serve-net hands net-io a merged control line: the owner's line plus serve-net's own drain state (epoch = sum, state = the stricter, deadline = the earlier). `DrainHandle::drain` sets serve-net's half and wakes the workers | net-io's `request_stop` carries no deadline; with the merged line a drain requested from any thread uses its own deadline, and the owner's line keeps working unchanged |
| D-033 | 2026-10-23 | `Running::stats()` is always available from per-worker single-writer request counters; no separate `Counters` observer | operators expect counts without installing anything; one local increment per counter is about 1 ns [est.] |
| D-034 | 2026-10-23 | Deferral: `reply.defer()` returns the token and `call` returns a plain `Outcome::Deferred`. Generic families fill a `Reply<F>` (their `Family::Parts` plus the link serve-net needs to defer it); `HttpReply` is `Reply<HttpFamily>` with HTTP methods | with `Outcome::Deferred(token)` a service could not both hand the token to another thread and return it; one wrapper gives every family deferral |
| D-035 | 2026-10-23 | `ListenerSpec` and `ServerConfig` are not `Clone` | a handed-off socket (`net_io::ListenerHandle`) is move-only; a reload configuration is built anew from the program's configuration |
| D-036 | 2026-10-23 | Refinements found while building 0.3 against http-net 0.6: `EngineEvent::Head` carries no value (the head is read with `Engine::head()`); `Engine` gains `declared_length`, `expects_continue`, `head_len`, `keep_alive`; `Family::engine` also takes the profile's limits; `Family::head` takes a `HeadContext` (the connection's client and the listener's ingress rules) and returns the request's client with the view; `Family::respond` takes the time as Unix seconds and returns the status; `Family::Parts: Default`; `ClientInfo::direct` is public | a generic head view may have drop glue, so a head carried inside the event would keep the engine borrowed past the point where serve-net writes refusals; ingress normalization belongs to the family because only HTTP has forwarded headers |
| D-037 | 2026-10-23 | Until net-io 0.3 is published, `src/net.rs` defines the handful of net-io items serve-net names (`Lane`, `ConnectionId`, `WorkerId`, `Cidr`, `TrustedProxies`, `ControlLine`, `ControlState`, `NoControl`, `SimpleControl`, `DriverKind`, `ProxyVersions`) with net-io's documented shapes; the day net-io 0.3 is on crates.io the module becomes `pub use net_io::{..}`. The 0.3 work stays on its branch until then | lets the 0.3 code that does not need net-io's behaviour be final and tested now, with no `path` dependency |
| D-038 | 2026-10-23 | `HttpService` and `Service<F>` require `Send + Sync`, not `'static`; `bind` requires `'static` | `testing::respond(&service, ..)` borrows a service; a running server still needs an owned one |
| D-040 | 2026-10-23 | The forwarded walk scans each line once from the right with no storage (quoted strings tracked by toggling, which is exact because a backslash inside a forwarding field is refused), and the client resolved at the head is passed to the second view built for the service call (`HeadContext::resolved`) | measured on a shared development machine: the walk went from about 204 ns to about 165 ns for a 3-entry `X-Forwarded-For`, it runs once per request instead of twice, and a 1,000-entry header costs the same as a 17-entry one (the scan stops at the hop limit) |
| D-039 | 2026-10-23 | The per-wake pipelining bound yields with `Step::Write` (flush, then `on_writable` continues with input already inside the engine) | the engine takes all buffered input at once, so returning `Read` would wait for bytes that may never come and stall buffered requests |
| D-041 | 2026-10-23 | The engine keeps its buffer between keep-alive requests; idle memory goes back through per-worker engine pools with the reactor driver (0.5), not through `shrink_idle` on every return to idle. A connection's output buffer starts at 1 KiB | releasing and reallocating the buffer would cost an allocation per keep-alive request; the counting-allocator tests prove zero allocations per request in steady state for GET, a 4 KiB POST, pipelined batches, and refusals |
| D-042 | 2026-10-23 | `testing::TestConnection` keeps one simulated keep-alive connection across requests (`new`, `with_profile`, `behind`, `send`, `send_into`, `is_open`) | service authors test keep-alive and pipelining with it; the allocation tests, fuzz targets, and benchmarks use it from outside the crate, so they need no hidden API |
| D-043 | 2026-10-23 | Adjustments to net-io 0.3's final behaviour (net-io D-065, D-067, D-074, D-075, D-081): keep-alive under pressure calls `set_idle_timeout` at each response (the interim close at 0.75 is gone); on the blocking driver serve-net asks net-io for `soft cap + headroom` workers, because net-io refuses at accept once every worker is busy and the 503 can only be written by a worker; `DrainHandle::drain` calls `ServerHandle::wake_all` after setting serve-net's half of the merged line, since net-io's accept thread runs the drain itself; the machine keeps setting phases explicitly | net-io answered every request; these are the places serve-net's design meets net-io's real 0.3 code |
| D-044 | 2026-10-23 | On the blocking driver the soft connection cap defaults to the pool size minus its headroom (256 threads: soft cap 224, net-io hard cap 252), and a configured `max_connections` raises the pool to soft cap plus headroom | every open connection holds a thread, and net-io refuses at accept once every thread is busy, so a 503 over the soft cap can only be written if threads remain for it (D-043); with no configured cap the pool size is the real limit |
| D-045 | 2026-10-23 | The net-io session turns a `Read` or `Park` step into `Write` while output is queued, and resumes the step after the flush (a parked reply keeps waiting; anything else continues with `on_writable`) | the machine counts a reply as handed over once `push` takes it, but net-io sends queued output only on `Write` and `Close`; without this every keep-alive reply waited in the output buffer forever (found by the first loopback test). The machine and its simulated suite stay unchanged |
| D-046 | 2026-10-23 | When any listener is behind a front end, serve-net lifts net-io's per-source connection cap for the server (sets it to the hard cap); direct-only servers keep net-io's default (16 per address on the blocking driver) | behind a proxy every connection comes from the proxy, so a per-source cap caps the whole site; net-io 0.3 applies the cap server-wide only (a per-listener value is accepted and ignored, reported to net-io), so a server mixing direct and proxied listeners loses the per-source cap on its direct listeners until net-io applies it per listener. Per-client limits behind a proxy belong to a gate that sees the forwarded client |
| D-047 | 2026-10-29 | The machine reads the merged control line (the owner's line plus serve-net's drain request) at each checkpoint, and `Running::drain` sets serve-net's half before net-io drains | net-io's `SessionContext::draining()` is fixed for the length of one callback, so a drain that starts while the service runs was invisible to the reply being written; a loopback test found a reply in flight without `Connection: close`. The line costs one more acquire load per checkpoint (the sum of two epochs) |
| D-048 | 2026-10-29 | `Stats` and the drain report's request counts come from per-worker counters written only by their worker (a relaxed load and store per event, no read-modify-write) and summed on call; `refused_while_draining` counts every refusal written while a drain ran | the hot path pays one local store per counter; the 0.3 benchmark shows no measurable change in the keep-alive request path (about 330 ns on a shared machine) |
| D-049 | 2026-10-29 | `health` is computed on call: a listener is failing after accept errors for longer than `accept_failure_window` (or at once when net-io gives it up), read from a tap in front of the owner's event sink; a worker is stalled when it is inside a service call and its progress counter has not moved since the previous call; overload is more than 20% busy refusals since the previous call | no background thread and no per-request cost beyond the progress counter; a worker waiting on an idle keep-alive connection is not stalled, so only a service call that never returns is reported |
| D-050 | 2026-10-29 | The machine marks the start of every reply (`Io::begin_reply`, net-io's `Output::begin_response`), and a gate denial before the reply's first byte is answered with the engine's refusal for the gate's code (400 to 599, else 403) and a close | without the marker net-io can only tell whether any byte of the connection left, so only the first reply of a keep-alive connection could be refused cleanly |
| D-051 | 2026-10-29 | The hook set carries all eight slots of the locked API now, `HookSet<A, D, K, O, G, E, C, T>`, with the TLS slot fixed to `NoTls` until `tls` arrives in 0.4; the examples stop on Enter (the owner's channel, OD-8) | the type programs never name but whose shape is public stays the same from 0.3 to 1.0; no signal library enters the dependency tree |
| D-052 | 2026-10-29 | A pre-1.0 polish milestone (0.9.x, before any 1.0 release candidate) revisits every accepted gap, deferred item, known issue, and missed target, runs a fresh performance pass, and re-runs the security and edge-case review; what stays open goes into the 1.0 readiness note for James | James's standing rule for every crate: "make sure to go back and fix any issues / perf tune / etc before we get close to release"; the register in `dev/ROADMAP.md` (v0.9.x) is where a gap goes instead of being accepted quietly |
| D-053 | 2026-10-29 | serve-net 0.4 depends on net-io 0.4; the per-source lift of D-046 moves to each proxy listener's own limits, and direct listeners on the same server keep net-io's per-source default | net-io 0.4 counts the per-source cap per listener, which closes the gap D-046 recorded for servers that mix direct and proxied listeners |
