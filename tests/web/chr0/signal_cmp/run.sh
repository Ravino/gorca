#!/bin/bash
# run.sh READER APP SCENARIO OUTDIR [QUERY] - one Orca / gorca run on the Signal-like page.
#   READER   orca | gorca | none
#   APP      chromium | chrome | electron   (electron: SC_ELECTRON_DIR, default ~/.cache/electron-probe,
#                                            binary dist2/$SC_APPNAME, default signal-desktop)
#   SCENARIO list | chat   (tests/web/chr0/signal_cmp/driver.py)
#   QUERY    page query, default ?focus=timeline for chat and ?focus=grid for list
# Private worlds as tests/web/m7/run_m7.sh: a headless gnome-shell on a unique Wayland socket and an
# isolated AT-SPI session (run-isolated.sh). Teardown signals only recorded PIDs / process groups.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
READER=${1:?reader}; APP=${2:?app}; SCEN=${3:?scenario}; OUT=${4:?outdir}
case "$SCEN" in list|list_focus) Q='?focus=grid' ;; *) Q='?focus=timeline' ;; esac
Q=${5:-$Q}
mkdir -p "$OUT"; OUT=$(cd "$OUT" && pwd)
CBASE=$(mktemp -d /tmp/gorca-sc.XXXXXX) || exit 3
chmod 700 "$CBASE"; mkdir -m 700 "$CBASE/x"
PIDS=()
cleanup() {
  trap '' EXIT INT TERM HUP
  for ((i=${#PIDS[@]}-1; i>=0; i--)); do kill "${PIDS[i]}" 2>/dev/null; done
  sleep 1
  for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null && kill -KILL "$p" 2>/dev/null; done
  rm -rf "$CBASE"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

export XDG_RUNTIME_DIR=$CBASE/x
unset WAYLAND_DISPLAY DISPLAY
out=$(env -u DBUS_SESSION_BUS_ADDRESS dbus-daemon --session --fork --print-address=1 --print-pid=1) || { echo "compositor dbus-daemon failed" >&2; exit 3; }
CBUS=$(echo "$out" | sed -n 1p)
PIDS+=("$(echo "$out" | sed -n 2p)")
WL=gorca-sc-$$
env DBUS_SESSION_BUS_ADDRESS="$CBUS" GSETTINGS_BACKEND=memory XDG_SESSION_TYPE=wayland \
  gnome-shell --headless --wayland --no-x11 --wayland-display="$WL" --virtual-monitor 1280x800 \
  >"$CBASE/shell.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 100); do [ -S "$CBASE/x/$WL" ] && break; sleep 0.2; done
[ -S "$CBASE/x/$WL" ] || { echo "private gnome-shell did not come up; see $CBASE/shell.log" >&2; exit 4; }
sleep 3
export GORCA_M7_WAYLAND=$CBASE/x/$WL
export GORCA_M7_CBUS=$CBUS
REAL_HOME=$(getent passwd "$(id -u)" | cut -d: -f6)
export SC_READER=$READER SC_APP=$APP SC_SCENARIO=$SCEN SC_OUT=$OUT SC_QUERY=$Q SC_ROOT=$ROOT
export SC_PAGE=${SC_PAGE:-$ROOT/tests/web/chr0/pages/signal-like.html}
export SC_ELECTRON_DIR=${SC_ELECTRON_DIR:-$REAL_HOME/.cache/electron-probe}
export GORCA_PASSTHROUGH="GORCA_M7_WAYLAND GORCA_M7_CBUS SC_READER SC_APP SC_SCENARIO SC_OUT SC_QUERY SC_ROOT SC_PAGE SC_ELECTRON_DIR SC_APPNAME SC_GORCA_BIN SC_GORCA_ARGS SC_RUST_LOG SC_SETTLE SC_FORCE_A11Y SC_BROWSER_FIRST"
unset XDG_RUNTIME_DIR
[ "$READER" = gorca ] && export GORCA_HARNESS_SCREEN_READER=0
cd "$ROOT" || exit 1
"$ROOT/tests/harness/run-isolated.sh" -- /usr/bin/python3 "$HERE/driver.py"
exit $?
