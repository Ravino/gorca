#!/usr/bin/env bash
# Runs (arm x scenario x condition x run index) in randomized order.
# Usage: run-all.sh [--arms "a bprime"] [--condition quiet|loaded|dev] [--runs 5] [--scenarios "benign adv-a1 ..."]
#                   [--quick] [--out BASE] [--quiet-wait SECS]
#   --quick: smoke-sized overrides, for bring-up only (never a gating run).
#   --out BASE: parent of the per-run directory (default ../results).
#   --quiet-wait SECS: for `quiet`, how long to wait for the host precondition before each run (default 900).
#     When the wait times out the run still happens, labelled `quiet-precondition-timeout` (and its
#     meta.json/summary.json carry quiet_violated); it is kept and never replaces a clean run.
#
# Isolation (spec clarification C-2):
#  - every invocation writes into its own directory BASE/<UTC stamp>-<pid>/<arm>/ (one subdirectory per
#    arm), so two arms or two reruns can never overwrite each other's results, driver copy or log;
#  - the run log is BASE/<stamp>-<pid>/run-all.log, the binaries are copied there and executed from the
#    copy, so a cargo build in another worktree cannot swap the binary under a running matrix;
#  - a global lockfile (default /var/tmp/m18a0-run-all.lock, override M18A0_LOCK) holds the PID of the
#    running invocation; a second invocation refuses to start while that PID is alive, and a stale lock
#    (dead PID) is replaced.
# `quiet` waits for host/prepare.sh (loadavg1 < 1.5, governor performance, no cargo/rustc/e2e) before each run.
# Needs `. ./env.sh` first.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/var/tmp/gorca-shared-target}" CARGO_INCREMENTAL=0
arms="a bprime"
cond=dev
runs=1
quick=0
base="$here/../results"
quiet_wait=900
scen="benign adv-a1 adv-a2 adv-a3 adv-a4 adv-a5 adv-a6 adv-a13 adv-a7 adv-a8 adv-a12 adv-all adv-each-added keypress-onoff k1k2-path flap-focus storm-10k storm-50k memory-1-10-50 wq1 wq2 wq3 wq6-wake wq6-rows mq2-replay pf6-order pf1-differential"
while [ $# -gt 0 ]; do
  case "$1" in
    --arms) arms="$2"; shift 2 ;;
    --condition) cond="$2"; shift 2 ;;
    --runs) runs="$2"; shift 2 ;;
    --scenarios) scen="$2"; shift 2 ;;
    --out) base="$2"; shift 2 ;;
    --quiet-wait) quiet_wait="$2"; shift 2 ;;
    --quick) quick=1; shift ;;
    *) echo "bad arg $1"; exit 2 ;;
  esac
done

# --- single-instance lock: lockfile with a PID, liveness-checked --------------------------------------
lock="${M18A0_LOCK:-/var/tmp/m18a0-run-all.lock}"
acquire() {
  # noclobber makes the create atomic; on failure inspect the holder.
  if (set -o noclobber; echo "$$" > "$lock") 2> /dev/null; then return 0; fi
  holder="$(cat "$lock" 2> /dev/null || true)"
  if [[ "$holder" =~ ^[0-9]+$ ]] && kill -0 "$holder" 2> /dev/null; then
    echo "REFUSED: another m18a0 run-all.sh is active (pid $holder, lock $lock)" >&2
    return 1
  fi
  echo "stale lock (pid '${holder}' not alive), replacing" >&2
  rm -f "$lock"
  (set -o noclobber; echo "$$" > "$lock") 2> /dev/null || { echo "REFUSED: lost the lock race" >&2; return 1; }
}
acquire || exit 3
trap 'if [ "$(cat "$lock" 2> /dev/null)" = "$$" ]; then rm -f "$lock"; fi' EXIT

stamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
rundir="$base/$stamp"
mkdir -p "$rundir"
rundir="$(cd "$rundir" && pwd)"
exec > >(tee -a "$rundir/run-all.log") 2>&1
echo "run-all: pid $$ dir $rundir arms='$arms' condition=$cond runs=$runs quick=$quick"

(cd "$here" && cargo build --release -p m18a0-run -p m18a0-report -p m18a0-load)
[ -f "$here/build/guests/SHA256SUMS" ] || "$here/guests/build.sh"
mkdir -p "$rundir/bin"
for b in m18a0-run m18a0-report m18a0-load; do cp "$CARGO_TARGET_DIR/release/$b" "$rundir/bin/$b"; done
bin="$rundir/bin/m18a0-run"
for a in $arms; do mkdir -p "$rundir/$a"; done

extra=""
[ "$quick" = 1 ] && extra="--samples 3000 --warmup 300 --leg-s 2 --duration-s 3 --runs 500"
jobs=$(for a in $arms; do for s in $scen; do for i in $(seq "$runs"); do echo "$a $s"; done; done; done | shuf)
echo "$jobs" | while read -r a s; do
  label=""
  if [ "$cond" = quiet ]; then
    if ! "$here/host/prepare.sh" wait "$quiet_wait"; then
      echo "WARNING: quiet precondition not met within $quiet_wait s; run is labelled"
      label="--label quiet-precondition-timeout"
    fi
  fi
  # shellcheck disable=SC2086
  "$bin" run --arm "$a" --scenario "$here/scenarios/$s.toml" --condition "$cond" --out "$rundir/$a" $label $extra || echo "FAILED RUN KEPT: $a $s"
done
for a in $arms; do
  "$rundir/bin/m18a0-report" "$rundir/$a" --arm "$a" --code "$here/arms" --out "$rundir/$a/report.md" || echo "report for $a returned non-zero (gate verdict or NOT MEASURED)"
done
echo "done: $rundir"
