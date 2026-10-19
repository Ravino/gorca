#!/usr/bin/env bash
# drive2.sh WORKDIR : client on monitor 2, click at known stage coords, report local coords; clamp tests.
set -u
W="$1"; . "$W/env"
export DBUS_SESSION_BUS_ADDRESS="$BUS" WAYLAND_DISPLAY="$WL" XDG_RUNTIME_DIR="$RT" GDK_BACKEND=wayland GTK_A11Y=none
P="gdbus call --session -d org.gorca.MouseProbe -o /org/gorca/MouseProbe -m org.gorca.MouseProbe"
/usr/bin/python3 "$(dirname "$0")/client.py" A > "$W/A.log" 2>&1 & echo $! | tee -a "$W/pids" > "$W/A.pid"
sleep 3; $P.HideOverview >/dev/null; sleep 1
ID=$($P.Frames | grep -o 'uint64 [0-9]*' | cut -d' ' -f2)
$P.MoveFrame $ID 1400 100 >/dev/null; sleep 1
echo "monitors: $($P.Monitors)"; echo "frames: $($P.Frames)"
for xy in "1500 200" "1400.5 100.5" "1599 499" "2079 699"; do set -- $xy
  $P.MoveAbs $1 $2 >/dev/null; sleep 0.2; $P.Click 1 >/dev/null; sleep 0.2; echo "click stage $1,$2 -> pointer $($P.Pointer)"; done
echo "--- clamp"; for xy in "5000 5000" "-50 -50" "2080 100"; do set -- $xy; $P.MoveAbs $1 $2 >/dev/null; sleep 0.2; echo "move $1,$2 -> $($P.Pointer)"; done
echo "--- client log"; grep EV "$W/A.log"
kill $(cat "$W/A.pid")
