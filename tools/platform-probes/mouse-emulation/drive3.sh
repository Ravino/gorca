#!/usr/bin/env bash
# drive3.sh WORKDIR : drag (press, move, release), double click, middle click, scroll-free.
set -u
W="$1"; . "$W/env"
export DBUS_SESSION_BUS_ADDRESS="$BUS" WAYLAND_DISPLAY="$WL" XDG_RUNTIME_DIR="$RT" GDK_BACKEND=wayland GTK_A11Y=none
P="gdbus call --session -d org.gorca.MouseProbe -o /org/gorca/MouseProbe -m org.gorca.MouseProbe"
/usr/bin/python3 "$(dirname "$0")/client.py" A > "$W/A.log" 2>&1 & echo $! > "$W/A.pid"
sleep 3; $P.HideOverview >/dev/null; sleep 1
ID=$($P.Frames | grep -o 'uint64 [0-9]*' | cut -d' ' -f2)
$P.MoveFrame $ID 1400 100 >/dev/null; sleep 1
echo START >> "$W/A.log"
$P.MoveAbs 1500 250 >/dev/null; sleep 0.2
echo "-- drag"; $P.Button 1 true >/dev/null; sleep 0.1; $P.MoveAbs 1600 300 >/dev/null; sleep 0.1; $P.MoveAbs 1700 350 >/dev/null; sleep 0.1; $P.Button 1 false >/dev/null; sleep 0.3
echo "-- two clicks back to back"; $P.Click 1 >/dev/null; $P.Click 1 >/dev/null; sleep 0.3
echo "-- middle"; $P.Click 2 >/dev/null; sleep 0.2
echo "-- bad button 99 (expect warning in shell log)"; $P.Click 99; sleep 0.2
grep -a EV "$W/A.log"
kill $(cat "$W/A.pid")
