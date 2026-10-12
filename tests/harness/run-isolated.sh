#!/usr/bin/env bash
# run-isolated.sh - run a command inside a private, headless AT-SPI session.
#
# Usage:
#   run-isolated.sh [options] [--] <cmd> [args...]
#   run-isolated.sh --hold [options]
#
# Options:
#   --hold               Do not run a command. Print the session environment as KEY=VALUE
#                        lines followed by a line "READY", then keep the session alive
#                        until stdin reaches EOF (for Rust test helpers, see README.md).
#   --no-screen-reader   Set only org.a11y.Status.IsEnabled (default sets ScreenReaderEnabled too).
#   --keep               Keep the temp dir after exit (also GORCA_HARNESS_KEEP=1).
#   -h, --help
#
# Extra env vars to pass through to <cmd>: GORCA_PASSTHROUGH="FOO BAR".
#
# Isolation model (see README.md):
#   * private XDG_RUNTIME_DIR/HOME under a fresh mktemp dir; deleted on exit
#   * private session bus: dbus-daemon with a generated config (no service dirs, so nothing
#     from the host can be D-Bus-activated)
#   * a11y bus: started by the real at-spi-bus-launcher against that session bus
#   * at-spi2-registryd started against it
#   * <cmd> runs under `env -i` with an allowlist; DISPLAY/WAYLAND_DISPLAY are never set
#   * every daemon runs in its own session (setsid); teardown kills the process groups and then
#     sweeps /proc for anything still referring to the temp dir.
set -u

: "${GORCA_PASSTHROUGH:=}"
HOLD=0; SCREEN_READER=1; KEEP=${GORCA_HARNESS_KEEP:-0}
while [ $# -gt 0 ]; do
  case "$1" in
    --hold) HOLD=1 ;;
    --no-screen-reader) SCREEN_READER=0 ;;
    --keep) KEEP=1 ;;
    -h|--help) sed -n '2,/^set -u/p' "$0" | sed '$d;s/^# \{0,1\}//'; exit 0 ;;
    --) shift; break ;;
    -*) echo "run-isolated: unknown option $1" >&2; exit 64 ;;
    *) break ;;
  esac
  shift
