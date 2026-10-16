#!/bin/bash
# Run a command next to a REAL gnome-terminal-server (VTE) in private sessions. Nothing here touches
# the user's live session (CLAUDE.md "Live-session safety"): two private worlds, like tests/web/m7.
#
#   tests/term/run_real_vte.sh <cmd...>
#
#   * the compositor world: its own dbus-daemon and a headless gnome-shell on a unique Wayland
#     socket, so the terminal has a display, plus (via tests/web/m7/rd.py) a RemoteDesktop session
#     that gives the headless seat a keyboard;
#   * the AT-SPI world (tests/harness/run-isolated.sh): session bus, a11y bus, registryd. <cmd> runs
#     in it with GORCA_VTE_WAYLAND (the compositor socket) and GORCA_VTE_CBUS (its bus) set.
#
# Teardown signals only the PIDs recorded here, never by name. VTE_KEEP=1 keeps the temp dir.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
cd "$ROOT" || exit 1
. ./env.sh

CBASE=$(mktemp -d /tmp/gorca-vte.XXXXXX) || exit 3
chmod 700 "$CBASE"
mkdir -m 700 "$CBASE/x"
PIDS=()
cleanup() {
  trap '' EXIT INT TERM HUP
  for ((i=${#PIDS[@]}-1; i>=0; i--)); do kill "${PIDS[i]}" 2>/dev/null; done
  sleep 1
  for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null && kill -KILL "$p" 2>/dev/null; done
  [ "${VTE_KEEP:-0}" = 1 ] || rm -rf "$CBASE"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

export XDG_RUNTIME_DIR=$CBASE/x
unset WAYLAND_DISPLAY DISPLAY
out=$(env -u DBUS_SESSION_BUS_ADDRESS dbus-daemon --session --fork --print-address=1 --print-pid=1) \
  || { echo "compositor dbus-daemon failed" >&2; exit 3; }
CBUS=$(echo "$out" | sed -n 1p)
PIDS+=("$(echo "$out" | sed -n 2p)")
WL=gorca-vte-$$
env DBUS_SESSION_BUS_ADDRESS="$CBUS" GSETTINGS_BACKEND=memory XDG_SESSION_TYPE=wayland \
  gnome-shell --headless --wayland --no-x11 --wayland-display="$WL" --virtual-monitor 1280x800 \
  >"$CBASE/shell.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 100); do [ -S "$CBASE/x/$WL" ] && break; sleep 0.2; done
[ -S "$CBASE/x/$WL" ] || { echo "private gnome-shell did not come up; see $CBASE/shell.log" >&2; VTE_KEEP=1; exit 4; }
sleep 3
export GORCA_VTE_WAYLAND=$CBASE/x/$WL
export GORCA_VTE_CBUS=$CBUS
export GORCA_VTE_TOOLS=$HERE
echo "private compositor: $GORCA_VTE_WAYLAND" >&2

export GORCA_PASSTHROUGH="GORCA_VTE_WAYLAND GORCA_VTE_CBUS GORCA_VTE_TOOLS GORCA_VTE_OUT GORCA_E2E_READER_LOG GORCA_VV_KEEP_DIR"
unset XDG_RUNTIME_DIR
tests/harness/run-isolated.sh -- "$@"
exit $?
