#!/usr/bin/env bash
# drive_btn.sh WORKDIR GTKMAJOR FX FY : real AT-SPI extents of a button vs the actual click landing.
set -u
W="$1"; . "$W/env"; MAJ="$2"; FX="$3"; FY="$4"
export DBUS_SESSION_BUS_ADDRESS="$BUS" XDG_RUNTIME_DIR="$RT" 
P="gdbus call --session -d org.gorca.MouseProbe -o /org/gorca/MouseProbe -m org.gorca.MouseProbe"
gdbus call --session -d org.a11y.Bus -o /org/a11y/bus -m org.freedesktop.DBus.Properties.Set org.a11y.Status IsEnabled "<true>" >/dev/null
if [ ! -f "$W/reg.pid" ]; then /usr/libexec/at-spi2-registryd >"$W/reg.log" 2>&1 < /dev/null & echo $! > "$W/reg.pid"; sleep 1.5; fi
export WAYLAND_DISPLAY="$WL" GDK_BACKEND=wayland
/usr/bin/python3 "$(dirname "$0")/client_btn.py" $MAJ > "$W/B.log" 2>&1 & echo $! > "$W/B.pid"
sleep 4; $P.HideOverview >/dev/null; sleep 1
ID=$($P.Frames | grep -o 'uint64 [0-9]*' | cut -d' ' -f2); $P.MoveFrame $ID $FX $FY >/dev/null; sleep 1.5
echo "meta frames: $($P.Frames)"
/usr/bin/python3 -I "$(dirname "$0")/extents.py" BTN$MAJ
