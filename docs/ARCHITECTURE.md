# serve-net architecture

Draft for 0.8.0. This describes the code as it stands; `dev/PLAN.md` holds the design intent and the decision log (D-entries), and `docs/API.md` the public contract. Where they disagree, `docs/API.md` is the contract and this file is out of date.

## What serve-net is

A server runtime between a transport and a protocol. Below it, net-io owns sockets, worker threads, the accept loop, the door, transport deadlines, hard caps, the connection gate, and drain mechanics. Above it, a service answers requests. serve-net owns everything in between: which protocol engine a connection runs, request deadlines, soft caps with a clean busy refusal, admission, the order in which hooks are called, deferred and offloaded replies, ingress normalization (who the client really is), reload, and the boot and shutdown order.

serve-net never does I/O itself. It is a set of sans-IO state machines that net-io drives through its `Session` trait, and protocol engines that serve-net drives the same way.

```
 owner: configuration, hooks, the control line, a Gate, an EventSink, Notify
   |
   v
 serve-net  Server::builder -> Built -> Bound -> Running
   |   per connection: ServeSession (net_io::Session) -> Conn (the machine) -> F::Engine
   |
   +--> engines (sans-IO): http-net (HTTP/1.1), ws-net (WebSocket), a custom family's engine
   v
 net-io     listeners, workers (blocking pool or reactor), door, deadlines, caps, gate, drain
```

Dependencies point one way. serve-net never names a connection gate, an authorization system, or a TLS library: the gate reaches every connection through net-io's `Gate` seam and every request through serve-net's `Checkpoint` hook, and TLS through `net_io::TlsAcceptor`.

## Lifecycle

| Stage | Type | What exists |
|---|---|---|
| configure | `ServerBuilder` | listeners, profiles, limits, workers, hooks; nothing validated yet |
| `build()` | `Built` | every rule in `config/validate.rs` has run and every error is collected (`ConfigErrors`); nothing bound, no thread |
| `bind(service)` / `bind_family(family, service)` | `Bound` | the family is checked against each listener's profile (`RuleId::EngineFamily`), sockets bound, inherited, or handed off, workers started with the door closed. Privileges can be dropped here |
| `open()` | `Running` | the door opens; `Notify::ready` once every listener accepts |
| `close_door()`, `drain(deadline)`, `wait()` | `DrainReport` | accepting stops, in-flight work finishes, the rest is killed at the deadline |

The service is given at `bind` because net-io needs the session factory when it builds its server (D-008). `src/server.rs` holds the builder and `bind_family`; `src/running.rs` the bound and running server.

## Modules

| Module | Role |
|---|---|
| `server.rs` | `ServerBuilder`, `Built`, `bind_family`: translates serve-net's listeners and profiles into net-io's, sets the hard caps (`connection_caps`), builds the shared tables |
| `running.rs` | `Bound`, `Running`, `DrainHandle`, `DrainReport`; the merged control line (owner's line plus serve-net's own drain state, D-032) |
| `reload.rs` | `Running::reload`: validate everything, diff listeners by name, bind new ones with the door closed, then publish new tables |
| `config/` | listeners, ingress, profiles, limits, workers, and `validate.rs` (one function per rule) |
| `conn/mod.rs` | `Conn`, the per-connection state machine, written against a small `Io` trait instead of net-io so tests drive it with scripted input and a manual clock |
| `conn/session.rs` | `ServeSession` and `Factory`: the machine as a net-io `Session`, and the soft connection caps checked at open |
| `conn/upgraded.rs` | the machine after a protocol switch (WebSocket) |
| `engine/` | the `Engine` and `Family` traits, `OutBuf`, refusals, and the HTTP/1.1 engine over http-net |
| `http.rs` | the HTTP family: `HttpHead`, `HttpRequest`, `HttpReply`, `HttpService`, and the forwarded-header resolution at the head |
| `forwarded.rs` | sans-IO parsers for `Forwarded`, `X-Forwarded-For`, a single-address field, `X-Forwarded-Proto` and `-Host`, and the right-to-left trusted walk |
| `client.rs`, `envelope.rs` | `ClientInfo` (the real client, whichever way it arrived) and the `Envelope` of facts kept per request |
| `hooks/` | `Admit` (`InFlightCap`), `Dispatch` (`Inline`, `Offload`), `Checkpoint`, `Observer`, and the `Hooks` set |
| `reply.rs` | `Reply<F>`, `ReplyToken`, and the per-connection deferral slot |
| `ws.rs` | WebSocket targets, services, and the outbox for sends from other threads (feature `ws`) |
| `snapshot.rs` | `Snapshot<T>`: tables swapped live by reload, read per worker |
| `stats.rs`, `health.rs`, `notify.rs` | per-worker counters, health computed on call, and readiness messages to a supervisor or systemd |
| `testing.rs` | call a service with raw request bytes, no sockets (feature `http1`) |

