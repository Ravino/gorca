#!/usr/bin/env bash
# Build the guest component and the host bench, then run every section. Throwaway spike.
# Usage: tools/spikes/wasm-plugin-cost/run.sh [build|run]   (needs `. ./env.sh` first)
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/var/tmp/gorca-shared-target}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
avail=$(df --output=avail -BG / | tail -1 | tr -dc 0-9)
if [ "$avail" -lt 6 ]; then echo "less than 6 GB free"; exit 1; fi
if [ "${1:-run}" = build ] || [ "${1:-run}" = run ]; then
  (cd "$here/guest" && cargo build --release --target wasm32-wasip2)
  (cd "$here/host" && cargo build --release)
fi
[ "${1:-run}" = build ] && exit 0
g="$CARGO_TARGET_DIR/wasm32-wasip2/release/spike_guest.wasm"
h="$CARGO_TARGET_DIR/release/wasm-plugin-cost"
echo "host: $(cat /proc/loadavg) governor=$(sort -u /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor | tr '\n' ' ')"
for s in load calls hot safety async; do "$h" "$g" "$s"; done
for a in ondemand pooling; do for st in min 1mb; do for r in 1 2 3; do "$h" "$g" mem "$a" "$st"; done; done; done
