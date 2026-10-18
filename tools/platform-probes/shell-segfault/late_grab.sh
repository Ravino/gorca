#!/bin/bash
# Late Grab/UngrabKeyboard regression (review F-1): an in-order fake manager (like Mutter: one main
# loop, calls handled in arrival order) blocks its main loop 4 s on the first GrabKeyboard (case
# "grab") or UngrabKeyboard (case "ungrab"), longer than GRAB_DEADLINE (3 s). Learn mode is then
# toggled; Mutter applies the timed-out call late, and gorca must still end in the wanted state.
#   tools/platform-probes/shell-segfault/late_grab.sh target/debug/examples/key_session
# Private dbus-daemon only; signals only the PIDs it started. Exit 0 = both cases pass.
set -u
BIN=${1:?usage: late_grab.sh <key_session binary>}
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=$(mktemp -d "${TMPDIR:-/tmp}/late-grab.XXXXXX")
fail=0
run_case() { # name stall_on sequence... expected_grab_all
  local name=$1 stall_on=$2 want=$3; shift 3
  dbus-daemon --session --nofork --print-address=3 3>"$OUT/$name.addr" >/dev/null 2>&1 &
  local dpid=$!; sleep 0.5
  local addr; addr=$(head -1 "$OUT/$name.addr")
  DBUS_SESSION_BUS_ADDRESS=$addr /usr/bin/python3 -I "$HERE/inorder_fake.py" 4 "$stall_on" >"$OUT/$name.fake" 2>&1 &
  local fpid=$!; sleep 1
  local fifo="$OUT/$name.in"; mkfifo "$fifo"
  "$BIN" "$addr" <"$fifo" 2>"$OUT/$name.gorca" & local gpid=$!
  exec 7>"$fifo"
  sleep 2; echo "chord 1" >&7; sleep 1
  for step in "$@"; do case $step in s*) sleep "${step#s}";; *) echo "all $step" >&7;; esac; done
  sleep 9; echo quit >&7; exec 7>&-; sleep 1
  kill "$gpid" "$fpid" "$dpid" 2>/dev/null; wait "$gpid" "$fpid" "$dpid" 2>/dev/null
  local got; got=$(grep '"ev": "state"' "$OUT/$name.fake" | tail -1 | grep -o '"grab_all": [a-z]*' | awk '{print $2}')
  if [ "$got" = "$want" ]; then echo "PASS $name grab_all=$got"; else echo "FAIL $name grab_all=$got want=$want ($OUT)"; fail=1; fi
}
run_case grab GrabKeyboard false 1 s0.5 0          # learn mode on, off while Grab is stalled
run_case ungrab UngrabKeyboard true 1 s1 0 s0.5 1  # learn mode off, on again while Ungrab is stalled
exit $fail
