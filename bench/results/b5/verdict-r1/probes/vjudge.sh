#!/usr/bin/env bash
# Reviewer re-run of B-5 (verdict): judge.sh loop with --allow-stale-reader (binary = d0a13c0 code,
# stamped e185a10 because the relink at HEAD was OOM-killed). Hog stopped by recorded PID only.
set -uo pipefail
cd <repo>/.claude/worktrees/agent-ab8dc123ed2f390a2
. ./env.sh
label=$1; rounds=$2; arms=$3; conds=$4; only=$5
out=bench/results/b5/$label; [ -e "$out" ] && { echo exists; exit 1; }
mkdir -p "$out/raw"; root=$(pwd); gorca=$root/target/release/gorca
rtkit_bus=unix:path=/run/dbus/system_bus_socket
{ echo "label=$label reviewer verdict run; binary code=d0a13c0 stamp=$($gorca --version)"; echo "loadavg_start=$(cat /proc/loadavg)"; echo "rounds=$rounds arms=$arms conds=$conds only=$only"; free -g | head -2; } >"$out/host.txt"
IFS=, read -r -a A <<<"$arms"; IFS=, read -r -a C <<<"$conds"
hog=""; stop_hog(){ [ -n "$hog" ] && { kill "$hog" 2>/dev/null; wait "$hog" 2>/dev/null; hog=""; }; }; trap stop_hog EXIT INT TERM
n=${#A[@]}
for ((r=1;r<=rounds;r++)); do for c in "${C[@]}"; do for ((k=0;k<n;k++)); do
  arm=${A[$(((k+r-1)%n))]}; d=$out/raw/$c/$arm/r$r; mkdir -p "$d"; l0=$(cat /proc/loadavg)
  if [ "$c" = sat ]; then target/release/gorca-hog --secs 1800 >"$d/hog.txt" 2>&1 & hog=$!; sleep 2; fi
  echo "== r$r $c $arm load $(cat /proc/loadavg)"
  st=0
  tests/harness/run-isolated.sh target/release/e2e run --allow-stale-reader --label "b5v-$arm-$c-r$r" --repeats 1 --only "$only" \
    --reader "daemon:$root/bench/b5/reader-cpu.sh,$gorca,--impersonate-orca,--topology=$arm" \
    --reader-env "GORCA_CPU_OUT=$root/$d/cpu.txt" --reader-env GORCA_M1=1 --reader-env GORCA_M4=1 --reader-env GORCA_M4B=1 \
    --reader-env "GORCA_TOPOLOGY_REPORT=$root/$d/rep.%p.json" --reader-env "GORCA_RTKIT_BUS_ADDRESS=$rtkit_bus" \
    --out "$d" --no-fail >"$d/stdout.txt" 2>"$d/stderr.txt" || st=$?
  stop_hog
  printf 'round=%s cond=%s arm=%s exit=%s\nload_before=%s\nload_after=%s\n' $r $c $arm $st "$l0" "$(cat /proc/loadavg)" >"$d/meta.txt"
  echo "   exit $st"
done; done; done
python3 bench/b5/summarize.py "$out" >/dev/null 2>&1; echo DONE
