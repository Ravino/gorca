#!/usr/bin/env bash
# Interleaved A/B of VT+V1, main core vs fix core, same harness, pinned to CPUs 2-5.
set -u
cd <repo>/.claude/worktrees/m6-focus-review
. ./env.sh
S=/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/m6ab
for pair in 1 2 3 4 5 6; do
  seed=$((pair * 7919))
  if [ $((pair % 2)) = 1 ]; then order="main fix"; else order="fix main"; fi
  for arm in $order; do
    echo "== pair $pair arm $arm seed $seed load $(cat /proc/loadavg) $(date -u +%H:%M:%S)" >> $S/ab.log
    taskset -c 2-5 tests/harness/run-isolated.sh target/release/e2e run \
      --label rv-ab-$arm-p$pair --reader daemon:$S/gorca-$arm,--impersonate-orca --allow-stale-reader \
      --only VT,V1 --repeats 1 --seed $seed --no-fail \
      --reader-env GORCA_M1=1 --reader-env GORCA_M6=1 --reader-env GORCA_M6_STRICT=1 \
      > $S/run-$arm-p$pair.log 2>&1
    echo "   exit $? load $(cat /proc/loadavg)" >> $S/ab.log
  done
done
echo DONE >> $S/ab.log
