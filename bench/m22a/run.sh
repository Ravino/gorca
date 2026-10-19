#!/usr/bin/env bash
# M22a-A4 / B6 runner: builds the benches (CARGO_TARGET_DIR outside /tmp) and runs them N times,
# pinned to one core, writing JSON to bench/m22a/results/ and the raw logs to bench/m22a/logs/.
#
#   bench/m22a/run.sh [runs] [cpu]        # defaults: 3 runs, cpu 2
#   M22A_ONLY=storm,focus bench/m22a/run.sh 1
#
# Every result file records the load average, governor and memory pressure at start (and the
# end load for the fuse bench). Nothing is deleted: a failing run stays next to the passing ones.
# Run from the repository root with the toolchain environment loaded (`. ./env.sh`).
set -euo pipefail
cd "$(dirname "$0")/../.."
runs="${1:-3}"
cpu="${2:-2}"
day="$(date +%Y%m%d)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/var/tmp/m22a-b6-target}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
mkdir -p bench/m22a/logs bench/m22a/results
cargo bench -p gorca-bench --bench fuse --bench sem_repr --no-run
fuse="$(ls -t "$CARGO_TARGET_DIR"/release/deps/fuse-* | grep -v '\.d$' | head -1)"
semr="$(ls -t "$CARGO_TARGET_DIR"/release/deps/sem_repr-* | grep -v '\.d$' | head -1)"
for i in $(seq 1 "$runs"); do
  stamp="${M22A_STAMP_PREFIX:-$day}-run$i"
  echo "== run $i ($stamp), load $(cat /proc/loadavg)"
  M22A_STAMP="$stamp" M22A_CPU="$cpu" "$fuse" > "bench/m22a/logs/$stamp-fuse.log" 2>&1 || true
  if [ -z "${M22A_SKIP_SEMR:-}" ]; then
    M22A_STAMP="$stamp" taskset -c "$cpu" "$semr" --bench > "bench/m22a/logs/$stamp-sem_repr.log" 2>&1 || true
  fi
done
echo "results: bench/m22a/results/, logs: bench/m22a/logs/"
