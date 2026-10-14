#!/bin/bash
# M7a end-to-end runner: gorca (daemon) + a PRIVATE Firefox + the emulated Mutter, all inside
# private sessions. Nothing here touches the user's live session.
#
#   tests/web/m7/run_m7.sh <label> [e2e options]       the M7a scenarios (B0 W1 BR1 BR2 W2 W2b W3 FM1 FM2)
#   tests/web/m7/run_m7.sh --probe <cmd...>            run <cmd> inside the same private setup
#
# Two private worlds, because the emulated Mutter (fake_mutter.py) owns
# org.freedesktop.a11y.Manager on the session bus that gorca watches, and a real compositor
# would want the same name:
#   * the AT-SPI session  (tests/harness/run-isolated.sh): session bus + a11y bus + registryd,
#     gorca, fake_mutter, the mock speech server, Firefox (as an AT-SPI application);
#   * the compositor world: its own dbus-daemon and a headless gnome-shell on a unique Wayland
#     socket, so Firefox has a display. Firefox reaches it through an absolute WAYLAND_DISPLAY.
#
# env: M7_FIREFOX (default firefox), M7_KEEP=1 keeps the compositor world's temp dir.
# Teardown signals only the PIDs recorded here (CLAUDE.md "Live-session safety"); never by name.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
cd "$ROOT" || exit 1
. ./env.sh

PROBE=0
if [ "${1:-}" = "--probe" ]; then PROBE=1; shift; fi
LABEL=${1:-m7}
[ "$PROBE" = 1 ] || shift

if [ "$PROBE" = 0 ]; then
  echo "== host: load $(cat /proc/loadavg) =="
  cargo build --release -p gorca-bench --bins -p gorca-daemon || exit 2
fi

CBASE=$(mktemp -d /tmp/gorca-m7c.XXXXXX) || exit 3
chmod 700 "$CBASE"
mkdir -m 700 "$CBASE/x"
PIDS=()
cleanup() {
  trap '' EXIT INT TERM HUP
  for ((i=${#PIDS[@]}-1; i>=0; i--)); do kill "${PIDS[i]}" 2>/dev/null; done
  sleep 1
  for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null && kill -KILL "$p" 2>/dev/null; done
  [ "${M7_KEEP:-0}" = 1 ] || rm -rf "$CBASE"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# --- the compositor world -------------------------------------------------------------------
export XDG_RUNTIME_DIR=$CBASE/x
unset WAYLAND_DISPLAY DISPLAY
out=$(env -u DBUS_SESSION_BUS_ADDRESS dbus-daemon --session --fork --print-address=1 --print-pid=1) \
  || { echo "compositor dbus-daemon failed" >&2; exit 3; }
CBUS=$(echo "$out" | sed -n 1p)
PIDS+=("$(echo "$out" | sed -n 2p)")
WL=gorca-m7-$$
env DBUS_SESSION_BUS_ADDRESS="$CBUS" GSETTINGS_BACKEND=memory XDG_SESSION_TYPE=wayland \
  gnome-shell --headless --wayland --no-x11 --wayland-display="$WL" --virtual-monitor 1280x800 \
  >"$CBASE/shell.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 100); do [ -S "$CBASE/x/$WL" ] && break; sleep 0.2; done
[ -S "$CBASE/x/$WL" ] || { echo "private gnome-shell did not come up; see $CBASE/shell.log" >&2; M7_KEEP=1; exit 4; }
sleep 3
export GORCA_M7_WAYLAND=$CBASE/x/$WL
export GORCA_M7_CBUS=$CBUS
echo "private compositor: $GORCA_M7_WAYLAND" >&2

# --- the AT-SPI world -----------------------------------------------------------------------
export GORCA_PASSTHROUGH="GORCA_M7_WAYLAND GORCA_M7_CBUS GORCA_M7_FIREFOX GORCA_E2E_SCRIPTS MOZ_ENABLE_WAYLAND"
export GORCA_M7_FIREFOX=${M7_FIREFOX:-firefox}
export MOZ_ENABLE_WAYLAND=1
export GORCA_E2E_SCRIPTS=$ROOT/tests/harness
unset XDG_RUNTIME_DIR

if [ "$PROBE" = 1 ]; then
  tests/harness/run-isolated.sh -- "$@"
  exit $?
fi
tests/harness/run-isolated.sh target/release/e2e run --label "$LABEL" \
  --reader "daemon:$ROOT/target/release/gorca,--impersonate-orca" \
  --reader-env GORCA_M7=1 "$@"
exit $?
