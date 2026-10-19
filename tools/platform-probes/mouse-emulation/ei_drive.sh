#!/usr/bin/env bash
W="$1"; . "$W/env"; EI="$2"
export DBUS_SESSION_BUS_ADDRESS="$BUS" WAYLAND_DISPLAY="$WL" XDG_RUNTIME_DIR="$RT" GDK_BACKEND=wayland GTK_A11Y=none
P="gdbus call --session -d org.gorca.MouseProbe -o /org/gorca/MouseProbe -m org.gorca.MouseProbe"
/usr/bin/python3 "$(dirname "$0")/client.py" A > "$W/A.log" 2>&1 & echo $! | tee -a "$W/pids" > "$W/A.pid"
sleep 3; $P.HideOverview >/dev/null; sleep 1
ID=$($P.Frames | grep -o 'uint64 [0-9]*' | cut -d' ' -f2); $P.MoveFrame $ID 1400 100 >/dev/null; sleep 1
echo START >> "$W/A.log"
"$EI" "$BUS" 1500 200; echo "pointer now $($P.Pointer)"
"$EI" "$BUS" 400 300; echo "pointer now $($P.Pointer)"
grep -a EV "$W/A.log"; kill $(cat "$W/A.pid")
