<h1 align="center">
    <img width="90px" height="auto" src="https://raw.githubusercontent.com/jamesgober/jamesgober/main/media/icons/hexagon-3.svg" alt="Triple Hexagon">
    <br><b>CHANGELOG</b>
</h1>
<p>
  All notable changes to <code>serve-net</code> will be documented in this file. The format is based on <a href="https://keepachangelog.com/en/1.1.0/">Keep a Changelog</a>,
  and this project adheres to <a href="https://semver.org/spec/v2.0.0.html/">Semantic Versioning</a>.
</p>

---

## [Unreleased]

---

## [0.8.1] - 2026-11-12

net-io 0.7.2, its security release (D-113).

### Security

- net-io 0.7.2 (from 0.7.1) fixes two critical and three high defects found by
  an independent review: on Windows, a crash or memory corruption when the gate
  denied a write on a named-pipe connection, and a forged listener hand-off
  message; on the reactor with TLS, silent connections holding the shared
  buffer budget; Unix sockets bound in a directory other users can write to;
  and named-pipe names Windows rewrites into file or network paths.

### Changed

- A Unix listener in a directory every user can write to, without the sticky
  bit, is refused at bind (net-io's `ConfigReason::InsecureDirectory`).

---

## [0.8.0] - 2026-11-11

The findings of an independent review of 0.7.1, each fixed with a test that
fails without it, on top of the 0.8 work below. FastCGI, net-io 0.8, and the
production-site exit move to 0.9.0 (D-112).

### Security

- On a TLS listener, a request could name another host in `Host` than the
  server name the client asked the TLS layer for, and serve-net served it as
  that host. A TLS acceptor that chose its policy by name (client
  certificates required for one host and not another) could be stepped
  around this way. Such a request is now answered `421 Misdirected Request`
  before `head` runs (T-42); `ListenerSpec::allow_sni_mismatch(reason)` turns
  the check off for a listener whose hosts share one TLS policy. Affects 0.4.0
  to 0.7.1 with `Ingress::DirectTls`. Advisory: [GHSA-2px9-pm7v-j3cj](https://github.com/jamesgober/serve-net/security/advisories/GHSA-2px9-pm7v-j3cj).
- WebSocket upgrades accepted any `Origin`, so a page on another site could
  open a socket with the visitor's cookies (cross-site WebSocket hijacking).
  Upgrades are now same-origin by default (`WebSocketTarget::origins`, below).
  Affects 0.6.0 to 0.7.1 with feature `ws`. Advisory: [GHSA-cjcj-vccf-cxfj](https://github.com/jamesgober/serve-net/security/advisories/GHSA-cjcj-vccf-cxfj).
- A panic in `done` (or the observer) kept the request's admission for the
  life of the server, so a few such requests made every later one a 503.
  Admission is now released on every path, a panic included.
- A panicking WebSocket handler with `catch_service_panics` off skipped
  `closed`, left every `WsSender` accepting sends into a queue nobody
  drained, and stayed in the drain registry. The connection now closes them,
  and `closed` runs once after `opened`.
- A reply body was copied whole into the connection's output buffer and held
  until the client read it: slow readers of a large body could hold one copy
  each (eight clients of an 8 MiB `body_static` held 64 MiB). The body is now
  sent from where the service put it, and a slow reader holds at most the
  output budget.
- `Limits::max_deferred_per_worker` did not cap parked requests as
  documented; it now does, and a deferral past it gets the busy refusal (T-26).
- `RuleId::PublicBind` did not check sockets passed in by systemd or a
  supervisor, or an inherited listener's fallback address; it now does, at
  build for the fallback and at bind, before the door opens, for a passed
  socket.
- On a wiping profile, a buffer that grows now zeroes the block it leaves
  (the request body, the reply head, WebSocket messages); a deferred reply
  that is replaced, never sent, or dropped with its token is zeroed; an
  offloaded request's body and a `DetachedHead` are zeroed when dropped.

### Added

- `WebSocketTarget::origins`: which web pages may open a WebSocket, checked
  before admission. Same-origin by default; a page on another origin is
  answered 403 (`RefusalKind::CrossOrigin`), so a browser's cookies no longer
  let another site's page talk to a WebSocket service as its visitor
  (cross-site WebSocket hijacking). The page is compared with the request's
  authority as serve-net resolves it, the trusted proxy's with `trust_host`
  on. A request with no `Origin` (not a browser) is accepted.
- `Family::upgrade_allowed`, a provided method that judges an upgrade on the
  head view; the default allows it.
- `docs/ARCHITECTURE.md`: how the crate fits together, from the builder to
  one request's path through the connection machine.
- A complete custom family in `tests/custom_family.rs`: a key-value line
  protocol served through `bind_family`, built from the public API alone.
- `Profile::wipes_buffers`, so a custom family's engine can zero the
  client's bytes it holds under a wiping profile, as the HTTP engine does.
- `Envelope::tls` and `HeadContext::tls`: the TLS handshake's facts (the
  server name, ALPN, version, cipher suite, and the SHA-256 of a verified
  client certificate's public key) on a connection serve-net decrypts.
- `ListenerSpec::allow_sni_mismatch`, `HeadContext::checks_sni`, and
  `RefusalKind::Misdirected`.
- `Family::reply_body`, a provided method: the body that follows what
  `respond` wrote, sent by serve-net from the reply data.
- `DrainReport::stuck`: `Running::drain` waits at most 5 s past its deadline
  for net-io's shutdown, and returns with this set when a worker never came
  back.

### Changed

- `catch_service_panics` covers `head`, `done`, and `upgraded` too: a panic
  in `head` answers the 500 refusal instead of a reset, and `done` still runs
  for that request with the option on or off.
- A reload that changes the transport limits net-io enforces on a kept
  listener (`write_stall`, `min_rate`, `max_pending_output`, `linger`,
  keep-alive idle, the read timeout, its hard and per-source caps, which
  moving it between direct and forwarded ingress changes) is refused with
  `ReloadListener`. Before, it was reported as applied and never took effect.
  Re-add the listener under a new name, or restart.
- Each worker reads the server's tables through its own cache when it
  accepts a connection: one atomic when nothing changed, instead of two
  takes of a lock all workers share.
- The README, the crate description, and the docs no longer present FastCGI
  as available; it is planned for 0.9.0.
- ws-net 0.9 (from 0.2): its fixes since 0.2 reach serve-net's WebSocket
  connections, among them an allocation failure answered with 1011 instead of
  aborting the process, and a 5 s close timeout armed from the start.
  `ws_net` types are part of serve-net's API, so code that names them moves
  to ws-net 0.9 too.
- net-io 0.7.1 (from 0.6.2): its hardening reaches every connection, among
  them TLS gate denials sent as an encrypted record and released buffers
  wiped in full, and idle TLS connections give their buffers back on the
  reactor. `net_io` types are part of serve-net's API, so code that
  names them moves to net-io 0.7 too.
- On the reactor with no `max_connections`, the soft cap now follows the
  process's descriptor limit along with net-io's hard cap: under a low
  `RLIMIT_NOFILE` (256 on a stock macOS) connections past the soft cap get
  the busy refusal (503) instead of a reset, and the profile caps still
  apply. The default stays 8,750 of 10,000 where descriptors allow.
- The TLS tests use tls-net 0.9. serve-net takes any `net_io::TlsAcceptor`
  and does not depend on tls-net; a service that uses tls-net should move
  to 0.9, which fixes a panic any client could cause in a session's
  `encrypt` or `close`.

### Fixed

- `BodyPolicy::Discard { max }` counted the head toward `max`, so a long head
  closed a connection whose small body was well under it.
- After a panic in `call` with `catch_service_panics` off, `Running::health`
  reported the worker stalled while it sat idle.
- A listener is refused when its profile's engine belongs to another
  family than the one being bound (`RuleId::EngineFamily`): `bind` served
  HTTP on a profile marked `EngineChoice::Custom`, and `bind_family` served
  a custom family on HTTP profiles. Checked at `bind`, `bind_family`, and
  reload. A server for a custom family now needs its own profile 0 with
  `EngineChoice::Custom`, since the default profile is HTTP.
- A wiping profile now zeroes ws-net's own input, message, and output
  buffers when an upgraded connection ends, not only serve-net's copies.
- A gate that denied the 101 of a WebSocket upgrade closed the connection
  with nothing written, after the service had already been told the socket
  was open. The 101 is now a reply like any other until it leaves: a denial
  is answered with the gate's status, the request ends `Denied`, and
  `upgraded` and `opened` run only after the 101 has gone out.

---

## [0.7.1] - 2026-11-07

### Fixed

- The busy refusal at the soft connection cap closed without the linger: it
  is written before anything is read, while net-io still counted the
  connection idle, so the client's request on its way drew a reset, and on
  Windows the client saw an aborted connection instead of the 503. The
  refusal now marks the connection as sending and closes with the linger
  like every other reply.

---

## [0.7.0] - 2026-11-07

Hardening: TLS with ALPN, the idle trim, WebSocket push, a 24-hour soak, and
an adversarial review with every finding fixed.

### Added

- `EngineChoice::Http1OrH2ByAlpn`: the engine by the TLS handshake's ALPN
  result. HTTP/1.1 for `http/1.1` or no ALPN; `h2` closes with
  `EndReason::NoEngine` until the HTTP/2 engine exists. Only on TLS listeners
  (`RuleId::AlpnWithoutTls`).
- An end-to-end TLS test with a real engine (tls-net 0.5 on ring, a
  dev-dependency only) and the ALPN cases.
- `docs/SECURITY.md` and a root `SECURITY.md`; attack tests for T-15, T-18,
  T-32, T-33, and T-40; kill-at-every-step restart tests; the leak test; loom
  models for the drain and the WebSocket sender.
- `WsSender` (`WebSocketConn::sender`): send on an open WebSocket from any
  thread, through a queue bounded by the connection's output budget.
- A drain closes open WebSockets with 1001 first.
- `WebSocketTarget::message_timeout` (30 s by default): a message trickled past
  it closes with 1008.
- `RuleId::AnyLocalPeerForwarded`, a build warning for forwarded headers on a
  Unix socket open to any local peer.
- `Envelope::gate_tag`: the `GateTag` the gate returned in `Gate::admit`, so a
  gate integration needs no map from connection id to tag. 8 bytes per
  connection (the envelope grows from 120 to 128 bytes on 64-bit targets).
  `GateTag` is re-exported.

### Fixed

- A reload that lowers `max_body` reaches a streamed body (`BodyPolicy::Stream`)
  on an open connection at its next request, as it already did for buffered
  bodies.
- V-24 is enforced: `trust_scheme` or `trust_host` with a single-address
  header or PROXY v1, which carry neither, is refused at build
  (`RuleId::SchemeTrustWithoutSource`).
- Restarts on Windows (T-30): the restart tests run on Windows as well. The
  resets once seen there were the new server's own per-source cap refusing
  the old server's clients; `docs/DEPLOYMENT.md` now says to start the new
  server with the old one's caps or more.

### Changed

- net-io 0.6.2. An idle keep-alive connection gives back its buffers after one
  second of quiet: about 4 KB per idle connection, from about 9 KB. A drain no
  longer cuts a request that has already arrived, and on Windows a worker no
  longer deadlocks at thread exit and the listen backlog is no longer capped
  at 200.
- A connection whose ALPN result names a protocol its profile has no engine for
  is closed with `EndReason::NoEngine` before any byte reaches an engine. Before,
  an `Http1` profile fed an HTTP/2 connection to the HTTP/1.1 parser, which
  answered 400.

### Moved

- To 0.8.0: FastCGI (with T-19 and the `fastcgi_mapping` fuzz target), which
  waits for fcgi-net.

---

## [0.6.3] - 2026-11-06

Three findings of an adversarial review, each fixed with a regression test.

### Security

- After a WebSocket upgrade, input the HTTP engine had not yet taken in the
  same read was fed back to it and parsed as a new HTTP request: a request
  smuggled behind about 64 KiB of frames reached `call`, and behind a trusted
  proxy (which forwards an upgraded connection as a tunnel) its own
  `X-Forwarded-For` was believed. It now goes to the upgraded protocol.
  Affects 0.6.0 to 0.6.2 with feature `ws`, when net-io reads more than the
  HTTP engine's free buffer at once (above net-io's default 16 KiB read size),
  and the test transport.
- With `ForwardedSource::Forwarded` and `MissingHeader::UsePeer`, a request
  from a trusted peer with no `Forwarded` element had `X-Forwarded-Host` and
  `X-Forwarded-Proto` believed at `head`, fields an RFC 7239 proxy neither sets
  nor strips; `call` saw the `Host` field. Neither is read for that source now.
  Affects 0.3.0 to 0.6.2 with that source, `UsePeer`, and `trust_host` or
  `trust_scheme` on.
- A PROXY v2 SSL TLV marked the client `https` whenever it was present. A
  proxy that sends the TLV on every connection (HAProxy's
  `send-proxy-v2-ssl`) sets the `PP2_CLIENT_SSL` bit only for TLS clients, so
  plain clients were reported as `https`. The bit is checked now. Affects 0.4.0
  to 0.6.2 on PROXY listeners with `trust_scheme` on.
- Advisory: [GHSA-jcp7-qqq6-xw24](https://github.com/jamesgober/serve-net/security/advisories/GHSA-jcp7-qqq6-xw24). 0.6.2 is yanked from crates.io.

---

## [0.6.2] - 2026-11-04

### Security

- With `ProxySource::trust_host(true)`, a client that was not a trusted proxy
  could set the authority the service saw in `call` by sending
  `X-Forwarded-Host` or `Forwarded: host=`. `head` saw the right authority; the
  view of the request built again for `call` took the host field without
  checking that the walk had passed a trusted hop. The authority now comes from
  a forwarded field only when the peer is a trusted proxy, at `head` and at
  `call` alike. Affects 0.3.0 to 0.6.1 on listeners behind forwarded headers
  with `trust_host` on; the client address and the scheme were not affected.
  Advisory: [GHSA-7vvv-g6cw-p8gj](https://github.com/jamesgober/serve-net/security/advisories/GHSA-7vvv-g6cw-p8gj). 0.3.0 to 0.6.1 are yanked from
  crates.io.

### Added

- `tests/threats.rs`: attack tests for T-3 (a client that never reads),
  T-6 (10,000 pipelined requests), T-8, T-13, T-17, T-29 (a request kept
  open through a drain), T-34, and T-35.
- The `forwarded_proto_host` fuzz target (T-17), which found the issue above;
  its oracle requires `head` and `call` to see the same request.

---

## [0.6.1] - 2026-11-04

### Security

- net-io 0.6. net-io 0.5.0 did not apply the minimum transfer rate on the
  reactor to a peer that sent more often than the check interval, so a client
  trickling a byte every 40 ms kept its connection (slowloris). The reactor is
  serve-net's default driver, so 0.5.0 and 0.6.0 were affected with default
  settings; the blocking pool was not.

### Fixed

- serve-net 0.5.0 and 0.6.0 could not be added to a new project: they require
  net-io 0.5, whose only release is yanked. 0.6.1 requires net-io 0.6.

---

## [0.6.0] - 2026-11-04

Upgrades: WebSocket on the same listener, a run under real systemd in CI,
and less work per request.

### Added

- WebSocket upgrades (feature `ws`, over ws-net 0.2): `HeadAction::Upgrade`
  with `WebSocketTarget`, `WebSocketService` called once per message on the
  worker, `WebSocketConn` to send, `WsMessage`, `WsSendError`;
  `UpgradedConn`, `HttpService::upgraded`, `Family::upgrade`, and
  `RequestEnd::Upgraded`. Each message passes the control line and the
  checkpoint; pings after a quiet interval; output bounded per connection.
- `examples/systemd.rs` and a CI job that runs it under real systemd as a
  `Type=notify-reload` unit with a watchdog, reloaded ten times under load.
- `docs/PERFORMANCE.md`: the method, the hardware, and each performance
  target's status; `docs/STABILITY-1.0.md`, the blockers to 1.0 as they
  become known.

### Changed

- `HeadAction` is `Clone` and no longer `Copy` or `PartialEq`.
- About a fifth less of serve-net's own work per keep-alive request (about
  220 to 172 ns; 308 to 267 ns for the whole request): the request path no longer
  clones the server's shared state per request, and release builds no longer
  read the clock around every service call.
- Every CI job has a time limit.

### Fixed

- The systemd CI script could hang: a user manager reporting `degraded` made
  it fall back to `sudo`, whose password prompt under `timeout` stopped the
  job with `SIGTTIN`. It now counts any manager state as up, runs `sudo -n`,
  and limits every call; the unit uses `KillMode=mixed` so the stop reaches
  the wrapper and the server drains. Found in a local run; CI and the library
  were not affected.

### Moved

- To 0.7.0: FastCGI and `Http1OrH2ByAlpn` with an end-to-end TLS test, which
  wait for fcgi-net and tls-net; the idle memory trim, WebSocket sends from
  another thread, and a 1001 Close at drain start, which wait for net-io 0.6.

---

## [0.5.0] - 2026-11-03

The reactor: thousands of connections per worker, streaming bodies, an
offload pool for slow work, and restarts that no longer lose requests under
load.

### Added

- `ServerBuilder::catch_service_panics`: a caught panic answers 500, closes
  the connection, and reports `RequestEnd::ServicePanic`.
- `InFlightCap::reserve`: capacity kept for one lane; ordinary traffic cannot
  take it. Rule `ReserveOverflow` is now checked. `Admit::driver` and
  `Admit::reserved_percent`, both with defaults.
- `Offload`, `OffloadPool`, `OffloadJob`: `Io` and `Cpu` services run on a
  bounded pool the owner provides; a full pool answers the busy refusal.
  `Family::detach` and `DetachedRequest` for custom families.
- Streaming: `BodyPolicy::Stream` with `HttpBody::Chunk`, `HttpReply::stream`
  and `ReplyStream`, and `Outcome::More`. A slow reader holds back the service,
  not memory. Provided `Family` methods for custom families.
- `Budget::dedicated_workers`: workers kept for one profile, out of reach of
  a flood on other listeners.
- `Workers::reactor`, and `Workers::auto()` now picks the reactor: one worker
  per logical CPU, thousands of connections on each. A service that blocks
  delays every connection of its worker; use `Offload`, or `Workers::blocking`.
- `WatchdogPinger`: pings a service manager only while every worker is
  healthy, so a stuck worker gets the process restarted.

### Changed

- http-net 0.9, net-io 0.5.
- The default driver is the reactor, and `Limits::max_deferred_per_worker`
  reaches net-io's completion queue on it.
- `Outcome` and `BodyPolicy` gain a variant each; both are `#[non_exhaustive]`.

### Fixed

- A drain could reset connections accepted as the door closed, with their
  request unread, so a restart under load lost requests.
- A blocking pool of 17 threads or fewer with no `max_connections` had a soft
  connection cap of 1 and served one client at a time; the headroom is now at
  most a quarter of the pool.

### Moved

- WebSocket upgrades, FastCGI, `Http1OrH2ByAlpn`, and an end-to-end TLS test
  move to 0.6.0. FastCGI and TLS wait for fcgi-net and tls-net.

---

## [0.4.0] - 2026-11-02

Deployment: behind any front end, over Unix sockets, under systemd, and
through a restart or a reload without dropping a connection.

### Added

- PROXY protocol ingress (`ProxySource::proxy_protocol`): v1 and v2, `LOCAL`,
  a trusted list per listener, the SSL and authority fields.
- Unix socket listeners: `ListenerSpec::unix`, `UnixMode`, `UnixPeerTrust`,
  `Ingress::Unix`, `ClientInfo::peer_credentials`.
- `ListenerSpec::inherited` with `fallback_bind` (systemd socket activation)
  and `handed_off`; `ServeError::Inherited` and `HandOff`.
- The TLS seam: `Ingress::DirectTls` and `ServerBuilder::tls`, for any
  `net_io::TlsAcceptor`.
- `Running::reload`, `ServerConfig`, `ReloadSummary`, `ServerBuilder::config`;
  `ServeError::Reload` and `ReloadBind`.
- `Notify`, `NoNotify`, `SystemdNotify`, `ServerBuilder::notify`.
- `Finished::head_reads`.
- From the 0.3 API, missing in 0.3.0: `ServerBuilder::transport_limits` and
  `clock`, `ListenerSpec::listen_options` and `transport_limits`,
  `HttpRequest::detach_head` and `DetachedHead`, `Observer::drain_started`,
  `drain_finished`, and calls to `capacity_refused` and `public_bind`.
- `Observer::reloaded`, `reload_refused`, `notify_failed`.
- Rules `IngressSource` and `ReloadListener`.
- `docs/DEPLOYMENT.md`; an interop CI job with real nginx, Apache, and HAProxy.

### Changed

- net-io 0.4.
- `Family::head` takes `&HeadContext<'a>`, the head's own lifetime.
- `ListenerSource` gains `Unix`, `Inherited`, and `HandedOff`.

### Fixed

- A head, body, or reply timeout closed the connection silently on real
  sockets; the 408 or 503 now reaches the client.
- serve-net's `min_rate`, `max_pending_output`, and `linger` were not passed
  to net-io, and net-io's 30 s read timeout could cut off a longer
  `request_timeout`.

### Moved

- FastCGI, WebSocket upgrades, `Http1OrH2ByAlpn`, and an end-to-end TLS test
  move to 0.5.0: fcgi-net, ws-net, and tls-net are not built yet.

---

## [0.3.0] - 2026-10-29

The first working server: HTTP/1.1 over net-io 0.3's blocking driver, with
every default limit and defence on, behind a reverse proxy or facing clients
directly.

### Added

- Tier 1: `serve_net::http(addr, handler)` and
  `serve_net::http_behind_proxy(addr, trusted, handler)`.
- Building and running: `Server::builder()` with listeners, profiles,
  limits, keep-alive, workers, and every hook (`admit`, `dispatch`,
  `checkpoint`, `observer`, `gate`, `events`, `control`); `build`, `Built`,
  `bind`, `bind_family`, `Bound`, `open`, `serve`; `Running` with
  `local_addr`, `listeners`, `close_door`, `drain`, `drain_handle`, `wait`,
  `stats`, `health`, `server_handle`; `DrainHandle`, `DrainReport`,
  `ListenerInfo`, `ListenerState`, `Health`, `HealthIssue`, `Stats`.
- Ingress: direct clients and trusted forwarded headers (`X-Forwarded-For`,
  RFC 7239 `Forwarded`, a single address header) with the right-to-left
  walk, client normalization (`ClientInfo`, `Scheme`, `IngressKind`).
- The HTTP family over http-net 0.6 (`HttpService`, `HttpHead`,
  `HttpRequest`, `HttpReply`, `handler`), deferred replies (`ReplyToken`),
  admission (`InFlightCap`), checkpoints, observation, `Snapshot`, and the
  engine and family traits for custom protocols.
- Two-tier connection caps: soft caps answered with 503 and a jittered
  `Retry-After`, net-io's hard caps above them; keep-alive shortened under
  pressure; gate denials answered with the engine's refusal.
- `testing`: `respond`, `TestRequest`, `TestReply`, `TestConnection`.
- Examples `hello` and `behind_proxy`.
- Tests: the simulated-connection suite, loopback socket tests on three
  operating systems (keep-alive, pipelining, drain with a request in flight,
  gate denial, proxies, the leak test), counting-allocator tests (zero
  allocations per request in steady state), property tests, fuzz targets,
  loom models, and criterion benchmarks.

### Changed

- The net-io items serve-net names (`ConnectionId`, `WorkerId`, `Lane`,
  `Cidr`, `TrustedProxies`, `ControlLine`, `ControlState`, `Gate`,
  `EventSink`, `CloseReason`, and the rest) are re-exported from net-io, and
  the `net_io` and `http_net` crates themselves.

---

## [0.2.0] - 2026-10-29

The design lock: the public API is written down in full and the design
behind it recorded, reconciled with net-io's own 0.2 design lock. Still no
functional code; `0.3.0` is the first working server.

### Added

- `docs/API.md`: every public item from `0.3.0` to `1.0.0` with its
  signature, behaviour, errors, cost target, grade, tier, the version that
  ships it, and examples. Tier 1 is `http`, `http_behind_proxy`, and
  `fastcgi`; Tier 2 is `Server::builder()`; Tier 3 covers custom protocol
  engines and families, admission, dispatch, revocation checkpoints,
  observation, readiness, and live tables.
- `dev/PLAN.md`: the requirements (FR-1 to FR-69, NFR-1 to NFR-24), what
  net-io does and what serve-net does, the connection state machine, ingress
  normalization and the trusted walk, admission and lanes, the control line,
  drain and restarts, reload, 40 threats each with its defence, test, and
  cost, 26 validation rules, performance targets, the testing plan, the
  enterprise and distributed requirements, upstream requests, and decisions
  D-006 to D-035.
- `dev/ROADMAP.md`: core (0.3), extended (0.4), feature complete (0.5),
  optimisation (0.6), hardening (0.7), alpha (0.8), beta (0.9), stable
  (1.0), with the upstream version each one needs.

### Changed

- serve-net is now the public, generic engine that applications wrap: the
  fixed security pipeline and the service core were removed from its scope
  (the wrapping application builds them on serve-net's hooks), and the
  connection gate is installed on net-io by the owner.
- serve-net runs on net-io's server: its per-connection engine is a net-io
  session, and net-io owns accepting, worker threads, the door, transport
  deadlines, hard caps, kill, and drain mechanics.
- `dev/DIRECTIVES.md`: the project invariants were rewritten as hand-off
  order invariants.
- The crate description, README, and crate documentation were brought in
  line with the new scope.

### Removed

- CGI mode. Services run as persistent processes; FastCGI covers shared
  hosting.

---

## [0.1.0] - 2026-10-13

The scaffold and the plan. No functional code yet.

### Added

- Crate manifest (edition 2024, MSRV 1.85, no dependencies), `Apache-2.0 OR MIT`
  license, README, REPS, CI (Linux, macOS, Windows on stable and 1.85; audit
  and deny), `deny.toml`, `clippy.toml`, `rustfmt.toml`, `.gitattributes`.
- `dev/PLAN.md` (purpose, scope, architecture, threats, ecosystem, open
  decisions, decision log), `dev/DIRECTIVES.md`, and `dev/ROADMAP.md`.

[Unreleased]: https://github.com/jamesgober/serve-net/compare/v0.8.1...HEAD
[0.8.1]: https://github.com/jamesgober/serve-net/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/jamesgober/serve-net/compare/v0.7.1...v0.8.0
[0.7.1]: https://github.com/jamesgober/serve-net/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/jamesgober/serve-net/compare/v0.6.3...v0.7.0
[0.6.3]: https://github.com/jamesgober/serve-net/compare/v0.6.2...v0.6.3
[0.6.2]: https://github.com/jamesgober/serve-net/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/jamesgober/serve-net/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/jamesgober/serve-net/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/jamesgober/serve-net/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/jamesgober/serve-net/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/jamesgober/serve-net/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/jamesgober/serve-net/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/serve-net/releases/tag/v0.1.0
