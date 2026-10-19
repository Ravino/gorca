#!/bin/bash
# The CHR-0 e2e matrix: new reader on Chrome, baseline (main b4310146) on Chrome, new reader on
# Firefox, Chromium and Electron. Results land in bench/results/e2e/<label>/; nothing is deleted.
#   CHR0_TAG=r2  label suffix, so a rerun never overwrites an earlier result
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
BASE=${CHR0_BASE_BIN:?set CHR0_BASE_BIN to a copy of the main gorca binary}
T=${CHR0_TAG:-r1}
ONLY=M8-SLOW,M8-SLOW0,M8-MSG,M8-MSGK
export GORCA_M8_DUMP=1
E2E_EXTRA="--repeats 3" "$HERE/run_e2e.sh" chrome "chr0-$T-new-chrome" "$ONLY"
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" chrome "chr0-$T-base-chrome" "$ONLY" "$BASE"
# real keys cannot reach the Firefox page in the rig (RIG_KEYS): M8-MSGK is Chromium only
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" firefox "chr0-$T-new-firefox" M8-SLOW,M8-SLOW0,M8-MSG
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" firefox "chr0-$T-base-firefox" M8-SLOW,M8-SLOW0,M8-MSG "$BASE"
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" chromium "chr0-$T-new-chromium" "$ONLY"
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" electron "chr0-$T-new-electron" "$ONLY"
