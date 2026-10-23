# serve-net - Roadmap

> Path from scaffold to a stable 1.0. Hard parts are front-loaded; each phase has hard exit criteria.
> Design and decisions: [`PLAN.md`](./PLAN.md). The public API, item by item with the version that ships it: [`../docs/API.md`](../docs/API.md).
>
> **Anti-deferral rule:** no listed hard task moves to a later phase unless this file records the move and the reason.
>
> **After every version:** the release routine (section "Release routine" below), then the next version starts.

## Upstream versions each serve-net version needs

A serve-net version is published only when every first-party crate it depends on is on crates.io at the version it needs. serve-net never uses a `path` dependency to get around a wait: a version that is built but blocked is finished, recorded here, and released the day its dependency is published.

| serve-net | Needs |
|---|---|
| 0.2 | net-io 0.2 design lock (published 2026-10-23); http-net 0.6 (published) |
| 0.3 | **net-io 0.3 published** (TCP, the blocking driver, sessions, completer, control line, events); http-net 0.6 |
| 0.4 | net-io 0.4 (PROXY, Unix sockets, the TLS seam with ALPN, inherited and handed-off listeners, descriptor 0); fcgi-net 0.3; ws-net 0.2; tls-net for direct TLS; net-io runtime listener add and remove for reload |
| 0.5 | net-io 0.5 (the reactor, Windows AF_UNIX) |
| 0.6 | http-net 0.6 or later (reply reuse: done) |
| 0.7 to 1.0 | net-io 0.7 or later (its hardening), so serve-net's soak runs on hardened transport |

## Reconciliation with the 0.1 roadmap (recorded 2026-10-23)

| 0.1 roadmap | Now | Change and reason |
|---|---|---|
| 0.2 design lock with pipeline slots and a core contract | 0.2 design lock | the fixed security pipeline and the service core are no longer serve-net's (D-006); the lock is reconciled with net-io's 0.2 lock instead (D-007 to D-035) |
| 0.3 thin slice behind Apache with door, egress, and every pipeline slot | 0.3 core | the gate and the pipeline are the owner's; 0.3 serves a browser on localhost on Windows and Linux, behind a proxy with forwarded headers |
| 0.4 every ingress mode including CGI | 0.4 extended | CGI dropped (D-006); PROXY, Unix, FastCGI, TLS, WebSocket, hand-off, systemd, reload |
| 0.5 scale | 0.5 feature complete | adds every remaining P0 to P2 item |
| (none) | 0.6 optimisation | new, required by the portfolio standard |
| 0.9 hardening | 0.7 hardening | moved earlier, where the standard places it; 0.9 becomes beta |
| (none) | 0.8 alpha, 0.9 beta | new |

No hard task moved later.

---

## v0.1.0 - Scaffold (DONE, 2026-10-13)
- [x] Manifest, README, CHANGELOG, REPS, dual license, CI, deny, clippy, rustfmt, DIRECTIVES, ROADMAP, PLAN.

## v0.2.0 - Design lock (CURRENT)
Exit: James approves `docs/API.md`; every open decision answered or carried with a reason; CI green.
- [x] `docs/API.md`: every public item with signature, behaviour, errors, cost, grade, tier, version, examples.
- [x] `dev/PLAN.md` rewritten: role (D-006), the division of work with net-io (8.2), requirements FR-1 to FR-69 and NFR-1 to NFR-24, threats T-1 to T-40, validation rules, enterprise and distributed (section 6), decisions D-006 to D-035.
- [x] Stale text removed: CGI and the fixed security pipeline out of `Cargo.toml`, `README.md`, `src/lib.rs`, `dev/PLAN.md`, `dev/DIRECTIVES.md`, this file.
- [x] Engine and family traits checked against http-net's `ServerConn`, ws-net's `Connection`, fcgi-net's planned responder, and several requests per connection (HTTP/2, multiplexed FastCGI).
- [x] Hook names agreed with net-io's 0.2 API (`ControlLine`, `Gate`, `EventSink`, `Lane`, `TrustedProxies`, `ListenerHandle`, `Completer`).
- [x] Upstream requests written (PLAN section 12): `TrustedProxies` in net-io 0.3; a per-connection idle override; runtime listener add and remove; per-listener PROXY trust; the raw monotonic clock. http-net reply reuse (OD-3) found already done in 0.5 and 0.6.
- [ ] James approves `docs/API.md` (PLAN O-4).
- [ ] Release routine: 0.2.0 tagged, released, published to claim the name (PLAN O-5).

## v0.3.0 - Core
Exit: a browser and curl get a page from `serve_net::http("127.0.0.1:8080", ...)` on Windows and WSL Ubuntu; malformed requests are refused; a drain with a slow request in flight works; every state of the connection state machine is covered by the simulated-connection suite; CI green on three OSes.

