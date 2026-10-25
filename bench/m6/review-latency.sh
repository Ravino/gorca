#!/usr/bin/env bash
# Latency gate of the review cut-over (M22b3, M-6 for review): an object move of simple review
# (OB1 next / previous / parent / first child / next in flow), the validity hash a step checks per
# node of the model (VD1) and a whole line-model build (FL1), on a 1000 and a 10000 node page,
# new words and classes (this tree) against the legacy ones (the tag legacy/review-last).
#
#   bench/m6/review-latency.sh [BASELINE_REF] [ROUNDS]
#
# The ignored release test `review::latency::gates_ob1_vd1_and_fl1_on_1k_and_10k_pages` runs in a
# throw-away worktree of BASELINE_REF and in this tree, alternately, ROUNDS times (default 5) on
# the same host, each in its own target dir. Gates, on the median over the rounds:
#   OB1, VD1: new p99 minus legacy p99 at most 100000 ns (0.1 ms), the keypress-to-speech budget
#             the cut-overs are held to, and the absolute p99 below 5 ms;
#   FL1:      a build runs off the key path in a task (published at 120 ms): new p99 below 120 ms
#             and at most 2 ms above the legacy p99.
# Run on a quiet host (bench/b1/quiet-host.sh); label the result with load and governor.
set -euo pipefail
here=$(cd "$(dirname "$0")/../.." && pwd)
base_ref=${1:-legacy/review-last}
rounds=${2:-5}
cd "$here"
. ./env.sh
export CARGO_INCREMENTAL=0
new_target=${CARGO_TARGET_DIR:-/var/tmp/gorca-shared-target}
old_target=/var/tmp/gorca-m6-baseline-target
wt=$(mktemp -d /var/tmp/gorca-m6-review-baseline.XXXXXX)
cleanup() { git -C "$here" worktree remove --force "$wt" >/dev/null 2>&1 || true; }
trap cleanup EXIT
git worktree add --detach "$wt" "$base_ref" >/dev/null
ln -s "$here/.toolchain" "$wt/.toolchain"
run() { # dir target
  (cd "$1" && CARGO_TARGET_DIR="$2" cargo test --release -p gorca-core --lib review::latency -- --ignored --nocapture 2>&1 | grep -E '^[0-9]+ nodes')
}
(cd "$wt" && CARGO_TARGET_DIR="$old_target" cargo test --release -p gorca-core --lib --no-run >/dev/null 2>&1)
(cd "$here" && CARGO_TARGET_DIR="$new_target" cargo test --release -p gorca-core --lib --no-run >/dev/null 2>&1)
echo "governor: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo n/a) load: $(cut -d' ' -f1-3 /proc/loadavg)"
out=$(mktemp -d)
for i in $(seq 1 "$rounds"); do
  run "$wt" "$old_target" >"$out/old.$i"
  run "$here" "$new_target" >"$out/new.$i"
  echo "round $i done"
done
python3 - "$out" "$rounds" <<'PY'
import re, statistics, sys
out, rounds = sys.argv[1], int(sys.argv[2])
unit = {"ns": 1, "µs": 1000, "us": 1000, "ms": 1_000_000, "s": 1_000_000_000}


def ns(s):
    m = re.match(r"([0-9.]+)(ns|µs|us|ms|s)$", s)
    return float(m.group(1)) * unit[m.group(2)]


R = re.compile(r"(\d+) nodes: (.+?): p50 \S+ p99 (\S+)")


def read(prefix):
    series = {}
    for i in range(1, rounds + 1):
        for line in open(f"{out}/{prefix}.{i}"):
            m = R.match(line.strip())
            if m:
                series.setdefault(f"{m.group(1)} {m.group(2)}", []).append(ns(m.group(3)))
    return series


old, new = read("old"), read("new")
bad = 0
print(f"{'series':44} {'legacy p99':>12} {'new p99':>12} {'delta':>10}")
for k in sorted(new):
    o = statistics.median(old.get(k, [float('nan')]))
    n = statistics.median(new[k])
    d = n - o
    if "FL1" in k:
        ok = d <= 2_000_000 and n < 120_000_000
    else:
        ok = d <= 100_000 and n < 5_000_000
    bad += not ok
    print(f"{k:44} {o:10.0f}ns {n:10.0f}ns {d:8.0f}ns {'ok' if ok else 'FAIL'}")
print("review latency:", "PASS" if not bad else f"FAIL ({bad} series)")
sys.exit(1 if bad else 0)
PY
