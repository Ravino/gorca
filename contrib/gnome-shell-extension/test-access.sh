#!/usr/bin/env bash
# Access-control test (v1 identity rules) against a PRIVATE headless shell. Touches nothing in the real session: no
# systemd, no service manager, no process lookup by name; caller identities are private copies of the python binary and
# teardown kills only recorded PIDs (tools/platform-probes/mouse-emulation/teardown.sh).
# Expect: T1 allowed, T3 denied (wrong exe), T2 denied (right exe, wrong cgroup in the rule), T4 allowed after the owner
# fixes the rule (config stamp change picked up without reconnecting).
set -u
D="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; W="${1:-${TMPDIR:-/tmp}/gorca-access-$$}"
TD="$D/../../tools/platform-probes/mouse-emulation/teardown.sh"
trap '"$TD" "$W"' EXIT
setsid "$D/test-rig.sh" "$W" >/dev/null 2>&1 < /dev/null &
for _ in $(seq 1 60); do sleep 1; grep -q "v2 owns" "$W/shell.log" 2>/dev/null && break; done
grep -q "v2 owns" "$W/shell.log" || { echo "rig did not start (see $W/shell.log)"; exit 2; }
P="$D/testclients/access_probe.py"
CFG="$W/config/gorca/shell-helper.json"
CGP="$(sed -n 's/^0:://p' /proc/self/cgroup)"
setrule() { printf '{"allowed":[{"exe":"%s/py-a","cgroup":"%s"}]}\n' "$W" "$1" > "$CFG.new"; chmod 600 "$CFG.new"; mv "$CFG.new" "$CFG"; }
echo "T1 allowed:";                         "$D/run-as.sh" "$W" a "$P"
echo "T3 denied (wrong exe):";              "$D/run-as.sh" "$W" c "$P"
sleep 1.1; setrule "$CGP/not-the-callers-cgroup"
echo "T2 denied (right exe, wrong cgroup):"; "$D/run-as.sh" "$W" a "$P"
sleep 1.1; setrule "$CGP"
echo "T4 allowed again (rule fixed):";      "$D/run-as.sh" "$W" a "$P"
