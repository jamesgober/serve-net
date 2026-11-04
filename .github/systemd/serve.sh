#!/usr/bin/env bash
# The unit's main process. systemd sends SIGHUP for a reload (Type=notify-reload)
# and SIGTERM to stop; serve-net handles no signals, so this shell turns them
# into lines on the example's stdin. The example sends every notification
# itself, which is why the unit sets NotifyAccess=all.
set -u

fifo=$(mktemp -u)
mkfifo "$fifo"
# The example pings the watchdog, not this shell: WATCHDOG_PID names us.
unset WATCHDOG_PID
"$SERVE_NET_BIN" <"$fifo" >>"${SERVE_NET_LOG:-/dev/stdout}" 2>&1 &
child=$!
exec 3>"$fifo"
rm -f "$fifo"

trapped=0
trap 'trapped=1; echo reload >&3' HUP
trap 'trapped=1; echo stop >&3' TERM

# `wait` returns early whenever a trap runs (and then has not reaped the
# child); wait again until it returns because the child ended.
while :; do
    trapped=0
    wait "$child"
    status=$?
    [ "$trapped" = 1 ] || break
done
exit "$status"
