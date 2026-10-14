#!/bin/bash
# The identity / update-latency part of the suite only (churn scenarios, Firefox and Chromium),
# for re-measuring after a change to the source's update path.
# usage: tests/web/b6/run_churn.sh OUT_DIR
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${1:?usage: run_churn.sh OUT_DIR}
CHROMIUM=${CHROMIUM:-$HOME/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome}
mkdir -p "$OUT"
run() { "$HERE/run_b6.sh" "$@"; }
run --pages spa-list-200,spa-list-2000,large10k --scenarios churn --out "$OUT/firefox"
run --pages spa-list-200,spa-list-2000 --scenarios churn-nodomid --out "$OUT/firefox"
run --browser chromium --browser-bin "$CHROMIUM" --pages spa-list-200 --scenarios churn,churn-nodomid --out "$OUT/chromium"
