#!/usr/bin/env bash
# B-5 runtime-topology judge: the same daemon binary under each topology arm, interleaved, with and
# without a saturating M-15 hog load. See docs/bakeoff/b5-runtime-topology/SPEC.md.
#
#   bench/b5/judge.sh <label> [--quick] [--rounds N] [--arms single,lanes,lanes-rt]
#                     [--conds idle,sat] [--only L1,L2,K1,L3-50k,L2-50k,K1-50k] [--no-build]
#
# For every round, every condition and every arm (order rotated each round, so no arm is always
# first or last) it runs one `e2e run` inside tests/harness/run-isolated.sh (private session, private
# a11y bus: never the live desktop). Condition `sat` starts `gorca-hog` (one busy thread per CPU, the
# default scheduling class) before the run and stops it by the PID recorded here, never by name.
#
# Results: bench/results/b5/<label>/raw/<cond>/<arm>/r<N>/{*.json,*.txt,rep.<pid>.json,meta.txt}
# and, from bench/b5/summarize.py, summary.json and summary.md next to raw/. Failing or noisy runs
# are kept, never overwritten: a rerun of the same label refuses to start.
#
# rtkit lives on the real system bus, which the harness replaces with nothing; the reader is given
# its address explicitly (GORCA_RTKIT_BUS_ADDRESS). The only thing asked of rtkit is a real-time
# class for the reader's own threads, in the private test process.
set -euo pipefail
cd "$(dirname "$0")/../.."
. ./env.sh

label=${1:?usage: judge.sh <label> [--quick] [--rounds N] [--arms ...] [--conds ...] [--only ...] [--no-build]}
shift
quick=""; rounds=3; arms="single,lanes,lanes-rt"; conds="idle,sat"
only="L1,L2,K1,L3-50k,L2-50k,K1-50k,K2,K2s,K3,R1,R1s,A1"; build=1
while [ $# -gt 0 ]; do
  case "$1" in
    --quick) quick="--quick" ;;
    --rounds) rounds=$2; shift ;;
    --arms) arms=$2; shift ;;
    --conds) conds=$2; shift ;;
    --only) only=$2; shift ;;
    --no-build) build=0 ;;
    *) echo "judge: unknown option $1" >&2; exit 64 ;;
  esac
  shift
done

out=bench/results/b5/$label
if [ -e "$out" ]; then echo "judge: $out exists; results are never overwritten (pick a new label)" >&2; exit 1; fi
mkdir -p "$out/raw"
root=$(pwd)
gorca=$root/target/release/gorca
rtkit_bus=${GORCA_RTKIT_BUS_ADDRESS:-unix:path=/run/dbus/system_bus_socket}

if [ "$build" = 1 ]; then
  cargo build --release -p gorca-bench --bins -p gorca-daemon --offline
fi

IFS=, read -r -a arm_list <<<"$arms"
IFS=, read -r -a cond_list <<<"$conds"

gov() { sort -u /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor 2>/dev/null | tr '\n' ' '; }
rtprop() { busctl --system get-property org.freedesktop.RealtimeKit1 /org/freedesktop/RealtimeKit1 org.freedesktop.RealtimeKit1 "$1" 2>&1 | tr '\n' ' '; }

{
  echo "label=$label"
  echo "utc=$(date -u +%FT%TZ)"
  echo "git_rev=$(git rev-parse --short HEAD) dirty=$(git status --porcelain --untracked-files=no | wc -l)"
  echo "kernel=$(uname -r)"
  echo "cpus=$(nproc)"
  echo "governor=$(gov)"
  echo "loadavg_start=$(cat /proc/loadavg)"
  echo "rounds=$rounds arms=$arms conds=$conds only=$only quick=${quick:-no}"
  echo "rtkit MaxRealtimePriority: $(rtprop MaxRealtimePriority)"
  echo "rtkit MinNiceLevel: $(rtprop MinNiceLevel)"
  echo "rtkit RTTimeUSecMax: $(rtprop RTTimeUSecMax)"
  echo "RLIMIT_RTPRIO=$(ulimit -r) RLIMIT_RTTIME=$(ulimit -R 2>/dev/null || echo ?)"
  echo "quiet-host:"; bench/b1/quiet-host.sh status 2>&1 | sed 's/^/  /'
} >"$out/host.txt"

# Background CPU busy percentage of the whole host over one second (other agents' work), before the
# hog starts: the "how quiet was it" record next to the load average.
busy_pct() {
  read -r _ u1 n1 s1 i1 w1 q1 sq1 _ </proc/stat; sleep 1; read -r _ u2 n2 s2 i2 w2 q2 sq2 _ </proc/stat
  local t1=$((u1+n1+s1+i1+w1+q1+sq1)) t2=$((u2+n2+s2+i2+w2+q2+sq2))
  echo $(( 100 * ((t2-t1) - (i2+w2-i1-w1)) / (t2-t1) ))
}

hog_pid=""
stop_hog() {
  if [ -n "$hog_pid" ]; then
    kill "$hog_pid" 2>/dev/null || true
    wait "$hog_pid" 2>/dev/null || true
    hog_pid=""
  fi
}
trap stop_hog EXIT INT TERM

n_arms=${#arm_list[@]}
for ((round = 1; round <= rounds; round++)); do
  for cond in "${cond_list[@]}"; do
    for ((k = 0; k < n_arms; k++)); do
      arm=${arm_list[$(((k + round - 1) % n_arms))]}
      dir=$out/raw/$cond/$arm/r$round
      mkdir -p "$dir"
      load0=$(cat /proc/loadavg)
      busy0=$(busy_pct)
      if [ "$cond" = sat ]; then
        # Long enough for any run; the hog is stopped by PID right after the run.
        target/release/gorca-hog --secs 1800 >"$dir/hog.txt" 2>&1 &
        hog_pid=$!
        sleep 2
      fi
      load_hog=$(cat /proc/loadavg)
      echo "== round $round cond $cond arm $arm (load $load_hog)"
      status=0
      tests/harness/run-isolated.sh target/release/e2e run \
        --label "b5-$arm-$cond-r$round" $quick --repeats 1 --only "$only" \
        --reader "daemon:$root/bench/b5/reader-cpu.sh,$gorca,--impersonate-orca,--topology=$arm" \
        --reader-env "GORCA_CPU_OUT=$root/$dir/cpu.txt" \
        --reader-env GORCA_M1=1 --reader-env GORCA_M4=1 --reader-env GORCA_M4B=1 \
        --reader-env "GORCA_TOPOLOGY_REPORT=$root/$dir/rep.%p.json" \
        --reader-env "GORCA_RTKIT_BUS_ADDRESS=$rtkit_bus" \
        --out "$dir" --no-fail >"$dir/stdout.txt" 2>"$dir/stderr.txt" || status=$?
      stop_hog
      {
        echo "round=$round cond=$cond arm=$arm exit=$status"
        echo "load_before=$load0"
        echo "background_cpu_busy_pct=$busy0"
        echo "load_with_hog=$load_hog"
        echo "load_after=$(cat /proc/loadavg)"
        echo "governor=$(gov)"
      } >"$dir/meta.txt"
      if [ "$status" != 0 ]; then echo "   e2e exited $status (kept in $dir)"; fi
    done
  done
done

echo "loadavg_end=$(cat /proc/loadavg)" >>"$out/host.txt"
python3 bench/b5/summarize.py "$out"
