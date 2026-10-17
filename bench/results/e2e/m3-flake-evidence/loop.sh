#!/usr/bin/env bash
cd "$(dirname "$0")/../../../.." && . ./env.sh
for i in 1 2 3 4 5 6 7; do
 for arm in old new; do
  echo "$(date -u +%T) $arm $i load $(cat /proc/loadavg)" >> /tmp/scratch/flake/loop.log
  tests/harness/run-isolated.sh target/release/e2e run --label m3-flakeB-$arm-$i --only E2 --repeats 1 --no-fail --reader daemon:/tmp/scratch/flake/gorca-$arm,--impersonate-orca --reader-env GORCA_M3=1 >> /tmp/scratch/flake/loop.out 2>&1
 done
done
echo done >> /tmp/scratch/flake/loop.log
