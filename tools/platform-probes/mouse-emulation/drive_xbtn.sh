#!/usr/bin/env bash
# drive_xbtn.sh WORKDIR GTKMAJOR DISPLAY FX FY : X11 (XWayland) client button extents on a scaled monitor.
set -u
W="$1"; . "$W/env"; MAJ="$2"; DSP="$3"; FX="$4"; FY="$5"
export DBUS_SESSION_BUS_ADDRESS="$BUS" XDG_RUNTIME_DIR="$RT"
P="gdbus call --session -d org.gorca.MouseProbe -o /org/gorca/MouseProbe -m org.gorca.MouseProbe"
gdbus call --session -d org.a11y.Bus -o /org/a11y/bus -m org.freedesktop.DBus.Properties.Set org.a11y.Status IsEnabled "<true>" >/dev/null
if [ ! -f "$W/reg.pid" ]; then /usr/libexec/at-spi2-registryd >"$W/reg.log" 2>&1 < /dev/null & echo $! > "$W/reg.pid"; sleep 1.5; fi
export GDK_BACKEND=x11 DISPLAY="$DSP" XAUTHORITY=$(ls "$RT"/.mutter-Xwaylandauth.* | head -1)
/usr/bin/python3 "$(dirname "$0")/client_btn.py" $MAJ > "$W/XB.log" 2>&1 & echo $! > "$W/XB.pid"
sleep 4; $P.HideOverview >/dev/null; sleep 1
ID=$($P.Frames | grep -o 'uint64 [0-9]*' | cut -d' ' -f2); $P.MoveFrame $ID $FX $FY >/dev/null; sleep 1.5
echo "meta frames: $($P.Frames)"
/usr/bin/python3 -I "$(dirname "$0")/extents.py" BTN$MAJ
