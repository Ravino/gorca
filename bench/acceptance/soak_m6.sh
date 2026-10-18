#!/bin/bash
# The M6 terminal soak (docs/plan/phase-2.md M6 acceptance: zero gnome-terminal-server aborts in 8 h).
#
#   bench/acceptance/soak_m6.sh [--secs N] [--quiet-host] [--label L] [--flood-secs N]
#
# ONE reader and ONE real gnome-terminal-server in private sessions (tests/term/run_real_vte.sh) run the
# VK scenario (crates/gorca-bench/src/e2e/m6.rs): rounds of the VD disturbances and bash use, and a
# `yes` / `seq` flood with typed keys and Ctrl+C every third round. Every round appends one line to
# soak.tsv (RSS, CPU ticks, MemAvailable, failing steps, flood latency, liveness). Results go to
# bench/results/acceptance/soak-m6-<UTC>/ and are never overwritten. Gate: vk_server_aborts == 0 and
# vk_reader_deaths == 0; the rest is reported for triage.
set -u
cd "$(dirname "$0")/../.." || exit 1
ROOT=$PWD
. ./env.sh
secs=28800; quiet=0; label=""; flood=25
while [ $# -gt 0 ]; do
  case "$1" in
    --secs) secs=$2; shift 2;;
    --flood-secs) flood=$2; shift 2;;
    --quiet-host) quiet=1; shift;;
    --label) label=$2; shift 2;;
    *) echo "unknown option $1" >&2; exit 2;;
  esac
done
utc=$(date -u +%Y%m%dT%H%M%SZ)
out=$ROOT/bench/results/acceptance/soak-m6-$utc${label:+-$label}
mkdir -p "$out" || exit 1
for b in target/release/gorca target/release/e2e; do [ -x "$b" ] || { echo "missing $b" >&2; exit 2; }; done
avail=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)
[ "$avail" -ge 6000 ] || { echo "MemAvailable ${avail} MB < 6000, refusing" >&2; exit 2; }
{
  echo "utc=$utc secs=$secs flood_secs=$flood"
  echo "git_rev=$(git rev-parse --short HEAD) dirty_tracked_files=$(git status --porcelain --untracked-files=no | wc -l)"
  echo "kernel=$(uname -r) loadavg=$(cat /proc/loadavg)"
  echo "mem_available_mb=$avail"
  echo "governor=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null)"
} >"$out/host.txt"
QUIET_ON=0
restore() {
  if [ "$QUIET_ON" = 1 ]; then
    QUIET_ON=0
    sudo bench/b1/quiet-host.sh off >>"$out/quiet-host.txt" 2>&1 && echo "quiet-host restored" | tee -a "$out/host.txt"
  fi
}
trap restore EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
if [ "$quiet" = 1 ]; then
  sudo bench/b1/quiet-host.sh on >>"$out/quiet-host.txt" 2>&1 && QUIET_ON=1
  { echo "quiet-host on:"; bench/b1/quiet-host.sh status; } >>"$out/host.txt" 2>&1
fi
# Sample the host every 60 s next to the scenario's own per-round series.
( while :; do echo -e "$(date +%s)\t$(cut -d' ' -f1-3 /proc/loadavg)\t$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)\t$(sed -n 1p /proc/pressure/memory | cut -d' ' -f2)"; sleep 60; done ) >"$out/host-load.tsv" &
SAMPLER=$!
trap 'kill $SAMPLER 2>/dev/null; restore' EXIT
export GORCA_VK_LOG=$out/soak.tsv
echo "soak: $secs s, results in $out"
# The e2e binary lives for the whole soak; cap = soak + 30 min for start-up and teardown.
timeout -k 60 $((secs + 1800)) tests/term/run_real_vte.sh target/release/e2e run --label "soak-m6-$utc" --only VK \
  --reader "daemon:$ROOT/target/release/gorca,--impersonate-orca" --repeats 1 \
  --reader-env GORCA_M6=1 --reader-env GORCA_M6_STRICT=1 --reader-env GORCA_M6_REAL=1 --reader-env NO_COLOR=1 \
  --reader-env "GORCA_VK_SECS=$secs" --reader-env "GORCA_VK_FLOOD_SECS=$flood" \
  --out "$out/res" >"$out/stdout.txt" 2>"$out/stderr.txt" \
  < /dev/null &
RUN=$!

wait $RUN
rc=$?
echo "exit=$rc" | tee -a "$out/host.txt"
echo "loadavg_end=$(cat /proc/loadavg)" >>"$out/host.txt"
exit $rc
