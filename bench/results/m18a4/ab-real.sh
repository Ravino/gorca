#!/bin/bash
# usage: ab.sh <tag> <spacing_ms> <tier> <sched> bin...   (interleaved, one run per bin)
cd <repo>/.claude/worktrees/agent-a01467fdcd7a26263
tag=$1; sp=$2; tier=$3; sch=$4; shift 4
for b in "$@"; do
  n=$(basename $b)
  GORCA_PLUGIN_SCHEDULER=$sch GORCA_BENCH_DUMMY_TIER=$tier GORCA_BENCH_SPACING_MS=$sp GORCA_BENCH_STAGES=1 \
    GORCA_BENCH_OUT=/var/tmp/m18a4/res-$tag.txt timeout 900 $b --nocapture > /var/tmp/m18a4/log-$tag-$n.log 2>&1
  echo "== $n $sch $tier ${sp}ms" >> /var/tmp/m18a4/res-$tag.txt
done
