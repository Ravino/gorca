#!/usr/bin/env bash
cd <repo>; . ./env.sh
pids=()
for i in $(seq 12); do yes >/dev/null & pids+=($!); done
echo "${pids[*]}" > /tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/flake/hogs.pids
for i in 1 2; do
 for arm in old new instr; do
  echo "$(date -u +%T) $arm $i load $(cat /proc/loadavg)" >> /tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/flake/stress.log
  tests/harness/run-isolated.sh target/release/e2e run --label m3-flakeS-$arm-$i --only E2 --repeats 2 --no-fail --reader daemon:/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/flake/gorca-$arm,--impersonate-orca --reader-env GORCA_M3=1 >> /tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/flake/stress-$arm.out 2>&1
 done
done
kill "${pids[@]}"
echo done >> /tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/flake/stress.log
