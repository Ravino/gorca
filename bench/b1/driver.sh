#!/usr/bin/env bash
# Per-slot driver used by `b1 run --driver bench/b1/driver.sh`.
#
# `b1 run` calls this once per (arm, run kind, repeat) with:
#   B1_ARM B1_DAEMON B1_RUN_KIND B1_REPEAT B1_RUN_DIR
# and expects B1_RUN_DIR to be filled as documented in crates/gorca-bench-b1/src/rundir.rs.
#
# * step : in-process policy microbench, run in the ARM's own worktree (it links the arm's
#          Candidate). The worktree is derived from B1_DAEMON=<tree>/target/release/<binary>.
# * walk|ff|idle|big|soak : the walk driver (H-3, test-engineer), a program given by
#          B1_WALK_DRIVER, run inside a private bus session (tests/harness/run-isolated.sh).
#          It receives the same B1_* variables and must write truth.jsonl, present.jsonl,
#          counters.json (and samples.json when it has tap-to-SPEAK timestamps).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
: "${B1_ARM:?}" "${B1_DAEMON:?}" "${B1_RUN_KIND:?}" "${B1_RUN_DIR:?}"
mkdir -p "$B1_RUN_DIR"
# `cargo bench` runs with the crate directory as cwd: a relative run directory would be written
# to the wrong place (arm A, finding 5). Everything below sees the absolute path.
B1_RUN_DIR="$(cd "$B1_RUN_DIR" && pwd)"
export B1_RUN_DIR
case "$B1_RUN_KIND" in
  step)
    tree="$(cd "$(dirname "$B1_DAEMON")/../.." && pwd)"
    cd "$tree"
    . ./env.sh
    # The harness crate is the same in every arm's tree; its adapter links that tree's
    # `gorca_mirror::fill::Candidate` (arms merge the harness commit, they never edit the bench).
    # GORCA_STEP_SUBJECT=noop gives the floor, spin:<us> the sensitivity check.
    GORCA_STEP_SUBJECT="${GORCA_STEP_SUBJECT:-candidate}" \
      cargo bench -p gorca-bench-b1 --bench step --offline -- ${B1_QUICK:+--quick}
    ;;
  walk|ff|idle|big|soak)
    : "${B1_WALK_DRIVER:?set B1_WALK_DRIVER to the H-3 walk driver binary (run-all.sh does)}"
    cd "$here"
    export GORCA_PASSTHROUGH="B1_ARM B1_DAEMON B1_RUN_KIND B1_REPEAT B1_RUN_DIR B1_SEED B1_SOAK_S"
    exec tests/harness/run-isolated.sh "$B1_WALK_DRIVER"
    ;;
  *)
    echo "driver.sh: unknown run kind $B1_RUN_KIND" >&2
    exit 64
    ;;
esac
