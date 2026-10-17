#!/usr/bin/env bash
S=/tmp/scratch/f
cd <repo>
. /tmp/scratch/envw.sh
pids=()
for i in $(seq 12); do yes >/dev/null & pids+=($!); done
echo "${pids[*]}" > $S/hogs.pids
for i in 1 2 3; do
  echo "$(date -u +%T) A$i load $(cat /proc/loadavg)" >> $S/stressA.log
  tests/harness/run-isolated.sh $S/e2e-old run --label m3-bugA-$i --only E2 --repeats 2 --no-fail --reader daemon:$S/gorca-new,--impersonate-orca --reader-env GORCA_M3=1 >> $S/stressA-$i.out 2>&1
done
kill "${pids[@]}"
echo done >> $S/stressA.log
