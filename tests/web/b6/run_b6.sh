#!/bin/bash
# B-6 harness runner: a PRIVATE session (own dbus, a11y bus, registryd, headless gnome-shell with a
# unique Wayland display) and the `b6` binary, which launches its own Firefox with a temporary
# --profile and stops only that process group. Nothing here touches the user's live session.
#
# usage: tests/web/b6/run_b6.sh [b6 run arguments...]
#   e.g. tests/web/b6/run_b6.sh --pages large10k,spa-list-200 --scenarios build,churn --runs 5 \
#                               --out bench/results/web-prep/run1
# env:   B6_BIN (default target/release/b6), B6_FIREFOX (default firefox), B6_KEEP=1 keeps $BASE
#
# Teardown kills only PIDs recorded here (CLAUDE.md "Live-session safety"): never by name.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
B6_BIN=${B6_BIN:-$ROOT/target/release/b6}
[ -x "$B6_BIN" ] || { echo "build first: cargo build --release -p gorca-bench-web (expected $B6_BIN)" >&2; exit 2; }

# Short unique base: AT-SPI P2P sockets live under $XDG_RUNTIME_DIR and sun_path is limited to ~107 bytes.
BASE=/tmp/b6-$$
mkdir -p -m 700 "$BASE/x" "$BASE/tmp"
PIDS=()
cleanup() {
  for ((i=${#PIDS[@]}-1; i>=0; i--)); do kill "${PIDS[i]}" 2>/dev/null; done
  sleep 1
  for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null && kill -KILL "$p" 2>/dev/null; done
  [ "${B6_KEEP:-0}" = 1 ] || rm -rf "$BASE"
}
trap cleanup EXIT

export XDG_RUNTIME_DIR=$BASE/x
export TMPDIR=$BASE/tmp
unset WAYLAND_DISPLAY DISPLAY DBUS_SESSION_BUS_ADDRESS

out=$(dbus-daemon --session --fork --print-address=1 --print-pid=1) || { echo "dbus-daemon failed" >&2; exit 3; }
export DBUS_SESSION_BUS_ADDRESS=$(echo "$out" | sed -n 1p)
PIDS+=("$(echo "$out" | sed -n 2p)")

/usr/libexec/at-spi-bus-launcher --launch-immediately >"$BASE/launcher.log" 2>&1 &
PIDS+=($!)
sleep 1
/usr/libexec/at-spi2-registryd --use-gnome-session >"$BASE/registryd.log" 2>&1 &
PIDS+=($!)
sleep 1
for prop in IsEnabled ScreenReaderEnabled; do
  gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
    --method org.freedesktop.DBus.Properties.Set org.a11y.Status "$prop" "<true>" >/dev/null
done
A11Y_ADDR=$(gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus --method org.a11y.Bus.GetAddress 2>/dev/null \
  | sed -n "s/^('\(.*\)',)$/\1/p")
[ -n "$A11Y_ADDR" ] || A11Y_ADDR=unix:path=$XDG_RUNTIME_DIR/at-spi/bus
echo "private a11y bus: $A11Y_ADDR" >&2

# Firefox has no ATK bridge when headless: it needs a compositor. A private headless gnome-shell on a
# unique Wayland display keeps the window visible and never touches the user's focus.
WL=gorca-b6-$$
export GSETTINGS_BACKEND=memory XDG_SESSION_TYPE=wayland
env -u WAYLAND_DISPLAY -u DISPLAY gnome-shell --headless --wayland --no-x11 --wayland-display="$WL" \
  --virtual-monitor 1280x800 >"$BASE/shell.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 60); do [ -S "$XDG_RUNTIME_DIR/$WL" ] && break; sleep 0.2; done
[ -S "$XDG_RUNTIME_DIR/$WL" ] || { echo "private gnome-shell did not come up; see $BASE/shell.log" >&2; B6_KEEP=1; exit 4; }
sleep 3
export WAYLAND_DISPLAY=$WL MOZ_ENABLE_WAYLAND=1

cd "$ROOT" || exit 1
"$B6_BIN" run --bus "$A11Y_ADDR" --tests-root "$ROOT/tests/web" --firefox "${B6_FIREFOX:-firefox}" "$@"
exit $?
