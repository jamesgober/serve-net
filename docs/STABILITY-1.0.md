# 1.0 readiness

What must be true before any `1.0.0` release candidate, and what is known to stand in the way. The
full audit (API, docs, tests, benchmarks, security) is recorded here at the v0.9.x polish exit
(`dev/ROADMAP.md`); until then this note lists the blockers as they become known.

## Blockers

| Blocker | Since | What closes it |
|---|---|---|
| Throughput against other servers is not measured (NFR-5, P-2 to P-4): at least 20% above hyper 1.x with hyper-util on tokio, plaintext keep-alive, pipelined, and connection churn | 0.6.0 (D-081, D-082) | the benchmark suite of `dev/PLAN.md` section 10.2 on reference hardware: the server and a coordinated-omission-corrected load generator on separate machines or NUMA nodes, five runs, published in `docs/PERFORMANCE.md`. A missed 20% target blocks 1.0 |
| NFR-5 on two machines: the pinned single-host run and the hosted runner are the 0.x numbers (D-086); the method of `dev/PLAN.md` section 10.2 wants the server and the generator on separate machines | 0.7.0 (D-086) | one run on two machines (or a two-socket host) before 1.0, published beside the others; open, not a 0.x blocker |
| The overload targets are not measured (P-6, P-7, NFR-6; P-12 on the blocking driver) | 0.6.0 (D-081) | the same hardware and load generator |

Every other open item is in the pre-1.0 polish register in `dev/ROADMAP.md`, which this note absorbs at
the v0.9.x exit.