## Threads

serve-net starts no threads of its own. net-io's driver runs every session:

- **Reactor** (the default where net-io has one): one worker per core, each owning its connections from accept to close. A connection's session is only ever called on its worker, so per-worker state (admission counters, stats, snapshot readers) is written with plain relaxed loads and stores, never a contended atomic.
- **Blocking pool**: one thread per open connection. The pool size bounds connections, which is why the soft cap sits under the pool and keep-alive shortens under pressure (PLAN section 8.10).

Work that leaves the worker goes through two seams only: a `ReplyToken` completed from any thread (it posts a wake through net-io's per-worker completer), and `Offload`, which runs the service on a bounded pool the owner provides. Both come back to the owning worker before any byte is written.

## One request

The path through `Conn` (`src/conn/mod.rs`), for any family:

1. **Open.** `Factory::open` checks the soft caps (global, listener, profile). Over a cap, the engine writes the busy refusal (`503` for HTTP) before anything is read, and the connection closes with a linger so the client reads it instead of a reset. net-io's hard cap sits above with a headroom; past it net-io resets without calling serve-net.
2. **Feed.** net-io hands its read buffer; `Conn::process` feeds the engine and polls it. The head deadline runs from accept for the first request and from the first byte for later ones.
3. **Head.** On `EngineEvent::Head` (`Conn::on_head`): the control line is read (one load when its epoch has not moved), the family builds the head view and the real client (`Family::head`: for HTTP this is the trusted forwarded walk, and on a TLS connection the check that `Host` names the TLS server name, answered 421 otherwise), then `Service::head` chooses the body policy, refuses, or asks for an upgrade. Then, in this order: the control line's shed and lockdown, `Admit::admit`, `Checkpoint` at `Admitted`, and the declared body length against `max_body`.
4. **Body.** Buffered up to `max_body`, discarded with a bound, or streamed to the service piece by piece, under the request deadline and net-io's minimum rate.
5. **Dispatch.** On `BodyEnd`: `Checkpoint` at `BeforeDispatch`, then `Dispatch` runs `Service::call` inline or offloaded.
6. **Reply.** `Outcome::Done`: `Checkpoint` at `BeforeReply`, the family encodes the reply's head into the `OutBuf`, and serve-net hands net-io the head and then the body straight from the reply data (`Family::reply_body`), as net-io's output budget takes them; a slow reader holds at most that budget, never a copy of the body. `Outcome::Deferred`: the connection parks under the reply deadline until the token completes or is dropped.
7. **Done.** After the last byte is handed over, `Service::done` runs once, `Admit::release` returns the slot, and the machine goes back to step 2 with any pipelined input, or idles under the keep-alive timeout.

Rules that hold on every path (PLAN section 8.3):

- No byte past the head is processed before admission, so a slow head never holds an admission slot.
- `Service::call` never runs for a request that failed ingress, was refused at `head`, was not admitted, or was denied.
- A refusal skips every later step and goes straight to the engine's refusal bytes and the close signal.
- `Service::done` runs exactly once for every request that reached `head`, whether it was answered, refused, timed out, drained, or killed.
- A reply is complete before its first byte goes to net-io, so a checkpoint denial never splits one and the gate's egress check sees its start; only a streamed reply leaves part by part.

## States

| State | Meaning | Deadline |
|---|---|---|
| `AwaitHead` | before the first request or between requests | head deadline, or keep-alive idle |
| `ReadBody` | admitted, reading the body | request deadline, minimum rate |
| `Streaming` | a streamed reply's head left; the service is called per part | write stall, minimum rate |
| `Parked` | waiting for a deferred reply | reply deadline |
| `Writing` | the reply is being handed over; then the next request | write stall |
| `Closing` | a closing reply or refusal is being handed over | write stall, then linger |
| `Upgraded` | the connection switched protocols | the target's budgets |
| `Closed` | finished | |

There is at most one serve-net deadline per connection at a time (the exchange or park deadline), on net-io's timing wheel, so timer memory is bounded by the connection cap.

## Families and engines

A family (`engine::Family`) is a set of engines that produce the same request view: that is what makes "one request, whichever way it arrived" a type fact. The HTTP family is built in; a custom family brings its own engine, head, request, and reply types and is bound with `bind_family`. `tests/custom_family.rs` is a complete one built from the public API alone: a key-value line protocol with declared value lengths, pipelining, and refusals in its own terms (D-110).

An engine (`engine::Engine`) is sans-IO: bytes in through `feed`, events out through `poll`, replies encoded into an `OutBuf`, refusals written by the engine in the protocol's terms. The connection machine never parses a protocol. One server serves one family; each listener's profile must name an engine of that family (`EngineChoice`), checked at bind and reload.

Dispatch is static: `Conn` and `ServeSession` are generic over the family, the service, and the hook set, so the hot path has no trait objects. Hooks left at their defaults (`NoObserver`, `NoCheckpoint`) compile to nothing.

## Shared state

| What | Where | How it changes |
|---|---|---|
| profiles, listener runtime facts, profile caps | `Tables` in a `Snapshot` | reload publishes a new one; each worker's reader picks it up at its next head |
| the control line | owner's `ControlLine`, merged with serve-net's drain state | one epoch load per checkpoint; the state is read only when the epoch moved |
| soft connection caps | `Cap` counters, one per profile and listener plus a global one | one add at open, one subtract at close |
| admission | `InFlightCap`, per worker per profile | plain stores on the owning worker |
| request counters | `Counters`, per worker, cache-line padded | summed on `Running::stats` |
| a deferred reply | `DeferSlot` per connection that ever deferred, reused | a mutex taken once per completion |

The loom models in `src/loom_tests.rs` cover the snapshot, the deferral slot, the in-flight cap, two concurrent drains, and a drain against an upgraded connection closing.

## Memory

An idle keep-alive connection on the reactor holds only its connection record and the transport's bookkeeping: the engine releases its buffer (`Engine::shrink_idle`), and the reply, body, and output buffers go back after a quiet period (D-087). Measured with 10,000 idle connections (`tests/idle.rs`): about 3,800 bytes each on Linux, 3,000 on macOS, and 4,000 on Windows (D-094, D-109). Under load every buffer is reused across requests, so a keep-alive request allocates nothing in steady state (`tests/alloc.rs`), and the heap stays flat over 100,000 connections (NFR-19).

A profile that wipes buffers (the default) zeroes the used part of every serve-net buffer before reuse, turns on http-net's request wipe, and zeroes ws-net's buffers when an upgraded connection ends. A custom engine reads `Profile::wipes_buffers` to do the same with the bytes it holds.

## Configuration and reload

Every rule runs at `build` and again at `reload`, and every error is reported at once with the rule's name and a hint (`docs/API.md`, rule table). A reload validates the whole next configuration first, so a refused reload changes nothing. Listeners are matched by name: the same socket keeps its connections and takes the new profile, limits, and trust lists through the snapshot; a changed address is a removal plus an addition, bound with the door closed before anything is published. The workers, PROXY trust, and TLS of a kept listener are fixed for the server's life (`ReloadWorkers`, `ReloadListener`).

## Drain and restart

`drain(deadline)` sets serve-net's half of the merged control line to `Draining`: net-io closes the door, closes idle connections at once, and kills what remains at the deadline; serve-net sets the close signal on every reply still in progress and sends a WebSocket `1001` to open sockets. `drain` waits at most 5 s past its deadline for net-io's shutdown; a worker that never comes back leaves `DrainReport::stuck` set. A restart without dropping connections hands the listening socket to the new process (descriptor passing or systemd socket activation) and drains the old one; `tests/restart.rs` kills the old process at four points under load and checks that nothing begun after the kill is lost (I-9, D-095).

## Where to read next

- `docs/API.md`: every public item, its default, and its cost.
- `docs/SECURITY.md`: the security model, the defaults, and what serve-net does not defend.
- `docs/PERFORMANCE.md`: measured costs and the comparison with hyper.
- `docs/DEPLOYMENT.md`: running behind a front end, under systemd, and restarts.
- `dev/PLAN.md` sections 8 and 9: the full design and threat model.