**Blocked: needs net-io 0.3 on crates.io** (in progress in the net-io repository as of 2026-10-23). Everything below that does not need net-io's code is built first, on branch `feature/0.3-core`; the parts marked (net-io) are written the day net-io 0.3 is published. No `path` dependency in the meantime.

| Area | Items (`docs/API.md`) | Needs net-io code |
|---|---|---|
| Tier 1 | `http`, `http_behind_proxy`, `handler`, `VERSION` | yes (start) |
| Building | `Server`, `ServerBuilder` (configuration and hooks), `build`, `Built`, `ConfigWarning` | no |
| Running | `bind`, `bind_family`, `Bound`, `open`, `serve`, `Running`, `close_door`, `drain`, `wait`, `DrainHandle`, `DrainReport`, `ListenerInfo`, `health`, `stats` | yes |
| Listeners | `ListenerSpec::tcp` and options, `ListenerSource` (`Tcp`) | no (the translation into net-io's specs: yes) |
| Ingress | `Ingress::Direct`, `Ingress::Proxy` with forwarded headers, `ProxySource`, `ForwardedSource`, `MissingHeader`, `UnknownClient` | no (the trust list type: net-io's `TrustedProxies`, requested for 0.3) |
| Normalization | `ClientInfo`, `Scheme`, `IngressKind`, the trusted walk | no |
| Requests | `Envelope`, `RequestSeq`, `HttpService`, `HttpHead`, `Fields`, `HeadAction`, `BodyPolicy` (`Buffer`, `Discard`), `HttpRequest`, `HttpBody`, `HttpReply`, `Outcome`, `Finished`, `RequestEnd`, `WorkClass`, `DetachedHead` | no |
| Deferral | `ReplyToken`, `CompleteError` | the completer: yes |
| Profiles | `Profile`, `ProfileId`, `Budget`, `ProfileTable`, `EngineChoice` (`Http1`, `Custom`) | no |
| Limits | `Limits`, `KeepAlive`, `Workers` (blocking, auto) | no (mapping onto net-io limits: yes) |
| Hooks | `Hooks`, `HookSet`, `DefaultHooks`, `Admit`, `InFlightCap`, `Dispatch`, `Inline`, `Checkpoint`, `NoCheckpoint`, `Observer`, `NoObserver` | no |
| Errors | `ServeError`, `ConfigErrors`, `ConfigError`, `RuleId`, `Subject`, `EndReason` | `ServeError::Bind` carries net-io's error |
| Engines | `Engine`, `EngineEvent`, `EngineProgress`, `EngineError`, `Refusal`, `RefusalKind`, `OutBuf`, `Family`, `HttpFamily`, `Reply`, `Service`, `NoCustom`, `engine::Http1` | no |
| Connection | the state machine (events in, actions out), deadlines, body buffering, pipelining, refusals | no |
| Session | the net-io `Session` and `SessionFactory` over the state machine; soft caps; the merged control line | yes |
| Snapshots | `Snapshot`, `SnapshotReader` | no |
| Testing | `testing::respond`, `TestRequest`, `TestReply` | no |

Tests: threats T-1 to T-9, T-11 to T-17, T-22 to T-24, T-26, T-28, T-29, T-32 to T-36, T-40; invariants 1 to 10 of PLAN section 11; the simulated-connection suite; validation rules; fuzz targets `forwarded_xff`, `forwarded_rfc7239`, `forwarded_proto_host`, `conn_sim`, `config` in CI smoke mode; loom models for `Snapshot`, the deferred-reply slot, the soft-cap counters, and drain; the counting-allocator test; benchmarks for the walk, the checkpoint, `InFlightCap`, `Snapshot`, and the null-engine loop.

net-io answered every request (2026-10-23, net-io D-081): `TrustedProxies` and `SessionContext::set_idle_timeout` ship in net-io 0.3.0; listener add and remove, per-listener PROXY trust, and the raw monotonic clock in 0.4.0. Keep-alive under pressure is now as specified (D-043).

**Progress (branch `feature/0.3-core`, 2026-10-23).** Built and tested without net-io: the forwarded-header parser and trusted walk; `ClientInfo`, `Scheme`, `IngressKind`; `Limits`, `KeepAlive`, `Profile`, `Budget`, `ProfileTable`, `EngineChoice`, `Workers`, `ListenerSpec`, `Ingress`, `ProxySource`; every validation rule that applies to 0.3; `Server::builder()` to `build()` with every hook setter; `Envelope`; the engine and family traits, `OutBuf`, refusals; `engine::Http1` over http-net 0.6; the HTTP family (`HttpService`, `HttpHead`, `HttpRequest`, `HttpReply`, `handler`); deferred replies (`Reply`, `ReplyToken`, `CompleteError`); `InFlightCap`, `Inline`, `Checkpoint`, `Observer`; `Snapshot`; the connection state machine with its simulated-connection suite (T-1, T-2, T-5, T-7, T-8, T-9, T-22, T-23, T-24, T-26, T-28, T-36; invariants 1, 4, 7); `testing` with `TestConnection`. Also done: the counting-allocator tests (zero allocations per request in steady state: GET, 4 KiB POST, pipelined batches, refusals; NFR-2, P-14); property tests for the walk and the machine (invariants 1 and 6 under arbitrary splits and output budgets); fuzz targets `forwarded_xff`, `forwarded_rfc7239`, `conn_sim`, `config` (clean after 60 s each, and a CI smoke job); loom models for `Snapshot`, the deferred-reply slot (complete against cancel, drop), and the shared in-flight counter, with a CI job; criterion benchmarks for the request path, the walk, the snapshot reader, and the control check. Measured on a shared development machine (only for comparing builds): keep-alive GET about 0.9 µs through engine and machine, the 3-entry walk about 165 ns (target 100 ns [est.], met or explained at 0.6), snapshot read and control check about 1 ns. Local gate green on Windows and WSL, stable and 1.85.

**Wire-up items.** Written on branch `feature/0.3-net-io` (stacked on `feature/0.3-core`) against net-io's 0.3 code before it reached crates.io, built with a command-line `[patch]` (`--config 'patch.crates-io.net-io.path=...'`), never a path dependency in the repository. The branch is pushed the day net-io 0.3 is published.
- [x] `net-io = "0.3"` in `Cargo.toml`; `src/net.rs` is `pub use net_io::{..}` (D-037); the `allow(dead_code)` notes that named net-io are gone. The crate-level `cfg_attr(not(feature = "http1"), allow(dead_code))` stays for another reason: without the HTTP family the forwarded walk has no reader.
- [x] `conn/session.rs`: net-io `Session` and `SessionFactory` over `Conn` (maps `Step`, `Phase`, `Timeout`, `set_idle_timeout`); soft caps (global, listener, profile) taken in `SessionFactory::open` and given back on drop; one `Completer` waker per worker; queued output flushed before `Read` or `Park` (D-045).
- [x] The merged control line (D-032) with `wake_all` after a drain request; `soft cap + headroom` workers on the blocking driver (D-043, D-044); the per-source cap behind a front end (D-046).
- [x] `Built::bind`, `bind_family`, `Bound` (`local_addrs`, `open`), `ServerBuilder::serve`, `Running` (`local_addr`, `local_addrs`, `close_door`, `drain`, `drain_handle`, `wait`, `server_handle`, abortive `Drop`), `DrainHandle`, `DrainReport`; `ServeError::Bind` naming the listener, `ServeError::Start`.
- [x] Tier 1 `http`.
- [x] Loopback tests (`tests/loopback.rs`): routes and keep-alive, pipelining, sequential connections, drain through `DrainHandle` closing an idle keep-alive connection, a bind conflict naming the listener, 24 connections from one peer behind a proxy.
- [x] A browser-style check: a local demo (not in the repository) served a page, `/health`, and a 404 on 127.0.0.1 with keep-alive, about 17,000 requests per second on one connection and 190,000 on 15 (p99 0.37 ms), and stopped cleanly on Ctrl+C.
- [ ] `on_denied` (the gate's refusal bytes) and `Output::begin_response`/`end_response` markers, so a gate can deny a reply; `ListenerInfo`, `health`, `stats` (request counters, D-033); `DrainReport::requests_finished` and `refused_while_draining` (0 until counted); gate and event-sink passthrough; `EndReason::Transport(CloseReason)`.
- [ ] Tier 1 `http_behind_proxy`; the `hello` and `behind_proxy` examples; loopback tests on Linux and macOS in CI; the leak test; `InFlightCap` benchmarks.

## v0.4.0 - Extended
Exit: the ingress equivalence matrix (PLAN section 11) passes for direct, PROXY v1 and v2, forwarded headers, Unix socket, and FastCGI (`mod_proxy_fcgi`, nginx `fastcgi_pass`, `mod_fcgid` descriptor 0); a WebSocket upgrade to ws-net works end to end; the restart test passes with a test supervisor (zero refused or reset); a systemd `Type=notify-reload` unit reloads under load with zero dropped connections in a container; the interop CI job (Apache, nginx, HAProxy containers) is green.
- API: `http_behind_proxy` with the PROXY option, `fastcgi`, `FastCgiListen`, `ServerConfig`, `ServerBuilder::config`, `reload`, `ReloadSummary`, `ListenerSpec::{unix, inherited, fastcgi_descriptor_zero, handed_off}`, `fallback_bind`, `unix_mode`, `UnixMode`, `Ingress::{DirectTls, Unix, FastCgi}`, `ProxySource::proxy_protocol`, `UnixPeerTrust`, `FastCgiIngress`, `ServerBuilder::tls`, `EngineChoice::{FastCgi, Http1OrH2ByAlpn}`, `UpgradeTarget`, `UpgradedConn`, `HttpService::upgraded`, `engine::FastCgi`, `engine::WebSocket`, `Notify`, `NoNotify`, `SystemdNotify`, `ServerBuilder::notify`, `Finished::head_reads`, `ClientInfo::peer_credentials`, `ServeError::{Inherited, HandOff, Reload, ReloadBind}`.
- Threat tests T-10, T-18 to T-21, T-30, T-31, T-38, T-39; fuzz target `fastcgi_mapping`.
- `docs/DEPLOYMENT.md`: Apache, nginx, HAProxy, systemd, the restart model.

## v0.5.0 - Feature complete
Exit: on net-io's reactor, 10,000 idle keep-alive connections with bounded memory on Linux, macOS, and Windows; deferred replies through an `Offload` pool; every P0, P1, and P2 item of `docs/API.md` implemented and documented; `docs/PLATFORM-NOTES.md` written.
- API: `Budget::dedicated_workers`, `BodyPolicy::Stream`, `HttpBody::Chunk`, `HttpReply::stream`, `Workers::reactor`, `catch_service_panics`, `InFlightCap::reserve`, `Offload`, `OffloadPool`, `OffloadJob`, `WatchdogPinger`.
- Threat tests T-25, T-27, T-37.

## v0.6.0 - Optimisation
Exit: NFR-1, NFR-2, NFR-3, NFR-5 met, or each gap recorded in `docs/PERFORMANCE.md` with its cause and the plan to close it before 1.0 (a missed 20% target blocks 1.0); `docs/PERFORMANCE.md` published.
- Benchmarks against hyper with hyper-util, actix-web, may-minihttp, nginx, h2o, and Pingora (restart and idle memory only).
- Idea I-3 (engine input without the copy) measured and decided; idea I-6 measured.

## v0.7.0 - Hardening
Exit: zero open findings; the 24 h soak clean (NFR-18); `docs/SECURITY.md` written.
- Every fuzz target running with an out-of-repo corpus; every threat with its attack test; loom models complete; the leak test; kill-at-every-step restart testing (idea I-9); an adversarial review with every finding fixed test-first.

## v0.8.0 - Alpha
Exit: a production site served for two weeks without a serve-net defect; `docs/ARCHITECTURE.md` draft; no API change needed by the first real wrapper that is not already made.
- A real application wrapper built on serve-net's hooks; a custom-family prototype for a database wire protocol.

## v0.9.0 - Beta and release candidates
Exit: no API change across two release candidates; every document complete.
- API freeze candidate; one non-HTTP service end to end; the Windows service status adapter (outside serve-net) exercised; `-rc` tags.

## v1.0.0 - Stable
- API frozen until 2.0; the full audit (API, docs, tests, benchmarks, security) recorded in `docs/STABILITY-1.0.md`; `dev/DIRECTIVES.md` definition of done item 10 satisfied. James decides the release.

## After 1.0
`engine::H2` (earlier as a minor version if h2-net reaches its 0.3 first), multiplexed FastCGI, systemd `FDSTORE`, lane queues with weighted deficit round robin, mod_fcgid on Windows (named pipes), cleartext HTTP/2 with prior knowledge, `Workers::uring`.

---

## Release routine

1. Finish the version: every item of its entry done; version bumped everywhere it appears (`Cargo.toml`, README install snippet, `docs/API.md`, rustdoc examples); `CHANGELOG.md` rolled; `docs/release/vX.Y.Z.md` written; README and `docs/API.md` checked against the build.
2. Local gate on Windows and WSL, stable and 1.85: fmt, clippy (default, all, no default features), tests, `cargo doc` with default and all features under `-D warnings`, `cargo package` in a fresh target directory.
3. Commit as James Gober and push a `feature/` branch; open a pull request.
4. Wait for CI on every platform; fix in place if it fails (never a follow-up patch release for a red version).
5. Squash-merge; tag `vX.Y.Z` (lightweight) and push it.
6. `gh release create vX.Y.Z --title "vX.Y.Z - <Name>" --notes-file docs/release/vX.Y.Z.md --prerelease` (0.x).
7. `cargo doc --no-deps` with default and all features: no warnings.
8. `cargo publish --dry-run`, then `cargo publish`.
9. Confirm the version on crates.io and the docs.rs build.
