#!/usr/bin/env bash
# usage: apps_a11y.sh ADDR GORCA_BIN MODE APP     PRIVATE headless shell session only.
# MODE: off = IsEnabled=false, no gorca;  gorca = the given gorca binary running (IsEnabled only)
# APP:  firefox | gtk4 | gtk3
# Prints whether the app's process holds a connection on the a11y bus.
export DBUS_SESSION_BUS_ADDRESS="$1"
BIN="$2"; MODE="$3"; APP="$4"
L=/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/ns
W=$L/sh
export XDG_CONFIG_HOME=$W/config GSETTINGS_BACKEND=keyfile XDG_RUNTIME_DIR=$W/run
export WAYLAND_DISPLAY=gorca-test-0 GDK_BACKEND=wayland MOZ_ENABLE_WAYLAND=1
set_status() { gdbus call --session -d org.a11y.Bus -o /org/a11y/bus -m org.freedesktop.DBus.Properties.Set org.a11y.Status "$1" "<$2>" >/dev/null; }
set_status ScreenReaderEnabled false; set_status IsEnabled false; sleep 0.5
GPID=""
if [ "$MODE" = gorca ]; then
  mkdir -p $L/cfg-apps
  "$BIN" --no-probe --config=$L/cfg-apps/gorca.toml --speech=ssip --ssip-socket=$L/none.sock > $L/gorca-apps.log 2>&1 &
  GPID=$!
  sleep 2
fi
echo "[$MODE/$APP] status: $(gdbus call --session -d org.a11y.Bus -o /org/a11y/bus -m org.freedesktop.DBus.Properties.GetAll org.a11y.Status)"
case "$APP" in
  firefox)
    PROF=$L/ffprof; rm -rf $PROF; mkdir -p $PROF
    setsid firefox-esr --profile $PROF --no-remote --new-instance about:blank > $L/app.log 2>&1 &
    APID=$!; WAIT=15 ;;
  gtk4)
    setsid env -u NO_AT_BRIDGE -u GTK_A11Y /usr/bin/python3 <repo>/tools/platform-probes/gtkkeys.py > $L/app.log 2>&1 &
    APID=$!; WAIT=5 ;;
  gtk3)
    setsid env -u NO_AT_BRIDGE -u GTK_A11Y /usr/bin/python3 $L/gtk3app.py > $L/app.log 2>&1 &
    APID=$!; WAIT=5 ;;
esac
sleep $WAIT
/usr/bin/python3 $L/a11y_pids.py "$APID"
# tear down only what this script started (own process group / recorded pid)
kill -TERM -- -$APID 2>/dev/null; sleep 1; kill -KILL -- -$APID 2>/dev/null
if [ -n "$GPID" ]; then kill -TERM $GPID; wait $GPID 2>/dev/null; fi
