# serve-net - Project Plan

> The design, strategy, and decision record for `serve-net`. A living document: every design decision, idea, or open question about serve-net is written here or in [`ROADMAP.md`](./ROADMAP.md) before it is acted on.
>
> **Read order:** [`../REPS.md`](../REPS.md) &rarr; [`DIRECTIVES.md`](./DIRECTIVES.md) &rarr; **this file** &rarr; [`ROADMAP.md`](./ROADMAP.md).
>
> **Status:** scaffold, `0.1.0`. Design lock is `0.2.0`.

---

## Contents

1. [Purpose](#1-purpose)
2. [Library or running service?](#2-library-or-running-service)
3. [Where it sits](#3-where-it-sits)
4. [The protocol engine slot](#4-the-protocol-engine-slot)
5. [The connection engine](#5-the-connection-engine)
6. [Ingress modes](#6-ingress-modes)
7. [The pipeline and its fixed order](#7-the-pipeline-and-its-fixed-order)
8. [Requests and responses](#8-requests-and-responses)
9. [The core](#9-the-core)
10. [Configuration and limits](#10-configuration-and-limits)
11. [Threats](#11-threats)
12. [Architecture](#12-architecture)
13. [Ecosystem and dependencies](#13-ecosystem-and-dependencies)
14. [First run: the thin slice](#14-first-run-the-thin-slice)
15. [Testing](#15-testing)
16. [Open decisions](#16-open-decisions)
17. [Decision log](#17-decision-log)

---

## 1. Purpose

serve-net is the engine a network service runs on: the network service module. It bundles net-io (transport), bouncer-io (the connection gate), and a protocol engine, adds the per-connection engine that drives them, and gives the whole bundle a core that owns its lifecycle.

It is one engine for every kind of service. The web runs it with http-net in the protocol slot; a database runs it with its own wire format. Limits, security hooks, drain, and lifecycle are the same everywhere.

---

## 2. Library or running service?

Both, in the way a car engine is both a part and the thing that runs.

- **It is a library:** a crate the service's `main()` uses. It never owns `main()`.
- **It runs:** once started, it owns live work: listeners, worker threads, every open connection, and the loop that drives each one. It keeps running until its core is told to drain and stop.

A web service's whole `main()` is meant to look like this:

```rust
// Sketch, not compiled.
fn main() -> Result<(), Error> {
    let kernel = Kernel::new("site")?;                 // system core (service-kernel)
    let app = web_net::App::new().routes(routes());    // the site (web-net)
    let server = serve_net::Server::http(config()?)    // the engine, http-net in the slot
        .app(app)
        .register(&kernel)?;                           // its core joins the system core
    kernel.run()                                       // runs until shutdown, then drains
}
```

serve-net is more than a wrapper: the per-connection engine, ingress normalization, and the fixed pipeline order are its own code. It is less than a framework: no routing, no handlers, no templates (web-net).

---

## 3. Where it sits

```
 ┌─ serve-net (network service module) ─────────────────────────────────────────────┐
 │  core ── registers with the system core (service-kernel)                          │
 │                                                                                   │
 │  net-io ─▶ bouncer door ─▶ PROXY / TLS ─▶ [protocol engine slot] ─▶ pipeline ─▶ app
 │  net-io ◀─ bouncer egress ◀────────────── [protocol engine slot] ◀─ pipeline ◀─ app
 └───────────────────────────────────────────────────────────────────────────────────┘
```

bouncer-io is not a stage the traffic flows through. It is consulted: at accept (the door), at checkpoints the pipeline calls between stages, and before every write (egress, enforced by net-io). It holds a reference to every in-flight request's ticket, which is how it can revoke anything at any time.

---

## 4. The protocol engine slot

A protocol engine is any sans-IO state machine that implements serve-net's `Engine` trait: bytes in, protocol events out; responses in, bytes out.

| Engine | Used for | Selected by |
|---|---|---|
| `http-net` | HTTP/1.1 | default for the web |
| `ws-net` | WebSocket | after an HTTP upgrade |
| `fcgi-net` | FastCGI from Apache or nginx | listener configuration |
| CGI | one request on stdin/stdout | process start mode |
| `h2-net` (later) | HTTP/2 | ALPN after TLS |
| a database wire format | HiveDB and others | the service that runs serve-net |

The slot is what makes "protocol parser" a role rather than a library: whichever engine is in the slot is the parser for that connection.

---

## 5. The connection engine

For each connection:

1. **accept** (net-io) and the **door** check (bouncer-io): banned sources are refused before a byte is read.
2. **handshake:** PROXY header from a trusted proxy (real client address, door check again), then TLS if configured (tls-net).
3. **engine:** feed bytes to the protocol engine; collect a request head and body (streamed or buffered per route policy).
4. **pipeline:** run the fixed stage order (section 7), with a bouncer checkpoint between stages.
5. **respond:** the response is serialized by the engine into bouncer's zeroizing `Collected` buffer and written through net-io's egress check.
6. **repeat or close:** keep-alive, pipelining, upgrade to another engine, or close with a reason.

Every step runs under net-io's limits (handshake deadline, idle, read, write, minimum rate) and serve-net's own (requests per connection, total request deadline).

---

## 6. Ingress modes

Every mode produces the same normalized request. Nothing above serve-net can tell which one was used.

| Mode | How it arrives | Real client address from | First run |
|---|---|---|---|
| direct | TCP on :80/:443, TLS through tls-net | the socket | later |
| reverse proxy, TCP | Apache `ProxyPass` / nginx `proxy_pass` to `127.0.0.1:port` | PROXY protocol or forwarded headers, trusted proxies only | **yes** |
| reverse proxy, Unix socket | same, over a Unix socket | same | yes on Unix |
| FastCGI | `mod_fcgid`, `mod_proxy_fcgi`, `fastcgi_pass`; with `mod_fcgid` the web server starts our process and passes the listening socket as descriptor 0 | `REMOTE_ADDR` from the trusted front end | if hosting requires |
| CGI | the web server runs the binary once per request | `REMOTE_ADDR` | last resort |

CGI meta-variables (RFC 3875) are mapped once, in fcgi-net's `cgi` module, and used by both FastCGI and CGI.

---

## 7. The pipeline and its fixed order

The pipeline has fixed slots. Adapters fill them; nobody can reorder them, so authentication can never end up after a handler by accident.

| Slot | Filled by | Refusal |
|---|---|---|
| 1 inspect head | customs-sec adapter | 400, 413, 415 |
| 2 authenticate | gate-net adapter | 401 or continue anonymous (per route) |
| 3 identity checkpoint | bouncer-io | denial |
| 4 authorize route | gate-sec adapter | 403 |
| 5 inspect body | customs-sec adapter | 400, 413, 422 |
| 6 application | web-net (router, middleware, handlers) | handler's own |
| 7 inspect export | customs-sec adapter | 500 (blocked leak) |
| 8 egress | bouncer-io via net-io | denial |

- A bouncer checkpoint runs between every pair of slots.
- Any refusal skips straight to serialization and egress; no later slot runs.
- The HTTP adapters for slots 1, 2, 4, 5, and 7 live in web-net (they know where HTTP keeps credentials and how it phrases refusals); serve-net owns the slots and the order.

---

## 8. Requests and responses

- **Request:** method, target, headers (zero-copy views where possible), body (buffered or streamed by route policy), `ConnInfo`, real client address, ingress mode, bouncer ticket, request id.
- **Response:** status, headers, body (buffered by default so the egress check sees it whole; streaming opt-in per route).
- Errors from any slot become a response through one function, with a generic message and a correlation id; details go only to logs (shield-sec's scrubbing).

---

## 9. The core

serve-net's core is a sub-core in James's cores architecture (`_strategy/CORE-ARCHITECTURE.md`, private):

| Duty | Detail |
|---|---|
| register | with the system core (service-kernel); registers bouncer-io with the security core |
| lifecycle | start listeners and workers in order; stop in reverse; drain with a deadline |
| health | listeners up, workers alive, connection counts, refusal rates |
| security link | holds the shared security state from the security core; bouncer reads it at every checkpoint |
| errors | every error and refusal goes to the error core |

---

## 10. Configuration and limits

- Listeners (address or socket path, mode, TLS on or off), trusted proxies, worker count, net-io limits, requests per connection, total request deadline, body buffering policy, drain deadline.
- Validated at startup; an invalid or insecure configuration refuses to start (shield-sec boot checks).

---

## 11. Threats

| Threat | Defence |
|---|---|
| floods, slowloris, slow-read | net-io limits, bouncer door, total request deadline |
| spoofed client address | trusted proxies only, per mode |
| request smuggling | http-net strict parsing; serve-net never re-frames a request |
| security stage skipped or reordered | fixed slots |
| data leaving after revocation | egress check, buffered responses |
| FastCGI or proxy port exposed to the internet | listeners for those modes bind to loopback or a socket by default; startup refuses a public bind without explicit override |

---

## 12. Architecture

```
src/
  lib.rs        crate docs, Tier-1 entry points (Server::http, Server::with_engine)
  server.rs     Server builder, listeners, workers
  engine.rs     Engine trait (the protocol slot)
  conn.rs       per-connection engine
  ingress/      direct, proxy, fcgi, cgi normalization
  pipeline.rs   fixed slots, checkpoints, refusal path
  request.rs    Request, Body
  response.rs   Response, serialization into Collected
  core.rs       the service core: register, start, stop, health, security link
  config.rs     configuration and validation
  error.rs      ServeError (#[non_exhaustive])
```

---

## 13. Ecosystem and dependencies

| Crate | Relationship |
|---|---|
| `net-io` | transport, limits, hooks, drain |
| `bouncer-io` | door, checkpoints, egress |
| `http-net` | default engine |
| `ws-net`, `fcgi-net`, `tls-net`, `h2-net` | optional engines and TLS, behind features |
| `service-kernel` | system core registration (see O-2) |
| `web-net` | the application and the HTTP adapters for the pipeline slots |

---

## 14. First run: the thin slice

The smallest serve-net that puts a site live behind the cPanel server, built to final quality and extended later, never rewritten:

- blocking driver (net-io 0.3), reverse proxy over TCP, trusted forwarded headers or PROXY,
- http-net in the slot, buffered requests and responses,
- the fixed pipeline with all slots (adapters may start minimal),
- bouncer door and egress,
- the core with start, stop, drain, health.

---

## 15. Testing

- In-process tests without sockets (engine fed bytes directly), and loopback socket tests per ingress mode.
- Pipeline: a refusal in each slot skips every later slot; checkpoints run between every pair.
- Every ingress mode yields an identical request for the same input.
- Drain: in-flight requests finish; new ones are refused; deadline forces close.

---

## 16. Open decisions

| Id | Question | Recommendation |
|---|---|---|
| O-1 | Is serve-net the name for James's network service module? | yes, unless James prefers another |
| O-2 | service-kernel depends on tokio today; register through it now, or define the core contract here until the kernel is cleaned? | define a minimal core contract trait in service-kernel first, cleaned of tokio |
| O-3 | Publish `0.1.0` to crates.io to claim the name? | yes |

---

## 17. Decision log

| Id | Date | Decision | Reason |
|---|---|---|---|
| D-001 | 2026-10-13 | serve-net is a generic engine with a protocol slot, not an HTTP-only server | one infrastructure for web, databases, and other services |
| D-002 | 2026-10-13 | Fixed pipeline slots; adapters fill them | security order cannot be changed by accident |
| D-003 | 2026-10-13 | bouncer-io is consulted, not flowed through | it holds references to in-flight tickets and can revoke at any checkpoint |
| D-004 | 2026-10-13 | All ingress modes normalize to one request type | nothing above serve-net depends on deployment |
| D-005 | 2026-10-13 | serve-net has a core that joins the system core | James's cores architecture |
