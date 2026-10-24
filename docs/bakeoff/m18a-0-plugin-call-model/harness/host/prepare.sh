#!/usr/bin/env bash
# Usage: host/prepare.sh check            # refuse (exit 1) unless loadavg1 < 1.0 and PSI cpu some avg10 < 1.0
#        sudo host/prepare.sh on|off      # bench/b1/quiet-host.sh on/off (governor performance, deep idle off)
# `quiet` runs must call `check` first; `off` must be called at the end and its revert reported.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../../../.." && pwd)"
case "${1:-check}" in
  on|off) exec "$root/bench/b1/quiet-host.sh" "$1" ;;
  check)
    "$root/bench/b1/quiet-host.sh" status
    load=$(cut -d' ' -f1 /proc/loadavg)
    psi=$(sed -n 's/^some avg10=\([0-9.]*\).*/\1/p' /proc/pressure/cpu)
    echo "loadavg1=$load psi_cpu_some_avg10=$psi"
    awk -v l="$load" -v p="$psi" 'BEGIN{exit !(l<1.0 && p<1.0)}' || { echo "REFUSED: host is not quiet" >&2; exit 1; }
    free -g | head -2
    ;;
esac
