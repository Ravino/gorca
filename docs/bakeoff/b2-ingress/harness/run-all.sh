#!/usr/bin/env bash
# Run the whole B-2 harness against the code in the current worktree.
#
#   docs/bakeoff/b2-ingress/harness/run-all.sh <label> [--quick]
#
# 1. conformance: cargo test -p gorca-bench-b2 (ingress model, drain, real buses)
# 2. benchmarks: b2 bench (T1..T7, fixture self-test) on the mock bus and the private
#    dbus-daemon / dbus-broker, 3 repeats
# 3. fake apps: storm, mixed and hung inside tests/harness/run-isolated.sh
#
# Results: bench/results/b2-ingress/<label>/<UTC>.{json,txt} and ...-fakeapp-<scenario>.json.
set -euo pipefail
cd "$(dirname "$0")/../../../.."
. ./env.sh
label=${1:?usage: run-all.sh <label> [--quick]}
quick=${2:-}
out="bench/results/b2-ingress/$label"
mkdir -p "$out"

echo "== conformance (debug and release) =="
cargo test -p gorca-bench-b2 --offline
cargo test -p gorca-bench-b2 --offline --release
echo "== bounds incl. ignored (B-5 giant message: the zbus reference is expected to fail it) =="
cargo test -p gorca-bench-b2 --offline --release --test bounds -- --include-ignored --test-threads=1 || echo "B-5 or a bound FAILED (expected only for B-5 on the reference arm)"

echo "== build release =="
cargo build --release -p gorca-bench-b2 --offline

echo "== uptime before: $(cat /proc/loadavg) =="
if [ "$quick" = "--quick" ]; then
  target/release/b2 bench --label "$label" --quick
else
  target/release/b2 bench --label "$label"
fi

echo "== fake apps in the isolated AT-SPI harness =="
secs=15; [ "$quick" = "--quick" ] && secs=5
for s in storm mixed hung; do
  tests/harness/run-isolated.sh target/release/b2 fakeapp "$s" --secs "$secs" --label "$label"
done
echo "== done: $out =="
