# Performance

What serve-net costs, measured, and where it misses a target. The targets are `dev/PLAN.md` section 5.2
(NFR) and section 10 (P-1 to P-14). Every number here names its method and hardware. A target that is
not met says why and what closes it; a missed throughput target blocks 1.0.

Status at 0.8.0. The request-path and defence numbers are from the 0.6 pass; idle memory and the
reply path are as of 0.8.0. Numbers from a development machine compare serve-net builds with each
other. They are not the competitor comparison (NFR-5), which needs the reference hardware described at
the end.

## Method

- `cargo bench --bench core` (criterion, `benches/core.rs`), release profile, pinned to one core with
  `taskset -c 6`, 3 s warm-up, 10 s measurement, 200 samples.
- The machine is shared with other work, so the tables report the minimum and the 10th percentile of
  the per-sample means rather than criterion's mean, which moved by 10% to 40% between identical runs
  under load. A change is accepted on the minimum and the 10th percentile moving together, with the
  untouched `engine/http_net_alone_keep_alive_get` as the control.
- The request path is measured through `serve_net::testing::TestConnection`: the real connection state
  machine with a simulated transport. It includes one extra copy of the output into the test buffer, so
  the result is an upper bound for serve-net's own work.

Hardware: AMD Ryzen 9 9950X3D (16 cores, 32 threads), 62 GiB, Linux 6.6 (WSL2 on Windows 11),
rustc 1.99.0. 2026-11-04.

## Request path (P-1, NFR-1)

| Benchmark | Min | p10 |
|---|---|---|
| `request/keep_alive_get`: a keep-alive GET through serve-net and http-net, 11-byte reply | 264 ns | 268 ns |
| `engine/http_net_alone_keep_alive_get`: http-net's `ServerConn` alone, the same request and reply | 92 ns | 93 ns |
| serve-net's own work (the difference) | 172 ns | 175 ns |
| `request/keep_alive_get_in_flight_shares`: with a profile `InFlightCap` of 64 | 266 ns | 270 ns |
| `request/keep_alive_get_in_flight_shared_counter`: with a cap of 2 (one shared counter) | 257 ns | 261 ns |

**Target: at most 150 ns. Not met: 172 ns, about 15% over.** The 0.6 pass took it from about 220 ns:

| Change | Effect (min) | Security impact |
|---|---|---|
| The request path borrows the server's shared state instead of cloning its `Arc` four times, and the tables twice | together with the next row, 308 ns to 267 ns for the whole request | none: the same state, read the same way. On many cores it also removes a cache line every worker wrote per request |
| The clock read that times a service call runs in debug builds only, where its check runs | (above) | none: the check it served never ran in a release build |
| The reply keeps its deferral link between calls instead of cloning it per call | not measurable (within noise) | none: the reply is reachable only inside `call`, and a deferral still takes the link |

Tried and dropped: a smaller field array for replies with few fields (no measurable change).

What remains, and the plan: one real clock read per request for `Finished::replied_at` (about 20 ns on
this machine, kept because a cached time would make every quick reply look instant), the reply built
through `HttpReply` and re-encoded, the second view of the head for the call, the per-worker counters,
and the test transport's copy. Closing the gap needs a profile on a Linux host with `perf`, which this
machine does not have; it is in the v0.9.x performance pass (`dev/ROADMAP.md`).

## Defences (section 10.3)

| Defence | Target | Measured |
|---|---|---|
| Control checkpoint, nothing changed (P-8, NFR-11) | at most 2 ns | 0.32 ns |
| Snapshot reader, nothing changed | | 0.30 ns |
| Trusted walk, 3-entry `X-Forwarded-For` (P-9) | at most 100 ns [est.] | 137 ns (`keep_alive_get_xff_3_entries` 399 ns less `keep_alive_get_xff_header_direct` 262 ns). Not met; in the v0.9.x register |
| `InFlightCap` take and release (NFR-12) | at most 5 ns | within noise of the request without a cap |
| Feeding the engine from net-io's buffer (idea I-3) | 10-30 ns per KiB [est.] | 1.7 ns for an 80-byte head, 26 ns for 4 KiB (about 6.5 ns per KiB) |

**Idea I-3, decided:** the copy into the engine is about 1% of a keep-alive GET, so a borrowed-input
mode in http-net is not worth its API for heads. Its other claim, half the read memory per connection,
is the idle-memory question below and is decided there.

