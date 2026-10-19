#!/bin/bash
# CHR-0 review round 2 rerun: CHR-0 scenarios on Chrome, M8 quick on Firefox.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
export GORCA_M8_DUMP=1
E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" chrome chr0-f3-new-chrome M8-SLOW,M8-SLOW0,M8-MSG,M8-MSGK
E2E_EXTRA="--repeats 1 --quick" "$HERE/run_e2e.sh" firefox chr0-f3-quick-m8-firefox M8-QN,M8-TB,M8-FM,M8-AF,M8-LM,M8-LV,M8-IF,M8-HD,M8-AN,M8-SY,M8-RD,M8-SH,M8-EL,M8-NEWS,M8-WIKI,M8-FORGE,M8-APP,M8-SPA,M8-CL,M8-BWG,M8-BFB,M8-BHO,M8-TG,M8-FL,M8-FLS,M8-TG2,M8-TG3,M8-SLOW0,M8-MSG
