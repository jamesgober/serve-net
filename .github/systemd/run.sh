#!/usr/bin/env bash
# The systemd CI job: the systemd example as a transient Type=notify-reload unit
# with a watchdog, reloaded ten times under steady load. Passes when every
# request is answered 200, every `systemctl reload` returns success (systemd
# saw RELOADING=1 and READY=1), the watchdog never fires, and the stop is clean.
# Runs as a user unit where a user manager is up, otherwise as root.
set -euo pipefail

cd "$(dirname "$0")/../.."
cargo build --example systemd
bin="${CARGO_TARGET_DIR:-$PWD/target}/debug/examples/systemd"
unit="serve-net-systemd-$$"
url="http://127.0.0.1:9101/"
log=$(mktemp)
codes=$(mktemp)
chmod a+rw "$log"

# Every external call has a time limit, so a stuck step fails the job instead
# of hanging it. `sudo -n` fails rather than prompting: a password prompt
# under `timeout` stops the whole job on SIGTTIN, where no signal reaches it.
limit=(timeout -k 5 30)
# Any state at all ("running", "degraded", ...) means a user manager is up;
# only no answer means there is none.
state=$(systemctl --user is-system-running 2>/dev/null || true)
if [ "$(id -u)" != 0 ] && [ -n "$state" ] && [ "$state" != offline ]; then
    ctl=("${limit[@]}" systemctl --user)
    run=("${limit[@]}" systemd-run --user)
elif [ "$(id -u)" = 0 ]; then
    ctl=("${limit[@]}" systemctl)
    run=("${limit[@]}" systemd-run)
elif sudo -n true 2>/dev/null; then
    ctl=("${limit[@]}" sudo -n systemctl)
    run=("${limit[@]}" sudo -n systemd-run)
else
    echo "no user manager and no passwordless sudo: cannot run a unit"
    exit 1
fi

cleanup() {
    "${ctl[@]}" stop "$unit" >/dev/null 2>&1 || true
    "${ctl[@]}" reset-failed "$unit" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# systemd-run returns once the unit is active, which for this type means READY=1.
# KillMode=mixed: the stop's SIGTERM goes to the wrapper alone, which turns it
# into `stop` for the example; with the default, the example gets the SIGTERM
# too and dies before it drains.
"${run[@]}" --unit="$unit" \
    -p Type=notify-reload -p NotifyAccess=all -p KillMode=mixed \
    -p WatchdogSec=2 -p TimeoutStopSec=10 \
    --setenv=SERVE_NET_BIN="$bin" --setenv=SERVE_NET_LOG="$log" \
    /bin/bash "$PWD/.github/systemd/serve.sh"
curl -fsS --max-time 5 "$url" >/dev/null

# Steady load: ten requests per connection, one connection after another.
ten=()
for _ in $(seq 1 10); do ten+=(-o /dev/null "$url"); done
(
    for _ in $(seq 1 200); do
        curl -s --max-time 10 -w '%{http_code}\n' "${ten[@]}" || echo "curl failed: $?"
    done >"$codes"
) &
load=$!

for _ in $(seq 1 10); do
    "${ctl[@]}" reload "$unit"
    sleep 0.2
done
wait "$load"

total=$(wc -l <"$codes")
bad=$(grep -vc '^200$' "$codes" || true)
echo "requests: $total, not 200: $bad"
grep -v '^200$' "$codes" | sort | uniq -c || true
[ "$total" = 2000 ]
[ "$bad" = 0 ]

# Past two watchdog intervals: still active, so the pings kept coming.
sleep 5
"${ctl[@]}" is-active --quiet "$unit"
reloads=$(grep -c '^reloaded:' "$log" || true)
echo "reloads applied: $reloads"
[ "$reloads" = 10 ]

# A clean stop: the drain ran and the unit did not end in failure.
"${ctl[@]}" stop "$unit"
cat "$log"
grep -q '^stopped:' "$log"
if "${ctl[@]}" is-failed --quiet "$unit"; then
    echo "the unit failed on stop"
    exit 1
fi
echo "systemd: ok"
