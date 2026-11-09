# Security

How serve-net treats what it does not control, the defaults that matter, what a deployment should
check, and what serve-net leaves to others. The full threat table (T-1 to T-40, each with its defence,
its test, and its cost) and the build-time validation rules (V-1 to V-26) are in
[`../dev/PLAN.md`](../dev/PLAN.md) section 9. To report a vulnerability, see
[`../SECURITY.md`](../SECURITY.md).

## Trust boundaries

| Boundary | Untrusted side | What serve-net does |
|---|---|---|
| The network peer | every byte, every timing, every connection count | request deadlines, a minimum transfer rate, request and body limits, soft connection caps, admission, and fixed refusals. Transport deadlines and the hard cap are net-io's |
| A front end (reverse proxy, PROXY protocol sender) | trusted only when its address (or, on a Unix socket, its credentials) is on the list | believes its client address, scheme, and host only then; refuses at build to listen where an untrusted party could reach a listener that trusts forwarded facts |
| The service | trusted code that may be slow, panic, or forget work | deadlines on deferred replies, bounded queues, `done` on every path, panics caught on request (`catch_service_panics`) |
| The configuration | may be wrong | insecure or inconsistent configurations are refused at build and at reload, and a refused reload changes nothing |
| Other processes on the host | may connect to a Unix socket or the notify socket | socket mode `0o660` by default, peer credential checks, notify is send-only |

## Defaults that matter

- **Nothing forwarded is believed by default.** A direct listener reads no forwarded field at all. A
  listener behind a proxy believes `X-Forwarded-For` or `Forwarded` only from a peer on its trust list,
  and the scheme and host only with `trust_scheme` and `trust_host`, both off by default. A missing
  forwarded header from a trusted proxy is refused (`MissingHeader::Refuse`).
- **Trust lists are narrow.** An empty trust list, an entry that covers every address, and an entry
  broader than /8 (IPv4) or /32 (IPv6) are refused at build (`RuleId::TrustEmpty`, `TrustWildcard`,
  `TrustTooBroad`; a broad entry needs `allow_broad_trust` with a reason). Listeners that trust
  forwarded facts must bind a loopback or private address (`RuleId::PublicBind`).
- **IPv6 listeners are v6-only** unless asked, and IPv4-mapped addresses are reported, capped, and
  matched as their IPv4 form.
- **Limits are on.** Head and request deadlines, a minimum transfer rate, a write-stall deadline, a body
  limit, a pending-output budget, a pipelining budget per wake, and 1,000 requests per connection.
- **Refusals say nothing.** A refusal is a fixed short body for its status, with no `Server` field and
  no byte of the request in it, and it closes the connection after a lingering close.
- **Buffers are wiped.** A profile zeroes request and reply bytes when they are released
  (`wipe_buffers`, on by default), WebSocket buffers held by ws-net included (since 0.8.0).
- **WebSocket upgrades are same-origin.** A page on another site is answered 403 before the 101
  (`WebSocketTarget::origins`, since 0.8.0), so a browser's cookies cannot carry another site's page
  into the service (cross-site WebSocket hijacking).
- **TLS protocols are matched.** A connection whose ALPN result names a protocol its profile has no
  engine for is closed before any byte reaches an engine.

## A deployment checklist

1. List only the proxies that connect to you in `TrustedProxies`, and turn on `trust_scheme` and
   `trust_host` only if the proxy sets those fields and strips any the client sent.
2. Keep forwarded-header listeners on loopback, a private address, or a Unix socket, and firewall them
   so only the proxy reaches them.
3. Read the authority in `head` or `call` alike; both see the same value since 0.6.2.
4. Raise `RLIMIT_NOFILE` above the connections you expect. With no `max_connections` the caps follow
   the limit, so a low one serves fewer clients, each still refused with a 503 past the soft cap; an
   explicit `max_connections` the limit cannot hold is refused at start.
5. Offer only `http/1.1` in your TLS acceptor's ALPN list until the HTTP/2 engine exists.
6. Leave `wipe_buffers` on unless a measured cost says otherwise.
7. Give a WebSocket target an `OriginPolicy::allow` list when its pages live on another origin, and use
   `any_origin` only for an endpoint that takes no cookies or HTTP credentials.
8. Watch `Observer::connection_ended` and the transport counters for refusals, timeouts, and slow
   peers: they are the signal that a defence is working or a limit is too tight.

## What serve-net does not defend

- Floods larger than the link: filter them upstream.
- Bytes already written: a reply cannot be recalled once sent. Buffered replies (the default) let the
  egress check see the whole reply first.
- A trusted front end that lies: a trusted peer is believed by definition.
- Application abuse such as credential stuffing or scraping: that is the gate's and the application's.
- SYN floods: the operating system and net-io (backlog, SYN cookies).

## How it is tested

- **Attack tests** named for their threats: `tests/threats.rs`, and the T-numbered tests in the other
  suites.
- **Fuzz targets** (`fuzz/`), run in CI on every change with a corpus kept outside the repository:
  `forwarded_xff`, `forwarded_rfc7239`, `forwarded_proto_host`, `conn_sim`, `config`. The
  `forwarded_proto_host` oracle requires `head` and `call` to see the same request.
- **Loom models** of the shared state (`--cfg serve_net_loom`).
- **Real front ends** in CI: nginx, Apache, and HAProxy in front of serve-net.
- **A 24-hour soak** (`compare soak`): a mixed load checked every minute for memory and descriptor
  growth and for any unexpected answer.

## Advisories

| Advisory | Affected | Fixed | Summary |
|---|---|---|---|
| [GHSA-7vvv-g6cw-p8gj](https://github.com/jamesgober/serve-net/security/advisories/GHSA-7vvv-g6cw-p8gj) | 0.3.0 to 0.6.1 | 0.6.2 | with `trust_host` on, a client that was not a trusted proxy could set the authority a service saw in `call` |
