#!/usr/bin/env bash
# Access-control test against a private headless shell. Needs systemd --user. Touches nothing in the real session.
# Expect: T1 allowed, T2 AccessDenied (right exe, wrong cgroup), T3 AccessDenied (wrong exe, right cgroup).
set -u
D="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; W="${1:-/tmp/gorca-helper-test}"
CG=/user.slice/user-$(id -u).slice/user@$(id -u).service/app.slice/gorca-shtest.service
cp /usr/bin/gjs-console "$W.gjs-copy" 2>/dev/null || { W=$(mktemp -d); cp /usr/bin/gjs-console "$W.gjs-copy"; }
("$D/headless-shell.sh" "$W" /usr/bin/gjs-console="$CG" >"$W.log" 2>&1 &); sleep 9
SP=$(pgrep -x gnome-shell | while read p; do tr '\0' '\n' </proc/$p/environ | grep -q "$W/run" && echo $p; done | head -1)
A=$(tr '\0' '\n' </proc/$SP/environ | sed -n 's/^DBUS_SESSION_BUS_ADDRESS=//p')
run(){ systemd-run --user --quiet --wait --pipe --collect --unit=gorca-shtest.service --setenv=DBUS_SESSION_BUS_ADDRESS="$A" "$@" </dev/null 2>&1; }
echo "T1 allowed:"; run /usr/bin/gjs-console "$D/probe-client.js" ping
echo "T2 denied (wrong cgroup):"; DBUS_SESSION_BUS_ADDRESS="$A" gjs "$D/probe-client.js" ping
echo "T3 denied (wrong exe):"; run "$W.gjs-copy" "$D/probe-client.js" ping
kill "$SP"; rm -f "$W.gjs-copy"
