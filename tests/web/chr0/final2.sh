#!/bin/bash
# CHR-0 review round 1 rerun, under host load (label f2): the CHR-0 scenarios on Chrome and the
# regression A/B for Firefox (M8) and Chrome (M9 rig). The baseline runs are the f1 ones (same
# main binary), compare with bench/results/e2e/chr0-f1-ab-base-*.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
T=${CHR0_TAG:-f2load}
export GORCA_M8_DUMP=1
NEW=M8-SLOW,M8-SLOW0,M8-MSG,M8-MSGK
OLD=M8-QN,M8-TB,M8-FM,M8-AF,M8-LM,M8-LV,M8-IF,M8-HD,M8-AN,M8-SY,M8-RD,M8-SH,M8-EL,M8-NEWS,M8-WIKI,M8-FORGE,M8-APP,M8-SPA,M8-CL,M8-BWG,M8-BFB,M8-BHO,M8-TG,M8-FL,M8-FLS,M8-TG2,M8-TG3,M8-RDESC,M8-RNAME,M8-RINV,M8-RNAMES,M8-PF1,M8-PF2
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" chrome "chr0-$T-new-chrome" "$NEW"
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" firefox "chr0-$T-ab-new-m8-firefox" "$OLD"
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" chrome "chr0-$T-ab-new-m9-chrome" "$OLD"
