#!/usr/bin/env bash
S=/tmp/scratch/f
cd <repo>
echo "$(date -u +%T) E2f-$1 load $(cat /proc/loadavg)" >> $S/e2f.log
tests/harness/run-isolated.sh $S/e2e-new run --label m3-e2f-$1 --only E2f --repeats 1 --no-fail --reader daemon:$S/gorca-$2,--impersonate-orca --reader-env GORCA_M3=1 > $S/e2f-$1.out 2>&1
