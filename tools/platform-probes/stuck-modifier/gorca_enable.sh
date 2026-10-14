#!/usr/bin/env bash
# usage: gorca_enable.sh ADDR GORCA_BIN [PRESET]   PRIVATE headless shell session only.
# Starts gorca in the private session, shows org.a11y.Status + the private gsettings keyfile
# before / while running / after a SIGTERM of the recorded PID.
# PRESET: "plain" (default), "orca" (ScreenReaderEnabled already true: a screen reader runs)
export DBUS_SESSION_BUS_ADDRESS="$1"
BIN="$2"
PRESET="${3:-plain}"
L=/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/ns
W=$L/sh
export XDG_CONFIG_HOME=$W/config GSETTINGS_BACKEND=keyfile XDG_RUNTIME_DIR=$W/run
show() {
  echo "  gsettings screen-reader-enabled=$(gsettings get org.gnome.desktop.a11y.applications screen-reader-enabled) toolkit-accessibility=$(gsettings get org.gnome.desktop.interface toolkit-accessibility)"
  echo "  bus: $(gdbus call --session -d org.a11y.Bus -o /org/a11y/bus -m org.freedesktop.DBus.Properties.GetAll org.a11y.Status)"
}
set_status() { gdbus call --session -d org.a11y.Bus -o /org/a11y/bus -m org.freedesktop.DBus.Properties.Set org.a11y.Status "$1" "<$2>" >/dev/null; }
set_status ScreenReaderEnabled false; set_status IsEnabled false; sleep 0.5
if [ "$PRESET" = orca ]; then set_status ScreenReaderEnabled true; fi
echo "[$PRESET] before gorca"; show
mkdir -p $L/cfg-$PRESET
"$BIN" --no-probe --config=$L/cfg-$PRESET/gorca.toml --speech=ssip --ssip-socket=$L/none.sock > $L/gorca-$PRESET.log 2>&1 &
PID=$!
echo $PID > $L/gorca-$PRESET.pid
sleep 3
echo "[$PRESET] gorca running (pid $PID)"; show
if [ "$PRESET" = orca ]; then
  :
fi
kill -TERM $PID
wait $PID 2>/dev/null
sleep 1
echo "[$PRESET] after gorca stopped"; show
grep -iE "enable|org.a11y.Status|restore" $L/gorca-$PRESET.log | head -5
