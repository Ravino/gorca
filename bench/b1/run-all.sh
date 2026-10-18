#!/usr/bin/env bash
# Run the B-1 harness for one or more arms, interleaved.
#
#   bench/b1/run-all.sh <label> --arm a:/path/to/gorca-daemon --arm b:/path/... [--arm r0:...]
#                       [--quick] [--plan walk=5,ff=5,idle=3,big=3,step=7] [--baseline R0.json]
#                       [--rundirs ROOT] [--no-fail]
#
# 1. unit + integration tests of the harness crate (verdicts, accounting, gating);
# 2. release build of `b1`;
# 3. `b1 run` with bench/b1/driver.sh as the per-slot driver (or --rundirs for recorded runs).
#
# B1_WALK_DRIVER defaults to target/release/b1-walk (built here).
# Results: bench/results/b1-mirror-fill/<label>/<UTC>.{json,txt}. `soak` is separate:
#   bench/b1/run-all.sh <label> --plan soak=1 --arm ...
# Quiet-host recipe (authoritative runs only): bench/b1/README.md.
set -euo pipefail
cd "$(dirname "$0")/../.."
. ./env.sh
label=${1:?usage: run-all.sh <label> --arm label:path ... [options]}
shift
quick=0
args=()
for a in "$@"; do
  if [ "$a" = "--quick" ]; then quick=1; else args+=("$a"); fi
done
echo "== host: load $(cat /proc/loadavg) governor $(sort -u /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor 2>/dev/null | tr '\n' ' ') =="
cargo test -p gorca-bench-b1 --offline
# Authoritative runs: the full +-20 % service-time fidelity check (H-10(a)). The result goes into
# the run's host meta (`fidelity`); a failure marks the run INVALID (see b1.rs).
export B1_STRICT=1
# The check measures sleep/wake accuracy of the fake app, so a single scheduler or cpuidle hiccup
# on a shared desktop can fail it (seen: one INVALID in three runs on a quiet host). Bounded retry:
# up to B1_FIDELITY_TRIES attempts (default 3); every attempt is logged and the run is valid only if
# one attempt passes cleanly; the verdict string lists how many attempts it took.
fid_tries=${B1_FIDELITY_TRIES:-3}
B1_FIDELITY="fail"
for ((fi = 1; fi <= fid_tries; fi++)); do
  if fid_out=$(cargo test -p b1-fakeapp --test fidelity --offline 2>&1); then
    if [ "$fi" = 1 ]; then B1_FIDELITY=pass; else B1_FIDELITY="pass (attempt $fi of $fid_tries)"; fi
    break
  fi
  echo "$fid_out" | tail -20
  B1_FIDELITY="fail: $(echo "$fid_out" | grep -m1 'model off' | tr -d '"' || true) (after $fi attempts)"
  echo "== fidelity attempt $fi failed${B1_FIDELITY:+: $B1_FIDELITY}"
  sleep 2
done
export B1_FIDELITY
echo "== fidelity: $B1_FIDELITY =="
cargo build --release -p gorca-bench-b1 --bins --offline
# The walk driver and the fake app (H-3, H-2), next to each other in target/release.
cargo build --release -p gorca-testkit --bin b1-walk --offline
cargo build --release -p b1-fakeapp --offline
export B1_WALK_DRIVER="${B1_WALK_DRIVER:-$PWD/target/release/b1-walk}"
plan=()
has_plan=0; has_rundirs=0
for a in "${args[@]}"; do
  [ "$a" = "--plan" ] && has_plan=1
  [ "$a" = "--rundirs" ] && has_rundirs=1
done
if [ "$quick" = 1 ] && [ "$has_plan" = 0 ]; then plan=(--plan walk=1,ff=1,idle=1,big=1,step=3); export B1_QUICK=1; fi
if [ "$has_rundirs" = 1 ]; then
  exec target/release/b1 run --label "$label" "${plan[@]}" "${args[@]}"
fi
exec target/release/b1 run --label "$label" --driver "$PWD/bench/b1/driver.sh" "${plan[@]}" "${args[@]}"
