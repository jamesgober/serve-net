# serve-net - Roadmap

> Path from scaffold to a stable 1.0. Hard parts are front-loaded; each phase has hard exit criteria.
> Design and decisions: [`PLAN.md`](./PLAN.md).
>
> **Anti-deferral rule:** no listed hard task moves to a later phase unless this file records the move and the reason.

## v0.1.0 - Scaffold (DONE, 2026-10-13)
- [x] Manifest, README, CHANGELOG, REPS, dual license, CI, deny, clippy, rustfmt, DIRECTIVES, ROADMAP, PLAN.

## v0.2.0 - Design lock
Exit: Engine trait, Server builder, Request and Response, pipeline slots, core contract, and configuration written in `docs/API.md` and approved.
- [ ] Agree hook usage with net-io 0.2.0 and bouncer-io 0.2.0.
- [ ] Core contract agreed with service-kernel (O-2).

## v0.3.0 - The thin slice
Exit: a web-net app serves real pages behind Apache `ProxyPass` on the cPanel server, with the door, egress, and every pipeline slot in place.
- [ ] Listeners and workers over net-io's blocking driver.
- [ ] Per-connection engine with http-net; keep-alive; limits; total request deadline.
- [ ] Reverse-proxy ingress (TCP) with trusted forwarded headers and PROXY.
- [ ] Fixed pipeline slots with checkpoints; refusal path.
- [ ] Core: register, start, stop, drain, health.

## v0.4.0 - Every ingress mode
Exit: the same app gives identical requests through direct, proxy TCP, proxy Unix socket, FastCGI, and CGI.
- [ ] Unix socket ingress; FastCGI (fcgi-net, including descriptor 0 under mod_fcgid); CGI.
- [ ] Direct mode with tls-net; WebSocket upgrade to ws-net.

## v0.5.0 - Scale
Exit: on net-io's reactor, 10,000 idle keep-alive connections with bounded memory.
- [ ] Reactor driver integration; h2-net slot when it exists.

## v0.9.0 - Hardening
- [ ] Adversarial review; soak test; interop against Apache and nginx front ends.

## v1.0.0 - Stable
- [ ] Frozen after the first site and one non-HTTP service run on it.
