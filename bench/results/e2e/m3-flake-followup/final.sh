#!/usr/bin/env bash
S=/tmp/scratch/f
cd <repo>
cp target/release/gorca $S/gorca-fix
cp target/release/e2e $S/e2e-new
run() { # label arm only repeats
  echo "$(date -u +%T) $1 load $(cat /proc/loadavg)" >> $S/final.log
  tests/harness/run-isolated.sh $S/e2e-new run --label $1 --only $3 --repeats $4 --no-fail --reader daemon:$S/gorca-$2,--impersonate-orca --reader-env GORCA_M3=1 > $S/final-$1.out 2>&1
}
run m3-fin-suite fix C1,C1h,C2,E1,E2f,S2,SA1,SA2,SA3,SA3a,PW 3
for i in 1 2 3 4 5; do
  run m3-fin-e2-fix-$i fix E2 1
  run m3-fin-e2-base-$i new E2 1
done
pids=()
for i in $(seq 12); do yes >/dev/null & pids+=($!); done
echo "${pids[*]}" > $S/hogs2.pids
echo "$(date -u +%T) hogs started ${pids[*]}" >> $S/final.log
for i in 1 2; do
  run m3-fin-hog-e2-fix-$i fix E2 2
  run m3-fin-hog-e2-base-$i new E2 2
done
run m3-fin-hog-e2f-fix fix E2f 1
run m3-fin-hog-e2f-base new E2f 1
kill "${pids[@]}"
echo "$(date -u +%T) fin load $(cat /proc/loadavg)" >> $S/final.log
