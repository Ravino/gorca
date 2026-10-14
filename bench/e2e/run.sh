#!/usr/bin/env bash
# Run the Phase 1 end-to-end latency harness against the code in the current worktree.
#
#   bench/e2e/run.sh <label> [--quick] [--reader stub|daemon:/path/to/gorca-daemon[,arg...]]
#                    [--backend voicego|ssip] [--only L1,L2,...] [--repeats N] [--no-fail]
#
# 1. unit tests of the harness (mock servers, correlation)
# 2. release build of the harness binaries
# 3. `e2e run` inside tests/harness/run-isolated.sh (private session + a11y bus, never the real ones)
#
# Results: bench/results/e2e/<label>/<UTC>.{json,txt}. Compare runs with bench/e2e/compare.py.
set -euo pipefail
cd "$(dirname "$0")/../.."
. ./env.sh
label=${1:?usage: run.sh <label> [--quick] [e2e options]}
shift
echo "== host: load $(cat /proc/loadavg) =="
cargo test -p gorca-bench --lib e2e --offline
cargo build --release -p gorca-bench --bins -p gorca-daemon --offline
exec tests/harness/run-isolated.sh target/release/e2e run --label "$label" "$@"
