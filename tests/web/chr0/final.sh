#!/bin/bash
# CHR-0 final verification, sequential (one rig at a time):
#   1. the CHR-0 scenarios (M8-SLOW, M8-SLOW0, M8-MSG, M8-MSGK) on Chrome, Firefox, Chromium, Electron;
#   2. the existing M8 corpus on Firefox, and through the M9 rig on Chrome, for main (baseline)
#      and for this tree (the regression A/B).
# CHR0_BASE_BIN: a copy of main's gorca binary. CHR0_TAG: label suffix (a rerun never overwrites).
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
BASE=${CHR0_BASE_BIN:?set CHR0_BASE_BIN to a copy of the main gorca binary}
T=${CHR0_TAG:-f1}
export GORCA_M8_DUMP=1
NEW=M8-SLOW,M8-SLOW0,M8-MSG,M8-MSGK
OLD=M8-QN,M8-TB,M8-FM,M8-AF,M8-LM,M8-LV,M8-IF,M8-HD,M8-AN,M8-SY,M8-RD,M8-SH,M8-EL,M8-NEWS,M8-WIKI,M8-FORGE,M8-APP,M8-SPA,M8-CL,M8-BWG,M8-BFB,M8-BHO,M8-TG,M8-FL,M8-FLS,M8-TG2,M8-TG3,M8-RDESC,M8-RNAME,M8-RINV,M8-RNAMES,M8-PF1,M8-PF2

E2E_EXTRA="--repeats 3" "$HERE/run_e2e.sh" chrome "chr0-$T-new-chrome" "$NEW"
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" chrome "chr0-$T-base-chrome" "$NEW" "$BASE"
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" firefox "chr0-$T-new-firefox" M8-SLOW,M8-SLOW0,M8-MSG
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" firefox "chr0-$T-base-firefox" M8-SLOW,M8-SLOW0,M8-MSG "$BASE"
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" chromium "chr0-$T-new-chromium" "$NEW"
# Electron: the held-open load hides the document from the rig's find_doc (research report, section 4)
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" electron "chr0-$T-new-electron" M8-SLOW0,M8-MSG,M8-MSGK

# regression A/B: the existing corpus
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" firefox "chr0-$T-ab-new-m8-firefox" "$OLD"
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" firefox "chr0-$T-ab-base-m8-firefox" "$OLD" "$BASE"
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" chrome "chr0-$T-ab-new-m9-chrome" "$OLD"
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" chrome "chr0-$T-ab-base-m9-chrome" "$OLD" "$BASE"
