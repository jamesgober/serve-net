# Platform notes

What differs between Linux, macOS, and Windows when running serve-net. Everything here is about
behaviour you can observe; net-io's own `docs/PLATFORM-NOTES.md` covers the transport underneath.

Each item says which version it describes, because several change when net-io's reactor arrives.

## Drivers

| | Linux | macOS | Windows |
|---|---|---|---|
| Blocking pool | yes | yes | yes |
| Reactor | epoll | kqueue | I/O completion ports |
| `Workers::auto()` | reactor | reactor | reactor |

The reactor runs one worker per logical CPU, and a connection open and idle holds no read buffer. A
service that blocks delays every connection of its worker: declare its `WorkClass` and use `Offload`,
or choose `Workers::blocking`.

The blocking pool runs one thread per connection, 256 by default, so a connection open and idle costs a
thread and its stack. Size `Workers::blocking(n)` for connections, not for CPUs.

With no `max_connections`, the soft cap is 8,750 on the reactor and the pool less its headroom on the
blocking driver: 224 of 256 threads, 6 of 8. Connections above it get the busy refusal. On the blocking
driver keep-alive is shortened and then closed as the pool fills; on the reactor it is not, because an
idle connection there costs about 9 KB of heap and no thread.

On the reactor the hard cap is 10,000 or what the process's descriptors allow, whichever is lower. Raise
`RLIMIT_NOFILE` for a server that should reach it: a stock macOS allows 256, and above a low hard cap
clients are reset rather than refused with a 503.

## Reading and waking

On the blocking driver:

- **Linux.** A blocking read waits under `SO_RCVTIMEO`, so a wake cannot interrupt it. Timers that need
  a read to end run on real time, which is why a `ManualClock` moved forward does not reach a blocked
  read there.
- **macOS and Windows.** A worker polls before it reads, so a wake interrupts the wait.
- **Windows.** Each worker's wake channel holds one loopback UDP port. A test machine running many
  servers at 256 threads can run out of ports or descriptors (error 10055); on Linux the same load shows
  as `EMFILE`. In a test harness, limit test threads or use small pools.

## Restarts

A restart hands the listening socket to the new process, and neither drops a connection.

| | Linux | macOS | Windows |
|---|---|---|---|
| `ListenerSpec::inherited` (systemd sockets) | yes | no | no |
| `ListenerSpec::handed_off` | yes | yes | yes |

- The promise that a restart refuses and resets nothing is tested on all three. It needs room on the
  new server: its own caps refuse the old server's clients like any other, so start it with the same
  caps and workers as the old one, or more. The resets once seen on small Windows machines were that.
- `Running::drain` waits about 3 ms before draining so that connections accepted as the door closes are
  served and not reset. With every accept thread dead it waits at most 150 ms.
- `Running::reload` removing a listener closes that socket at once. Connections accepted in that
  moment, or still in the kernel's queue, are lost, because no other process holds the socket. To move
  a listener without loss, add the new one, then remove the old one after clients have moved.

## Unix sockets

Linux and macOS. `UnixPeerTrust` reads the peer's credentials from the kernel, and `UnixMode` sets the
socket file's mode. net-io 0.5 has AF_UNIX and named pipes on Windows; serve-net does not offer them yet.

## Service managers

`SystemdNotify` and `WatchdogPinger` with it work on Linux only: `from_env` returns `None` elsewhere.
Without systemd, give `WatchdogPinger` your own `Notify`.

`Type=notify` and `Type=notify-reload` (systemd 253 and later) are both supported.
`WATCHDOG_PID` naming another process keeps the pinger from starting.

## Panics

`catch_service_panics` needs `panic = "unwind"`. In a `panic = "abort"` build `build` warns, and a
panicking service ends the process: run under a supervisor that restarts it.

A job on an `Offload` pool catches a panic in every `panic = "unwind"` build, so the pool keeps its
thread.
