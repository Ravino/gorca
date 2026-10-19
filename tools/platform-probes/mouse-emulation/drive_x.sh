#!/usr/bin/env bash
# drive_x.sh WORKDIR DISPLAY : same as drive2 but the client is an X11 (XWayland) window.
set -u
W="$1"; . "$W/env"
export DBUS_SESSION_BUS_ADDRESS="$BUS" XDG_RUNTIME_DIR="$RT" GDK_BACKEND=x11 DISPLAY="$2" GTK_A11Y=none
unset WAYLAND_DISPLAY; export XAUTHORITY=$(ls "$RT"/.mutter-Xwaylandauth.* | head -1)
P="gdbus call --session -d org.gorca.MouseProbe -o /org/gorca/MouseProbe -m org.gorca.MouseProbe"
/usr/bin/python3 "$(dirname "$0")/client.py" X > "$W/X.log" 2>&1 & echo $! | tee -a "$W/pids" > "$W/X.pid"
sleep 4; $P.HideOverview >/dev/null; sleep 1
echo "frames: $($P.Frames)"
for xy in "500 300" "700 400"; do set -- $xy; $P.MoveAbs $1 $2 >/dev/null; sleep 0.2; $P.Click 1 >/dev/null; sleep 0.2; done
$P.Click 3 >/dev/null; sleep 0.3
grep -a "EV\|rror" "$W/X.log"
kill $(cat "$W/X.pid")
