#!/bin/bash
# Re-review: W3 alone, one repeat per daemon start (the presentation log is per daemon).
cd /tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/src || exit 1
N=${1:-30}
for i in $(seq 1 $N); do
  echo "=== run $i load $(cat /proc/loadavg)"
  tests/web/m7/run_m7_rr.sh m7a-rr-w3-$i --only W3 --repeats 1 --no-fail 2>&1 | tail -n 3
  grep -h "W3.lost\|W3 lost" bench/results/e2e/m7a-rr-w3-$i/*.txt
done
echo DONE
