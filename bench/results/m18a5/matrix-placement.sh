#!/bin/bash
# usage: m185-matrix2.sh <resfile> <reps> <variants...>  (variants: off, v2, v3, v4, v3-warm); interleaved
BIN=/var/tmp/m18a5/plugin_latency4
RES=$1; reps=$2; shift 2
VARS="$@"
for rep in $(seq 1 $reps); do
 for sch in sps; do
  for sp in 24 200; do
   for var in $VARS; do
     [ "$var" = v3-warm ] && [ "$sch" = labels ] && continue
     for i in $(seq 1 18); do
       l=$(cut -d' ' -f1 /proc/loadavg); awk -v l=$l 'BEGIN{exit !(l<4)}' && break; sleep 5
     done
     pl=auto; vn=""; warm=""
     case $var in
       off) pl=off;;
       v2) vn=2;; v3) vn=3;; v4) vn=4;;
       v3-warm) vn=3; warm=1;;
     esac
     env GORCA_PLUGIN_PLACEMENT=$pl ${vn:+GORCA_PLACEMENT_VARIANT=$vn} ${warm:+GORCA_BENCH_WARM=1} GORCA_PLUGIN_SCHEDULER=$sch \
       GORCA_BENCH_DUMMY_TIER=first GORCA_BENCH_SPACING_MS=$sp GORCA_BENCH_STAGES=1 GORCA_BENCH_OUT=$RES timeout 900 $BIN --nocapture \
       > /var/tmp/m18a5/log4-$var-$sch-$sp.log 2>&1
     echo "== $var $sch first ${sp}ms" >> $RES
   done
  done
 done
done
echo DONE >> $RES
