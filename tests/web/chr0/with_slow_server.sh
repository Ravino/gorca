#!/bin/bash
# with_slow_server.sh <cmd...> - run <cmd> with the loopback slow server (port 18771) up.
#
#   tests/web/chr0/with_slow_server.sh tests/web/m9/run_m9.sh m8 chr0-chromium --only M8-SLOW,M8-SLOW0
#
# The server is started here, its PID recorded, and signalled by PID when <cmd> ends. It listens
# on 127.0.0.1 only and serves a 1x1 gif after the delay the page asks for.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
PORT=${CHR0_PORT:-18771}
/usr/bin/python3 -I "$HERE/slow_server.py" "$PORT" 2>"${CHR0_SERVER_LOG:-/dev/null}" &
SPID=$!
cleanup() { kill "$SPID" 2>/dev/null; wait "$SPID" 2>/dev/null; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
sleep 0.5
kill -0 "$SPID" 2>/dev/null || { echo "slow server did not start" >&2; exit 2; }
"$@"