done
if [ "$HOLD" = 0 ] && [ $# -eq 0 ]; then echo "run-isolated: no command" >&2; exit 64; fi

ROOT=$(mktemp -d "${TMPDIR:-/tmp}/gorca-h.XXXXXX") || exit 70
RT="$ROOT/run"; HOME_DIR="$ROOT/home"
mkdir -m 700 "$RT" "$HOME_DIR" "$ROOT/cfg" "$ROOT/data" "$ROOT/cache" "$ROOT/state" "$ROOT/log" || exit 70
SESSION_ADDR="unix:path=$RT/bus"
PIDS=()   # pids of setsid-ed daemons (each is its own process-group leader)
CMD_PID=""

# Environment for every process the harness starts (daemons and the test command).
base_env() {
  local v extra=()
  for v in PATH LANG LC_ALL TERM CARGO_HOME RUSTUP_HOME CARGO_TARGET_DIR RUST_LOG RUST_BACKTRACE \
           GOROOT GOPATH VIRTUAL_ENV LD_LIBRARY_PATH $GORCA_PASSTHROUGH; do
    [ -n "${!v+x}" ] && extra+=("$v=${!v}")
  done
  printf '%s\0' "HOME=$HOME_DIR" "XDG_RUNTIME_DIR=$RT" "XDG_CONFIG_HOME=$ROOT/cfg" \
    "XDG_DATA_HOME=$ROOT/data" "XDG_CACHE_HOME=$ROOT/cache" "XDG_STATE_HOME=$ROOT/state" \
    "DBUS_SESSION_BUS_ADDRESS=$SESSION_ADDR" "DBUS_SYSTEM_BUS_ADDRESS=unix:path=$ROOT/no-system-bus" \
    "GSETTINGS_BACKEND=memory" "ATSPI_DBUS_IMPLEMENTATION=dbus-daemon" \
    "GORCA_HARNESS_DIR=$ROOT" "${extra[@]}"
}
run_clean() { # run_clean <cmd...> : exec under env -i with the base environment
  local envs=(); while IFS= read -r -d '' e; do envs+=("$e"); done < <(base_env)
  env -i "${envs[@]}" "$@"
}

cleanup() {
  trap '' EXIT INT TERM HUP
  [ -n "$CMD_PID" ] && kill -TERM "$CMD_PID" 2>/dev/null
  local p
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill -TERM -- "-$p" 2>/dev/null; done
  # grace period, then SIGKILL anything left
  local i
  for i in 1 2 3 4 5 6 7 8 9 10; do
    sweep_pids >/dev/null || break
    sleep 0.1
  done
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill -KILL -- "-$p" 2>/dev/null; done
  local left; left=$(sweep_pids)
  [ -n "$left" ] && kill -KILL $left 2>/dev/null
  if [ "$KEEP" = 1 ]; then echo "run-isolated: kept $ROOT" >&2; else rm -rf "$ROOT"; fi
}
# Prints pids (not ours) whose cmdline or environment mention $ROOT.
sweep_pids() {
  local out
  out=$(grep -alF -- "$ROOT" /proc/[0-9]*/cmdline /proc/[0-9]*/environ 2>/dev/null |
        sed -n 's#^/proc/\([0-9]*\)/.*#\1#p' | sort -un | grep -vx "$$")
  [ -n "$out" ] && { echo $out; return 0; }
  return 1
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

start_daemon() { # start_daemon <logname> <cmd...>
  local name=$1; shift
  run_clean setsid "$@" >"$ROOT/log/$name.log" 2>&1 < /dev/null &
  PIDS+=("$!")
}
bus_has_name() { # bus_has_name <address> <name>
  busctl --address="$1" --no-pager --acquired list 2>/dev/null | awk '{print $1}' | grep -qxF -- "$2"
}
wait_for() { # wait_for <what> <timeout_s> <cmd...>
  local what=$1 t=$2; shift 2
  local n=$((t * 50))
  while [ $n -gt 0 ]; do "$@" && return 0; sleep 0.02; n=$((n - 1)); done
  echo "run-isolated: timeout waiting for $what (logs in $ROOT/log)" >&2
  for f in "$ROOT"/log/*.log; do echo "--- $f" >&2; tail -n 20 "$f" >&2; done
  KEEP=0; return 1
}

# 1. private session bus
cat > "$ROOT/session.conf" <<CONF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>$SESSION_ADDR</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
  <limit name="reply_timeout">300000</limit>
</busconfig>
CONF
start_daemon session-bus dbus-daemon --config-file="$ROOT/session.conf" --nofork
wait_for "session bus" 5 test -S "$RT/bus" || exit 70
wait_for "session bus ready" 5 bus_has_name "$SESSION_ADDR" org.freedesktop.DBus || exit 70

# 2. a11y bus via the real launcher (it starts its own dbus-daemon, socket under $RT/at-spi)
start_daemon at-spi-bus-launcher /usr/libexec/at-spi-bus-launcher --launch-immediately
wait_for "org.a11y.Bus" 10 bus_has_name "$SESSION_ADDR" org.a11y.Bus || exit 70
A11Y_ADDR=$(busctl --address="$SESSION_ADDR" --no-pager call org.a11y.Bus /org/a11y/bus org.a11y.Bus GetAddress 2>/dev/null | sed -n 's/^s "\(.*\)"$/\1/p')
[ -n "$A11Y_ADDR" ] || { echo "run-isolated: GetAddress failed" >&2; exit 70; }
case "$A11Y_ADDR" in *"$ROOT"*) ;; *) echo "run-isolated: a11y bus address escapes sandbox: $A11Y_ADDR" >&2; exit 70 ;; esac

# 3. enable accessibility (F-9)
busctl --address="$SESSION_ADDR" set-property org.a11y.Bus /org/a11y/bus org.a11y.Status IsEnabled b true || exit 70
[ "$SCREEN_READER" = 1 ] && { busctl --address="$SESSION_ADDR" set-property org.a11y.Bus /org/a11y/bus org.a11y.Status ScreenReaderEnabled b true || exit 70; }

# 4. registry daemon (no --use-gnome-session: there is no session manager)
start_daemon at-spi2-registryd /usr/libexec/at-spi2-registryd
wait_for "org.a11y.atspi.Registry" 10 bus_has_name "$A11Y_ADDR" org.a11y.atspi.Registry || exit 70

export_env() {
  printf 'DBUS_SESSION_BUS_ADDRESS=%s\nAT_SPI_BUS_ADDRESS=%s\nGORCA_A11Y_BUS_ADDRESS=%s\nXDG_RUNTIME_DIR=%s\nHOME=%s\nGORCA_HARNESS_DIR=%s\n' \
    "$SESSION_ADDR" "$A11Y_ADDR" "$A11Y_ADDR" "$RT" "$HOME_DIR" "$ROOT"
}

if [ "$HOLD" = 1 ]; then
  export_env; echo READY
  cat > /dev/null          # returns on EOF of stdin
  exit 0
fi

# 5. run the command; keep the shell responsive to signals while it runs
envs=(); while IFS= read -r -d '' e; do envs+=("$e"); done < <(base_env)
env -i "${envs[@]}" "AT_SPI_BUS_ADDRESS=$A11Y_ADDR" "GORCA_A11Y_BUS_ADDRESS=$A11Y_ADDR" "$@" &
CMD_PID=$!
wait "$CMD_PID"; rc=$?
CMD_PID=""
exit $rc
