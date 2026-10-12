#!/usr/bin/env bash
# Runs the harness smoke tests inside a fresh isolated session, and proves isolation:
# the user's real session/a11y/system buses must show no new clients while the tests run.
# Usage: tests/harness/run-smoke.sh [unittest args, e.g. -k Hung]
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
PY=/usr/bin/python3

snapshot() { # sorted list of unique client PIDs+process names on a bus (busctl itself excluded)
  busctl "$@" --no-pager --acquired list 2>/dev/null | awk 'NR>1 && $3 != "busctl" {print $2, $3}' | sort -u
}
REAL_A11Y=""
if [ -n "${XDG_RUNTIME_DIR:-}" ] && [ -S "$XDG_RUNTIME_DIR/at-spi/bus" ]; then
  REAL_A11Y="--address=unix:path=$XDG_RUNTIME_DIR/at-spi/bus"
fi
before_s=$(snapshot --user); before_a=""; [ -n "$REAL_A11Y" ] && before_a=$(snapshot $REAL_A11Y)

rc=0
# one process per file: libatspi can abort its host process on malformed data
for f in "$HERE"/smoke/test_*.py; do
  "$HERE/run-isolated.sh" "$PY" -W ignore::ResourceWarning "$f" "$@" || rc=1
done

after_s=$(snapshot --user); after_a=""; [ -n "$REAL_A11Y" ] && after_a=$(snapshot $REAL_A11Y)
leak=0
if [ "$before_s" != "$after_s" ]; then echo "ISOLATION FAIL: real session bus clients changed"; diff <(echo "$before_s") <(echo "$after_s"); leak=1; fi
if [ "$before_a" != "$after_a" ]; then echo "ISOLATION FAIL: real a11y bus clients changed"; diff <(echo "$before_a") <(echo "$after_a"); leak=1; fi
if ls -d "${TMPDIR:-/tmp}"/gorca-h.* >/dev/null 2>&1; then echo "LEAK: temp dir left behind"; leak=1; fi
[ "$leak" = 0 ] && echo "isolation check: real buses unchanged, no leftover temp dirs"
[ $rc = 0 ] && [ $leak = 0 ]
