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

### Added

- The 0.3 core that does not need net-io's code, on branch `feature/0.3-core`
  (not released; it waits for net-io 0.3): forwarded-header ingress with the
  right-to-left trusted walk, client normalization, configuration and its
  validation rules, the builder, the engine and family traits, the HTTP/1.1
  engine over http-net 0.6, the HTTP request and reply types, deferred
  replies, admission (`InFlightCap`), checkpoints, observation, snapshots,
  the connection state machine, and `testing` helpers that run a service
  without sockets (`respond`, `TestRequest`, `TestConnection`).
- Counting-allocator tests (zero allocations per request in steady state),
  property tests, four fuzz targets with a CI smoke job, loom models with a
  CI job, and criterion benchmarks.
- The server over net-io 0.3: `serve_net::http`, `Built::bind`,
  `bind_family`, `Bound`, `ServerBuilder::serve`, `Running` (`local_addr`,
  `close_door`, `drain`, `drain_handle`, `wait`, `server_handle`),
  `DrainHandle`, `DrainReport`, `ServeError::Bind` and `Start`; soft
  connection caps answered with 503; loopback socket tests.

### Changed

- The net-io items serve-net names (`ConnectionId`, `WorkerId`, `Lane`,
  `Cidr`, `TrustedProxies`, `ControlLine`, `ControlState`, `DriverKind`, and
  the rest) are now re-exported from net-io instead of defined here.

---

## [0.2.0] - 2026-10-23

The design lock: the public API is written down in full and the design
behind it recorded, reconciled with net-io's own 0.2 design lock. Still no
functional code; `0.3.0` is the first working server and waits for net-io
`0.3.0`.

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

[Unreleased]: https://github.com/jamesgober/serve-net/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jamesgober/serve-net/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/serve-net/releases/tag/v0.1.0
