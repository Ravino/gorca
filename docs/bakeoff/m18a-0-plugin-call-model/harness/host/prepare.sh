#!/usr/bin/env bash
# Usage: host/prepare.sh check            # refuse (exit 1) unless the quiet-host precondition holds:
#          loadavg1 < 1.5, PSI cpu some avg10 < 1.0, every CPU governor is `performance`,
#          and no cargo, rustc or e2e process is running (matched on the exact process name).
#        host/prepare.sh wait [SECS]      # poll the check every 5 s until it passes (exit 0) or SECS
#                                         # (default 900) elapse (exit 1)
#        sudo host/prepare.sh on|off      # bench/b1/quiet-host.sh on/off (governor performance, deep idle off)
# `quiet` runs must pass the check first; `off` must be called at the end and its revert reported.
# Thresholds are overridable for diagnostics only: QUIET_MAX_LOAD, QUIET_MAX_PSI.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../../../.." && pwd)"
max_load="${QUIET_MAX_LOAD:-1.5}"
max_psi="${QUIET_MAX_PSI:-1.0}"

# Prints one reason per line for every violated condition; empty output = quiet.
violations() {
  local load psi gov busy
  load=$(cut -d' ' -f1 /proc/loadavg)
  psi=$(sed -n 's/^some avg10=\([0-9.]*\).*/\1/p' /proc/pressure/cpu)
  awk -v l="$load" -v m="$max_load" 'BEGIN{exit !(l<m)}' || echo "loadavg1=$load >= $max_load"
  awk -v p="$psi" -v m="$max_psi" 'BEGIN{exit !(p<m)}' || echo "psi_cpu_some_avg10=$psi >= $max_psi"
  gov=$(sort -u /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor 2>/dev/null | tr '\n' ' ')
  [ "$gov" = "performance " ] || echo "governor is '${gov% }', not performance"
  # Exact process-name match; this only reads, it never signals anything.
  busy=$(ps -eo pid=,comm= | awk '$2=="cargo"||$2=="rustc"||$2 ~ /e2e/ {printf "%s(%s) ", $2, $1}')
  [ -z "$busy" ] || echo "busy processes: $busy"
}

case "${1:-check}" in
  on | off) exec "$root/bench/b1/quiet-host.sh" "$1" ;;
  check)
    "$root/bench/b1/quiet-host.sh" status
    v=$(violations)
    if [ -n "$v" ]; then
      echo "REFUSED: host is not quiet:" >&2
      echo "$v" | sed 's/^/  /' >&2
      exit 1
    fi
    echo "quiet: loadavg1=$(cut -d' ' -f1 /proc/loadavg)"
    free -g | head -2
    ;;
  wait)
    limit="${2:-900}"
    t0=$SECONDS
    last=""
    while :; do
      v=$(violations)
      if [ -z "$v" ]; then
        echo "quiet after $((SECONDS - t0)) s"
        exit 0
      fi
      if [ "$v" != "$last" ]; then
        echo "waiting for quiet host: $(echo "$v" | tr '\n' ';')" >&2
        last="$v"
      fi
      if [ $((SECONDS - t0)) -ge "$limit" ]; then
        echo "TIMEOUT after $limit s: $(echo "$v" | tr '\n' ';')" >&2
        exit 1
      fi
      sleep 5
    done
    ;;
  *)
    echo "usage: prepare.sh check|wait [SECS]|on|off" >&2
    exit 2
    ;;
esac
