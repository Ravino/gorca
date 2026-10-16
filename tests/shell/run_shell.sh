#!/bin/bash
# run_shell.sh - run <cmd...> inside ONE private world in which a real headless GNOME Shell 48
# is an AT-SPI application on a private a11y bus (M4b ground-truth rig).
#
#   tests/shell/run_shell.sh [--no-unsafe] [--keep] [--] <cmd...>
#   tests/shell/run_shell.sh -- /usr/bin/python3 tests/shell/probe_shell.py --out DIR
#
# World = tests/harness/run-isolated.sh (private session bus, private a11y bus, registryd; all
# under a fresh /tmp/gorca-h.* dir) + a headless `gnome-shell --wayland --headless --no-x11
# --unsafe-mode` started inside it. Why the Shell shows up on the a11y bus with no extra step:
# run-isolated sets org.a11y.Status IsEnabled/ScreenReaderEnabled=true on the private bus
# BEFORE the Shell starts; Shell/Clutter's cally + atk-bridge reads that at init and registers.
#
# Differences from tests/web/m7/run_m7.sh (which has a separate "compositor world"): here the
# Shell lives in the SAME session bus as the AT-SPI services, so the Shell itself owns
# org.freedesktop.a11y.Manager and org.freedesktop.Notifications on that bus.
# !! DO NOT start tests/harness/fake_mutter.py in this world: it also wants
# !! org.freedesktop.a11y.Manager (and would only fight the real Mutter).
#
# The Shell is given the HOST system bus (read-only use, as run_m7.sh does): without a
# reachable system bus the Shell dies at startup (loginManager.js: "Could not connect: No such
# file or directory", then SIGSEGV). Nothing writes to it.
#
# Env exported to <cmd> (also printed on stderr as KEY=VALUE lines before it starts):
#   DBUS_SESSION_BUS_ADDRESS, AT_SPI_BUS_ADDRESS (= GORCA_A11Y_BUS_ADDRESS), XDG_RUNTIME_DIR,
#   WAYLAND_DISPLAY (name; socket is $XDG_RUNTIME_DIR/$WAYLAND_DISPLAY), GORCA_SHELL_PID,
#   GORCA_SHELL_LOG, GORCA_M7_CBUS (= session bus; for tests/web/m7/rd.py), GORCA_HARNESS_DIR.
# Teardown: the inner script signals only the Shell PID it recorded; run-isolated.sh then
# kills its own process groups and sweeps /proc for processes whose environ mentions its dir.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)

if [ "${1:-}" = "__inner" ]; then
  shift
  UNSAFE=$1; shift
  export DBUS_SYSTEM_BUS_ADDRESS=unix:path=/run/dbus/system_bus_socket
  WL=gorca-shell-$$
  LOG=$GORCA_HARNESS_DIR/log/shell.log
  args=(--headless --wayland --no-x11 --wayland-display="$WL" --virtual-monitor 1280x800)
  [ "$UNSAFE" = 1 ] && args+=(--unsafe-mode)
  gnome-shell "${args[@]}" >"$LOG" 2>&1 &
  SP=$!
  inner_cleanup() { trap '' EXIT INT TERM HUP; kill -TERM "$SP" ${NP:-} 2>/dev/null; sleep 1; kill -KILL "$SP" ${NP:-} 2>/dev/null; }
  trap inner_cleanup EXIT
  trap 'exit 143' TERM
  trap 'exit 130' INT
  for _ in $(seq 300); do [ -S "$XDG_RUNTIME_DIR/$WL" ] && break; sleep 0.2; done
  [ -S "$XDG_RUNTIME_DIR/$WL" ] || { echo "run_shell: gnome-shell did not create its socket; see $LOG" >&2; tail -20 "$LOG" >&2; exit 4; }
  # wait until the Shell is on the a11y bus (registered with the registry)
  ok=0
  for _ in $(seq 300); do
    kill -0 "$SP" 2>/dev/null || { echo "run_shell: gnome-shell died; see $LOG" >&2; tail -20 "$LOG" >&2; exit 4; }
    if busctl --address="$AT_SPI_BUS_ADDRESS" --no-pager list 2>/dev/null | awk -v p="$SP" '$2==p {f=1} END{exit !f}'; then ok=1; break; fi
    sleep 0.2
  done
  [ "$ok" = 1 ] || echo "run_shell: WARNING gnome-shell (pid $SP) never appeared on the a11y bus" >&2
  # wait until the shell's JS UI is up: it owns org.gnome.Shell long before Main.start() is
  # done (the /org/gnome/Shell object does not exist yet), so wait for its own log line
  for _ in $(seq 300); do grep -q 'GNOME Shell started at' "$LOG" && break; sleep 0.2; done
  # In Shell 48 org.freedesktop.Notifications is owned by a separate service process
  # (org.gnome.Shell.Notifications, normally D-Bus-activated; the private bus has no service
  # dirs) that forwards Notify to the Shell's own /org/freedesktop/Notifications. Run it.
  gjs -m /usr/share/gnome-shell/org.gnome.Shell.Notifications >"$GORCA_HARNESS_DIR/log/shell-notifications.log" 2>&1 &
  NP=$!
  for _ in $(seq 100); do
    busctl --address="$DBUS_SESSION_BUS_ADDRESS" --no-pager --acquired list 2>/dev/null | grep -q '^org.freedesktop.Notifications ' && break
    sleep 0.2
  done
  export WAYLAND_DISPLAY=$WL GORCA_SHELL_PID=$SP GORCA_SHELL_LOG=$LOG GORCA_M7_CBUS=$DBUS_SESSION_BUS_ADDRESS
  {
    echo "== private shell world =="
    echo "load=$(cat /proc/loadavg)"
    for v in DBUS_SESSION_BUS_ADDRESS AT_SPI_BUS_ADDRESS XDG_RUNTIME_DIR WAYLAND_DISPLAY GORCA_SHELL_PID GORCA_SHELL_LOG GORCA_HARNESS_DIR; do echo "$v=${!v}"; done
  } >&2
  "$@" &
  CP=$!
  wait "$CP"; rc=$?
  exit $rc
fi

UNSAFE=1; KEEP=()
while [ $# -gt 0 ]; do
  case "$1" in
    --no-unsafe) UNSAFE=0 ;;
    --keep) KEEP=(--keep) ;;
    --) shift; break ;;
    -*) echo "run_shell: unknown option $1" >&2; exit 64 ;;
    *) break ;;
  esac
  shift
done
[ $# -gt 0 ] || { echo "run_shell: no command" >&2; exit 64; }
export GORCA_PASSTHROUGH="${GORCA_PASSTHROUGH:-} GORCA_PROBE_FILTER"
exec "$ROOT/tests/harness/run-isolated.sh" "${KEEP[@]}" -- "$HERE/run_shell.sh" __inner "$UNSAFE" "$@"