## Allocation (P-14, NFR-2)

Zero allocations per request after warm-up, enforced by `tests/alloc.rs` with a counting allocator:
10,000 keep-alive GETs, 5,000 POSTs with a 4 KiB buffered body, 2,000 batches of 8 pipelined requests,
and a refusal. Met for requests that follow each other within a second: a connection quiet for a
second gives its engine, output, body, and reply buffers back (the idle trim, D-087), so its next
request allocates them again, by design.

## Reply bodies (0.8.0)

A reply body is sent from where the service put it (`body` into the reply's buffer, `body_static` with
no copy at all), as net-io's output budget takes it; it is no longer copied whole into the connection's
output buffer. That removes one copy of every reply byte, and a slow reader holds at most the output
budget (`max_pending_output`, 256 KiB by default) instead of the whole body: eight stalled clients of
an 8 MiB static body held 64 MiB before and hold the budget now (`tests/reply_memory.rs`). Cost: one
more push into net-io per reply.

## Idle memory (P-5, NFR-3)

About 3.8 KB per idle keep-alive connection on Linux, 3.0 KB on macOS, and 4.0 KB on Windows on the
reactor (D-094, D-109), enforced by `tests/idle.rs` in CI with 10,000 connections: 4 KiB, and 4.25 KiB
on Windows, where net-io's completion-port bookkeeping adds about 150 bytes (in the register). Met on
Linux and macOS. The idle trim (D-087) releases the engine, output, body, and reply buffers after a
quiet second, on net-io 0.6.1 or later, which returns the buffer a timer callback held.

## Throughput against other servers (P-2 to P-4, NFR-5)

The target is at least 20% above hyper 1.x with hyper-util on tokio, on plaintext keep-alive,
pipelined, and connection-churn workloads. `compare/` (unpublished, its own workspace) runs both
servers in processes of their own against one closed-loop load generator on `mio`. Both answer `200`,
`content-type: text/plain`, and the 11-byte body `hello world`, keep connections alive, and time out a
slow head; the generator counts every non-`200` and every dropped connection as an error.

The runs that count (D-086): the server pinned to one set of cores and the generator to another, on
the 32-thread development host in a quiet window (CPUs 0-15 and 16-31: separate cores and separate L3
caches), and the hosted runner as a second view. A run on two machines stays open in
`docs/STABILITY-1.0.md`; it does not block 0.x.

**Hosted-runner run** (GitHub `ubuntu-latest`, AMD EPYC 9V45, 4 vCPUs: 2 for the server, 2 for
the generator; 5 rounds of 10 s, median; 2026-11-06). serve-net's per-source cap (256 connections
from one address by default, a defence hyper does not have) is lifted for the run, since every
benchmark connection comes from 127.0.0.1; no errors on either side.

| Row | serve-net | hyper | serve-net against hyper |
|---|---|---|---|
| P-2 keep-alive, 256 connections | 248,358/s | 252,690/s | -1.7% |
| P-2 keep-alive, 1,000 connections | 219,107/s | 240,785/s | -9% |
| P-3 pipelined 16, 256 connections | 1,856,846/s | 2,021,318/s | -8% |
| P-4 churn, 64 connections | 41,111/s | 40,857/s | +0.6% |

**Not met: serve-net is level with hyper to 9% behind it, against a target of 20% ahead.** The gap
grows with concurrency and pipelining, which points at per-request work on the worker rather than
the transport: the request path (172 ns of serve-net's own work against 150 ns) and the reply's
copy of a static body into the output buffer are the first two items of the pre-1.0 performance
pass. The pinned run on the development host waits for a quiet window: other work held the machine
at full load through 0.7. NFR-5 is a 1.0 blocker (`docs/STABILITY-1.0.md`).

## Not measured yet

| Target | Why | When |
|---|---|---|
| P-6, P-7, NFR-6: refusal cost and admitted p99 under 10x overload | needs the load generator above | with the competitor runs |
| Idea I-6: a damper while refusing | measured on the overload benchmark | with P-6 |
| P-12: the blocking driver against http-net's blocking server | needs a load generator | with the competitor runs |
| P-13, NFR-4: idle wake-ups | needs a wake-up counter from net-io | 0.7 soak |
| WebSocket idle memory and per-message cost (D-079) | needs a socket benchmark beside the idle test | v0.9.x pass |
