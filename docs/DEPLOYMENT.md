# Deploying serve-net

How to run a serve-net server behind the usual front ends, under systemd, and through a restart. Every configuration below is checked in the interop CI job (`.github/interop/`) except where marked.

## Facing clients directly

```rust,no_run
let server = serve_net::http("0.0.0.0:8080", |_req, reply| { reply.text(200, "ok"); })?;
server.wait();
# Ok::<(), serve_net::ServeError>(())
```

Every default limit applies. One address may hold 16 connections at a time on the blocking driver (net-io's per-source cap); set a gate for bans and rates.

For TLS, give the server an acceptor (`ServerBuilder::tls`) and use `Ingress::DirectTls` on the listener. tls-net provides the acceptor; any `net_io::TlsAcceptor` works.

## Behind nginx or Apache: forwarded headers

Bind loopback or a private address. A listener that trusts a front end on a public address is refused at start-up (`RuleId::PublicBind`), because anyone who reaches it could forge the client's address.

```rust,no_run
let server = serve_net::http_behind_proxy("127.0.0.1:8080", "127.0.0.1, ::1", |req, reply| {
    reply.text(200, &format!("{:?}", req.client().addr()));
})?;
server.wait();
# Ok::<(), serve_net::ServeError>(())
```

nginx:

```nginx
location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Connection "";
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
}
```

Apache (`mod_proxy_http` sends `X-Forwarded-For` by itself):

```apache
ProxyPass "/" "http://127.0.0.1:8080/"
ProxyPassReverse "/" "http://127.0.0.1:8080/"
```

The header is walked from the right: entries the trusted list covers are skipped, and the first one it does not cover is the client. Whatever a client wrote further left is never used. A trusted peer that sends no header is refused with 400; a health check from the proxy host can use `MissingHeader::UsePeer`.

To report `https` when the front end terminates TLS, either declare it (`ListenerSpec::scheme(Scheme::Https)`) or trust the proxy's word (`ProxySource::trust_scheme(true)`, with `X-Forwarded-Proto` set by the proxy). `Forwarded` (RFC 7239) and a single-address header such as `X-Real-IP` are also supported through `ProxySource::headers`.

## Behind HAProxy or a load balancer: the PROXY protocol

```rust,no_run
use serve_net::{Ingress, ListenerSpec, ProxySource, ProxyVersions, Server, TrustedProxies};
let trusted = TrustedProxies::parse("10.0.0.0/8")?;
let server = Server::builder()
    .listener(ListenerSpec::tcp("edge", "10.0.0.5:8080").ingress(Ingress::Proxy(
        ProxySource::proxy_protocol(ProxyVersions::Both, trusted),
    )))
    .serve(serve_net::handler(|_req, reply| { reply.text(200, "ok"); }))?;
# drop(server);
# Ok::<(), Box<dyn std::error::Error>>(())
```

HAProxy:

```haproxy
backend app
    server app 10.0.0.5:8080 send-proxy-v2
```

net-io reads the header before any byte reaches the engine, from trusted peers only, and closes every other connection before reading. A v2 `LOCAL` header (the proxy's own health check) keeps the socket peer and sets `ClientInfo::is_proxy_local()`. The per-source cap counts real clients here, not the proxy.

## Over a Unix socket

```rust,no_run
use serve_net::{Ingress, ListenerSpec, Server, UnixMode, UnixPeerTrust};
let server = Server::builder()
    .listener(
        ListenerSpec::unix("api", "/run/myapp/api.sock")
            .unix_mode(UnixMode { group: Some(33), ..UnixMode::new(0o660) })
            .ingress(Ingress::Unix(UnixPeerTrust::Groups(vec![33]), None)),
    )
    .serve(serve_net::handler(|_req, reply| { reply.text(200, "ok"); }))?;
# drop(server);
# Ok::<(), serve_net::ServeError>(())
```

The mode defaults to `0o660`; a mode writable by others is refused unless `allow_world_writable` gives a reason. Peers are checked against their credentials before any byte is read. With a proxy on the other end, give `Ingress::Unix` a `ProxySource`: the proxy is trusted by its credentials, and the source's list covers only further hops. Unix sockets need Linux, macOS, or another Unix for now.

## Under systemd

Socket activation hands the listening socket to the service, so the port is open while the service restarts:

```ini
# myapp.socket
[Socket]
ListenStream=127.0.0.1:8080
FileDescriptorName=web

# myapp.service
[Service]
ExecStart=/usr/local/bin/myapp
Type=notify
```

```rust,no_run
use serve_net::{ListenerSpec, Server, SystemdNotify};
let mut builder = Server::builder()
    .listener(ListenerSpec::inherited("web").fallback_bind("127.0.0.1:8080".parse()?));
if let Some(notify) = SystemdNotify::from_env() {
    builder = builder.notify(notify);
}
let server = builder.serve(serve_net::handler(|_req, reply| { reply.text(200, "ok"); }))?;
server.wait();
# Ok::<(), Box<dyn std::error::Error>>(())
```

`SystemdNotify` sends `READY=1` when every listener accepts, `STOPPING=1` when a drain starts, and `EXTEND_TIMEOUT_USEC=` for a drain over 5 s. A failure to reach the manager never stops the server; it shows in `health` and `Observer::notify_failed`. This section is not covered by the interop job yet.

serve-net handles no signals: map `SIGTERM` to `DrainHandle::drain` (and, for `Type=notify-reload`, `SIGHUP` to `Running::reload`) in your program.

## Changing configuration without a restart

Build a `ServerConfig` from your configuration and call `Running::reload`. It is all or nothing: validation runs first, added listeners bind before anything is published, and a failure leaves the server as it was. Connections see new limits and ingress at their next request head; work in flight keeps what it started with. Removed listeners close their doors and drain.

PROXY trust and TLS are fixed when a listener is bound (`RuleId::ReloadListener`): add the listener under a new name and remove the old one. `InFlightCap` budgets keep their start-up values until a restart.

## Restarting without dropping a connection

A supervisor (or systemd, through a socket unit) keeps the listening socket open across the restart:

1. The new process starts and adopts the socket (`ListenerSpec::handed_off` or `inherited`), sharing the accept queue with the old process.
2. The new process reports ready.
3. The old process calls `close_door`, then `drain(deadline)`, and exits.

Nothing is refused or reset at any step, because the socket was never closed (tested on Linux and macOS; Windows follows with net-io 0.5). net-io's `send_listeners` and `recv_listeners` move sockets between processes on Unix, with credential checks.

## Stopping

`Running::drain(deadline)`, or `DrainHandle::drain` from another thread with `wait` in `main`. The door closes, idle keep-alive connections close at once, replies in flight carry `Connection: close`, and whatever remains at the deadline is closed abortively. The `DrainReport` says what happened.
