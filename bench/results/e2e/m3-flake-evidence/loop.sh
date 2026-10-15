#!/usr/bin/env bash
cd <repo>; . ./env.sh
for i in 1 2 3 4 5 6 7; do
 for arm in old new; do
  echo "$(date -u +%T) $arm $i load $(cat /proc/loadavg)" >> /tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/flake/loop.log
  tests/harness/run-isolated.sh target/release/e2e run --label m3-flakeB-$arm-$i --only E2 --repeats 1 --no-fail --reader daemon:/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/flake/gorca-$arm,--impersonate-orca --reader-env GORCA_M3=1 >> /tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/flake/loop.out 2>&1
 done
done
echo done >> /tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/flake/loop.log
