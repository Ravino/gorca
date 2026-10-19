#!/usr/bin/env bash
# test-pointer.sh [WORKDIR] [--slow] : helper v2 pointer tests in a PRIVATE headless gnome-shell (own bus, own Wayland
# display, own runtime dir). Nothing touches the live
# session; teardown kills only PIDs recorded by the rig (tools/platform-probes/mouse-emulation/teardown.sh).
set -u
D="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
W="${1:-${TMPDIR:-/tmp}/gorca-ptest-$$}"; shift 2>/dev/null
SLOW="${1:-}"
TD="$D/../../tools/platform-probes/mouse-emulation/teardown.sh"
trap '"$TD" "$W"' EXIT
setsid "$D/test-rig.sh" "$W" "1280x800 1200x900" >/dev/null 2>&1 < /dev/null &
for _ in $(seq 1 60); do sleep 1; grep -q "v2 owns" "$W/shell.log" 2>/dev/null && break; done
grep -q "v2 owns" "$W/shell.log" || { echo "rig did not start (see $W/shell.log)"; exit 2; }
rc=0
"$D/run-as.sh" "$W" a "$D/testclients/test_pointer.py" $SLOW || rc=1
"$D/run-as.sh" "$W" c "$D/testclients/test_roles.py" c || rc=1
"$D/run-as.sh" "$W" c "$D/testclients/test_flood.py" flood > "$W/flood.out" 2>&1 &
FP=$!
"$D/run-as.sh" "$W" a "$D/testclients/test_flood.py" victim || rc=1
wait "$FP"; cat "$W/flood.out"
"$D/run-as.sh" "$W" b "$D/testclients/test_roles.py" b || rc=1
echo "test-pointer.sh exit $rc (workdir $W)"
exit $rc
