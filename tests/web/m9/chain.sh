#!/bin/bash
# One engine, the whole M9 baseline set, one repeat each (status), results under bench/results/e2e/<prefix>-*.
#   tests/web/m9/chain.sh chromium|firefox <prefix> [m7|typing|m8 ...]   (default: all three)
# Chromium wake mode comes from M9_AT (default poke). Each scenario group is its own private session,
# so a failure of one group does not stop the next.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
cd "$ROOT" || exit 1
ENGINE=${1:?engine}
PREFIX=${2:?prefix}
shift 2
GROUPS_=${*:-m7 typing m8}
M7IDS=B0,W1,BR1,BR2,W2,W2b,W3,FM1,FM2
TYIDS=FC1h,FC1,FE1,FCE2,FE2,FS2,FSA1,FSA3,FCE1,FT1,FP1,BSA1,BAR1,BL1,FR1
run() { # run m7|m8 <label> <args...>
  local kind=$1; shift
  if [ "$ENGINE" = chromium ]; then
    "$HERE/run_m9.sh" "$kind" "$@"
  elif [ "$kind" = m7 ]; then
    "$ROOT/tests/web/m7/run_m7.sh" "$@"
  else
    "$ROOT/tests/web/m8/run_m8.sh" "$@"
  fi
}
for g in $GROUPS_; do
  echo "== $(date -u +%T) $ENGINE $g load $(cat /proc/loadavg)"
  case "$g" in
    m7) run m7 "$PREFIX-m7" --only "$M7IDS" --repeats 1 ;;
    typing) run m7 "$PREFIX-typing" --only "$TYIDS" --repeats 1 ;;
    m8) run m8 "$PREFIX-m8" --quick --repeats 1 ;;
  esac
  echo "== $(date -u +%T) $ENGINE $g exit $?"
done
