#!/usr/bin/env bash
# M-6 gate of the focus cut-over (M22b1): Reader::handle(FocusChanged) on a cache hit, new
# presenter (this tree) against the legacy presenter (the tag legacy/focus-last).
#
#   bench/m6/focus-latency.sh [BASELINE_REF] [ROUNDS]
#
# The bench is the `m6_focus_latency` test in crates/gorca-core/src/reader.rs. The block between
# its BENCH markers is copied into a throw-away worktree of BASELINE_REF, built in its own target
# dir under /var/tmp, and both trees are run alternately ROUNDS times (default 5) on the same
# host. Gate: median p99 of the new tree minus median p99 of the baseline <= 100000 ns (0.1 ms).
# Run on a quiet host (bench/b1/quiet-host.sh); label the result with load and governor.
set -euo pipefail
here=$(cd "$(dirname "$0")/../.." && pwd)
base_ref=${1:-legacy/focus-last}
rounds=${2:-5}
cd "$here"
. ./env.sh
export CARGO_INCREMENTAL=0
new_target=${CARGO_TARGET_DIR:-/var/tmp/gorca-shared-target}
old_target=/var/tmp/gorca-m6-baseline-target
wt=$(mktemp -d /var/tmp/gorca-m6-baseline.XXXXXX)
cleanup() { git -C "$here" worktree remove --force "$wt" >/dev/null 2>&1 || true; }
trap cleanup EXIT
git worktree add --detach "$wt" "$base_ref" >/dev/null
ln -s "$here/.toolchain" "$wt/.toolchain"
python3 - "$here/crates/gorca-core/src/reader.rs" "$wt/crates/gorca-core/src/reader.rs" <<'PY'
import sys
src, dst = sys.argv[1], sys.argv[2]
s = open(src).read()
a = s.index("    // BENCH-BEGIN")
b = s.index("    // BENCH-END") + len("    // BENCH-END\n")
blk = s[a:b]
d = open(dst).read()
anchor = "    #[test]\n    fn cache_hit_focus_speaks_immediately_with_the_new_epoch() {"
assert anchor in d, "anchor not found in the baseline reader.rs"
open(dst, "w").write(d.replace(anchor, blk + "\n" + anchor, 1))
PY
run() { # dir target
  (cd "$1" && CARGO_TARGET_DIR="$2" cargo test --release -p gorca-core --lib m6_focus_latency -- --ignored --nocapture 2>&1 | grep '^M6 ')
}
# Build both once before the timed runs.
(cd "$wt" && CARGO_TARGET_DIR="$old_target" cargo test --release -p gorca-core --lib --no-run >/dev/null 2>&1)
(cd "$here" && CARGO_TARGET_DIR="$new_target" cargo test --release -p gorca-core --lib --no-run >/dev/null 2>&1)
echo "governor: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo n/a) load: $(cut -d' ' -f1-3 /proc/loadavg)"
news=()
olds=()
for i in $(seq 1 "$rounds"); do
  o=$(run "$wt" "$old_target")
  n=$(run "$here" "$new_target")
  echo "round $i legacy: $o"
  echo "round $i new:    $n"
  olds+=("$(sed 's/.*p99_ns=\([0-9]*\).*/\1/' <<<"$o")")
  news+=("$(sed 's/.*p99_ns=\([0-9]*\).*/\1/' <<<"$n")")
done
med() { printf '%s\n' "$@" | sort -n | sed -n "$((($# + 1) / 2))p"; }
mo=$(med "${olds[@]}")
mn=$(med "${news[@]}")
delta=$((mn - mo))
echo "median p99 legacy=${mo}ns new=${mn}ns delta=${delta}ns (gate <= 100000ns)"
if [ "$delta" -le 100000 ]; then echo "M-6 PASS"; else echo "M-6 FAIL"; exit 1; fi
