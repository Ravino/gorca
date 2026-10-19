#!/usr/bin/env bash
# drive.sh WORKDIR : start 2 clients, hide overview, click in them via the probe, print results.
set -u
W="$1"; . "$W/env"
export DBUS_SESSION_BUS_ADDRESS="$BUS" WAYLAND_DISPLAY="$WL" XDG_RUNTIME_DIR="$RT" GDK_BACKEND=wayland GTK_A11Y=none
P="gdbus call --session -d org.gorca.MouseProbe -o /org/gorca/MouseProbe -m org.gorca.MouseProbe"
/usr/bin/python3 "$(dirname "$0")/client.py" A > "$W/A.log" 2>&1 & echo $! > "$W/A.pid"
sleep 3; $P.HideOverview; sleep 1
echo "frames: $($P.Frames)"; echo "monitors: $($P.Monitors)"
