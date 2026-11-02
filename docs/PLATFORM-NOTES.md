# Platform notes

What differs between Linux, macOS, and Windows when running serve-net. Everything here is about
behaviour you can observe; net-io's own `docs/PLATFORM-NOTES.md` covers the transport underneath.

Each item says which version it describes, because several change when net-io's reactor arrives.

## Drivers

| | Linux | macOS | Windows |
|---|---|---|---|
| Blocking pool | yes | yes | yes |
| Reactor | net-io 0.5 | net-io 0.5 | net-io 0.5 |
| `Workers::auto()` today | blocking | blocking | blocking |

Every serve-net release up to this one runs on the blocking pool: one thread per connection, 256
threads by default. Until the reactor arrives, a connection open and idle costs a thread and its stack.
Size `Workers::blocking(n)` for connections, not for CPUs.

With no `max_connections`, the soft cap is the pool less its headroom: 224 of 256 threads, 6 of 8.
Connections above it get the busy refusal. Keep-alive is shortened and then closed as the pool fills.

## Reading and waking

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
| `ListenerSpec::handed_off` | yes | yes | yes, without the zero-loss promise |

- The promise that a restart refuses and resets nothing is tested on Linux and macOS.
  Windows gets it with net-io 0.5.
- `Running::drain` waits about 3 ms before draining so that connections accepted as the door closes are
  served and not reset. With every accept thread dead it waits at most 150 ms.
- `Running::reload` removing a listener closes that socket at once. Connections accepted in that
  moment, or still in the kernel's queue, are lost, because no other process holds the socket. To move
  a listener without loss, add the new one, then remove the old one after clients have moved.

## Unix sockets

Linux and macOS only. `UnixPeerTrust` reads the peer's credentials from the kernel, and `UnixMode` sets
the socket file's mode. Windows has no `ListenerSpec::unix` listener in serve-net.

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
