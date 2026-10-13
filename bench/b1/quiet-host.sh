#!/usr/bin/env bash
# Prepare / restore the host for an authoritative B-1 run (H-12). Needs root for the sysfs
# writes (sudo asks for a password; the user runs it), so it never runs by itself:
#
#   sudo bench/b1/quiet-host.sh on      # governor performance, idle states deeper than C1 off
#   bench/b1/quiet-host.sh status       # what is set now (no root needed)
#   sudo bench/b1/quiet-host.sh off     # back to what `on` saved (default: powersave, all states)
#
# Why both: the strict service-time check (H-10(a)) compares the round trip of a 0.5 ms fake
# service with a 0.001 ms one. While a core waits 0.5 ms for the reply it enters C2 (127 us exit)
# or C3 (1 048 us exit) on this host, so the model appears 0.2-0.6 ms slower than it is, and
# which cores the threads land on moves the base round trip between 0.24 and 1.6 ms.
# Measured on the development host (powersave, deep idle on, load ~2.5), see
# docs/reports/2026-10-14-b1-harness-gaps.md: unpinned 2 of 6 runs inside +-20 %, pinned 0 of 12.
# `cpupower` is not installed on Debian by default, so plain sysfs is used.
set -euo pipefail
SAVE=/run/b1-quiet-host.saved
cpu=/sys/devices/system/cpu

status() {
  echo "governor: $(sort -u $cpu/cpu*/cpufreq/scaling_governor 2>/dev/null | tr '\n' ' ')"
  for s in $cpu/cpu0/cpuidle/state*; do
    echo "idle $(cat "$s/name") latency=$(cat "$s/latency")us disabled=$(cat "$s/disable")"
  done
  echo "load: $(cat /proc/loadavg)"
}

case "${1:-status}" in
  status) status ;;
  on)
    [ "$(id -u)" = 0 ] || { echo "run with sudo" >&2; exit 1; }
    { echo "governor $(cat $cpu/cpu0/cpufreq/scaling_governor)"; } > "$SAVE"
    for g in $cpu/cpu*/cpufreq/scaling_governor; do echo performance > "$g"; done
    for s in $cpu/cpu*/cpuidle/state*; do
      lat=$(cat "$s/latency")
      if [ "$lat" -gt 20 ]; then echo "$s $(cat "$s/disable")" >> "$SAVE"; echo 1 > "$s/disable"; fi
    done
    status
    ;;
  off)
    [ "$(id -u)" = 0 ] || { echo "run with sudo" >&2; exit 1; }
    gov=powersave
    if [ -f "$SAVE" ]; then gov=$(awk '$1=="governor"{print $2}' "$SAVE"); fi
    for g in $cpu/cpu*/cpufreq/scaling_governor; do echo "$gov" > "$g"; done
    for s in $cpu/cpu*/cpuidle/state*; do
      lat=$(cat "$s/latency")
      if [ "$lat" -gt 20 ]; then echo 0 > "$s/disable"; fi
    done
    rm -f "$SAVE"
    status
    ;;
  *) echo "usage: $0 on|off|status" >&2; exit 64 ;;
esac
