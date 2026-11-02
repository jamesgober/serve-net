#!/usr/bin/env bash
# The interop job: real nginx, Apache, and HAProxy in front of the `interop`
# example. Each check names what the server must have learned.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
fifo="$(mktemp -u)"
mkfifo "$fifo"

cargo run --quiet --example interop <"$fifo" &
server=$!
exec 3>"$fifo"

docker run -d --rm --name interop-nginx --network host \
    -v "$here/nginx.conf:/etc/nginx/nginx.conf:ro" nginx:stable-alpine >/dev/null
docker run -d --rm --name interop-httpd --network host \
    -v "$here/httpd.conf:/usr/local/apache2/conf/httpd.conf:ro" httpd:2.4-alpine >/dev/null
docker run -d --rm --name interop-haproxy --network host \
    -v "$here/haproxy.cfg:/usr/local/etc/haproxy/haproxy.cfg:ro" haproxy:lts-alpine >/dev/null

cleanup() {
    docker rm -f interop-nginx interop-httpd interop-haproxy >/dev/null 2>&1 || true
    exec 3>&- || true
    wait "$server" || true
    rm -f "$fifo"
}
trap cleanup EXIT

wait_for() {
    for _ in $(seq 1 100); do
        if curl -s -o /dev/null --max-time 1 "$1"; then
            return 0
        fi
        sleep 0.2
    done
    echo "$1 never answered"
    return 1
}

check() {
    local name="$1" url="$2" want="$3"
    shift 3
    wait_for "$url"
    local got
    got="$(curl -s --max-time 5 "$@" "$url")"
    if [ "$got" != "$want" ]; then
        echo "FAIL $name: got [$got], want [$want]"
        exit 1
    fi
    echo "ok   $name: $got"
}

# The proxy appends the address it saw (127.0.0.1, trusted); the walk from the
# right stops at the first untrusted entry, the client's.
client='X-Forwarded-For: 203.0.113.50'
check nginx http://127.0.0.1:8081/ "Forwarded 203.0.113.50" -H "$client"
check apache http://127.0.0.1:8082/ "Forwarded 203.0.113.50" -H "$client"

# HAProxy's PROXY header carries the connection it accepted.
check "haproxy v2" http://127.0.0.1:8083/ "ProxyProtocol 127.0.0.1"
check "haproxy v1" http://127.0.0.1:8084/ "ProxyProtocol 127.0.0.1"

# A client on the PROXY listener with no header is closed, not served.
if curl -s --max-time 5 http://127.0.0.1:9002/ >/dev/null; then
    echo "FAIL a connection without a PROXY header was served"
    exit 1
fi
echo "ok   proxy listener refused a client with no header"
