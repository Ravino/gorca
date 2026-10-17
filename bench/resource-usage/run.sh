#!/usr/bin/env bash
# Whole-daemon resource usage (CPU, RSS/PSS, threads) under event storms: scenario RU-* of the e2e
# harness (crates/gorca-bench/src/e2e/ru.rs), the real gorca binary in a private session.
#
#   bench/resource-usage/run.sh <label> [--levels 0,1000,10000,...,typ] [--topos single,lanes-rt]
#                               [--repeats N] [--no-build]
#
# Results: bench/results/resource-usage/<label>/raw/<topo>/<level>/r<N>/{*.json,*.txt,RU-*.csv,
# reader.log,meta.txt}. A rerun of the same label refuses to start; failed runs are kept.
# Aborts a run (kept, marked) when MemAvailable < 4 GB.
set -euo pipefail
cd "$(dirname "$0")/../.."
. ./env.sh
label=${1:?usage: run.sh <label> [options]}
shift
levels="0,typ,1000,10000,50000,100000,200000,400000"; topos="single,lanes-rt"; repeats=3; build=1
while [ $# -gt 0 ]; do
  case "$1" in
    --levels) levels=$2; shift ;;
    --topos) topos=$2; shift ;;
    --repeats) repeats=$2; shift ;;
    --no-build) build=0 ;;
    *) echo "unknown option $1" >&2; exit 64 ;;
  esac
  shift
done
out=bench/results/resource-usage/$label
if [ -e "$out" ]; then echo "$out exists; pick a new label" >&2; exit 1; fi
mkdir -p "$out/raw"
root=$(pwd)
gorca=$root/target/release/gorca
if [ "$build" = 1 ]; then cargo build --release -p gorca-bench --bins -p gorca-daemon --offline; fi
{
  echo "label=$label utc=$(date -u +%FT%TZ)"
  echo "git_rev=$(git rev-parse --short HEAD) dirty=$(git status --porcelain --untracked-files=no | wc -l)"
  echo "kernel=$(uname -r) cpus=$(nproc)"
  echo "governor=$(sort -u /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor 2>/dev/null | tr '\n' ' ')"
  echo "loadavg_start=$(cat /proc/loadavg)"
  echo "levels=$levels topos=$topos repeats=$repeats"
  free -m | head -2
} >"$out/host.txt"
mem_avail_mb() { awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo; }
busy_pct() {
  read -r _ u1 n1 s1 i1 w1 q1 sq1 _ </proc/stat; sleep 1; read -r _ u2 n2 s2 i2 w2 q2 sq2 _ </proc/stat
  local t1=$((u1+n1+s1+i1+w1+q1+sq1)) t2=$((u2+n2+s2+i2+w2+q2+sq2))
  echo $(( 100 * ((t2-t1) - (i2+w2-i1-w1)) / (t2-t1) ))
}
IFS=, read -r -a lv <<<"$levels"
IFS=, read -r -a tp <<<"$topos"
for ((r = 1; r <= repeats; r++)); do
  for l in "${lv[@]}"; do
    for ((k = 0; k < ${#tp[@]}; k++)); do
      t=${tp[$(((k + r - 1) % ${#tp[@]}))]}
      dir=$out/raw/$t/$l/r$r
      mkdir -p "$dir"
      avail=$(mem_avail_mb)
      if [ "$avail" -lt 4096 ]; then
        echo "ABORT level $l topo $t r$r: MemAvailable ${avail} MB < 4096" | tee "$dir/ABORTED"
        continue
      fi
      load0=$(cat /proc/loadavg); busy0=$(busy_pct)
      echo "== r$r topo $t level $l (load $load0, avail ${avail} MB)"
      status=0
      id=RU-$l
      GORCA_PASSTHROUGH="RU_OUT" RU_OUT="$root/$dir" tests/harness/run-isolated.sh target/release/e2e run \
        --label "ru-$t-$l-r$r" --repeats 1 --only "$id" \
        --reader "daemon:$gorca,--impersonate-orca,--topology=$t" \
        --reader-env GORCA_M1=1 --reader-env GORCA_M4=1 --reader-env GORCA_M4B=1 \
        --reader-env "GORCA_TOPOLOGY_REPORT=$root/$dir/rep.%p.json" \
        --reader-env "GORCA_RTKIT_BUS_ADDRESS=unix:path=/run/dbus/system_bus_socket" \
        --out "$dir" --no-fail >"$dir/stdout.txt" 2>"$dir/stderr.txt" || status=$?
      {
        echo "exit=$status topo=$t level=$l round=$r"
        echo "load_before=$load0 background_cpu_busy_pct=$busy0 load_after=$(cat /proc/loadavg)"
        echo "mem_available_mb_before=$avail after=$(mem_avail_mb)"
      } >"$dir/meta.txt"
    done
  done
done
echo "loadavg_end=$(cat /proc/loadavg)" >>"$out/host.txt"
