# serve-net &mdash; Engineering Directives

> Engineering standards and the definition of done for this project. Read alongside `REPS.md` (root, authoritative), [`PLAN.md`](./PLAN.md) (design and decisions), and [`ROADMAP.md`](./ROADMAP.md) (current phase). If anything here conflicts with `REPS.md`, `REPS.md` wins, except where `PLAN.md` records an approved exception.

---

## 0. Philosophy

This library is built and maintained to a production standard and treated as a flagship piece of work. Plan the full path, then build one verified step at a time. "Good enough" is treated as a defect. It is a member of the owner's `-net` domain and part of the web and service stack (net-io, serve-net, web-net, the protocol engines, and the security crates). Every input from the network is hostile until proven otherwise.

---

## 1. What this is

serve-net is the network service engine: listeners and workers over net-io, the per-connection engine, the protocol engine slot, ingress normalization, the fixed pipeline slots with connection-gate checkpoints, responses through the egress check, drain, and the service core. It is not the web framework and does no routing. See `PLAN.md` sections 1 and 2.

---

## 2. Engineering law (non-negotiable)

- **Ours, not rented.** No third-party runtime dependency, except the exceptions recorded in `PLAN.md` (audited cryptography through `crypt-io`; rustls in `tls-net` only). First-party crates are wired when first used, at published versions (no `path` dependencies across repositories). Dev-only test tooling (`criterion`, `proptest`) is allowed.
- **Sans-IO where it is a protocol.** A protocol engine does no I/O: bytes in, events out; messages in, bytes out. Transport belongs to net-io and serve-net.
- **Strict standards.** Follow the RFCs exactly. Input that two implementations could interpret differently is refused, never repaired.
- **Unsafe.** `#![forbid(unsafe_code)]`. This crate has no reason to touch raw memory or the OS directly.
- **Hostile-input hardening.** Explicit budgets for bytes, time, and counts, each checked before the resource is committed; no reachable panics; no quadratic paths; linear-time scanning.
- **Fail closed.** When a check cannot run, the answer is no.
- **Secrets.** Compared in constant time, zeroized after use, never in logs, errors, or `Debug` output.
- **Performance.** Peak is the baseline: zero-copy views, allocation-free hot paths, no "faster" claim without `criterion` numbers at realistic scale.
- **Cross-platform.** Linux, macOS, and Windows are first-class, verified by CI on stable and MSRV 1.85.
- **Error handling.** Every fallible path returns `Result`; errors are `#[non_exhaustive]`, precise, and say what was refused.
- **Production-ready.** `#![deny(missing_docs)]`; no `unwrap`/`expect`/`todo!`/`dbg!`/printing in library code; every public item has rustdoc with a runnable example; every claim in the docs maps to a test.

---

## 3. Definition of done

1. Compiles clean on Linux/macOS/Windows, stable and MSRV 1.85.
2. `fmt`, `clippy -D warnings` (all targets, all/no-default features), `test --all-features`, `cargo doc -D warnings` clean.
3. `cargo audit` + `cargo deny check` pass.
4. No `unwrap`/`expect`/`todo!`/`dbg!` in shipping code.
5. A Tier-1 API exists and headlines the docs.
6. Standards cases as data, run whole and split at every byte; property tests for every section-4 invariant; a fuzz target for every parser or decoder; every threat in `PLAN.md` has an attack test that must fail.
7. Hot-path changes carry benchmarks; no regression over 5%.
8. `README.md`, `docs/API.md`, and `CHANGELOG.md` updated; the matching `docs/release/vX.Y.Z.md` written before the tag.
9. `PLAN.md` decision log updated for every design decision made during the work.
10. v1.0.0 only after the first site and one non-HTTP service has used the API end to end, and after an adversarial review with every finding fixed test-first.

---

## 4. Project-specific invariants

- Every accepted connection passes the connection-gate door before a byte is read.
- Pipeline slots run in their fixed order; a refusal in any slot runs no later slot.
- A connection-gate checkpoint runs between every pair of slots.
- Every response is written through the egress check.
- Every ingress mode yields an identical request for the same input.
- Proxy, FastCGI, and CGI listeners never bind to a public address without explicit override.

---

## 5. Writing style

- Plain, specific, human prose in every shipping file. No marketing voice.
- Banned in shipping files: "comprehensive", "robust", "seamless", "leverage", and the em-dash character (U+2014) in prose.
- Commits: imperative present tense, descriptive subject line.
