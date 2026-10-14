#!/bin/bash
# Full web-prep measurement suite: build / churn / scale / research on Firefox, research and churn on
# Chromium, each scenario in its own private session and browser (see run_b6.sh).
#
# usage: tests/web/b6/run_all.sh OUT_DIR
# env:   CHROMIUM (default: Playwright's chromium-1243), B6_BIN
#
# Conditions to record next to the results: host load, CPU governor, kernel (RT), whether Orca/the
# user's Firefox were running. The suite never touches them; it only competes for CPU.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${1:?usage: run_all.sh OUT_DIR}
CHROMIUM=${CHROMIUM:-$HOME/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome}
mkdir -p "$OUT"
{
  echo "date: $(date -u +%FT%TZ)"
  echo "kernel: $(uname -r)"
  echo "cpus: $(nproc)"
  echo "governor: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo n/a)"
  echo "loadavg_start: $(cut -d' ' -f1-3 /proc/loadavg)"
  echo "firefox: $(firefox --version 2>/dev/null)"
  echo "chromium: $("$CHROMIUM" --version 2>/dev/null)"
} > "$OUT/conditions.txt"

run() { "$HERE/run_b6.sh" "$@"; }

run --pages large1k,large10k,large100k --scenarios build --runs 10 --full-matrix --out "$OUT/firefox"
run --pages spa-list-200,spa-list-2000,large10k --scenarios churn --out "$OUT/firefox"
run --pages spa-list-200,spa-list-2000 --scenarios churn-nodomid --out "$OUT/firefox"
run --pages large10k,large100k --scenarios scale --runs 10 --out "$OUT/firefox"
run --pages virtual-unmount,virtual-recycle,virtual-grid,virt-tanstack-index,virt-tanstack-slot,virt-vue-recycle,virt-cdk,spa-list-200,large10k \
    --scenarios research --out "$OUT/firefox"

CR=(--browser chromium --browser-bin "$CHROMIUM")
run "${CR[@]}" --pages large1k,large10k --scenarios build --runs 5 --out "$OUT/chromium"
run "${CR[@]}" --pages spa-list-200 --scenarios churn,churn-nodomid --out "$OUT/chromium"
run "${CR[@]}" --pages virtual-unmount,virtual-recycle,virtual-grid,virt-tanstack-index,virt-tanstack-slot,virt-vue-recycle,virt-cdk,spa-list-200,large10k \
    --scenarios research --out "$OUT/chromium"

echo "loadavg_end: $(cut -d' ' -f1-3 /proc/loadavg)" >> "$OUT/conditions.txt"
