#!/usr/bin/env bash
# Phase 2 acceptance gate run: every Phase 2 gate set, sequentially, on the code in this checkout
# with the shipped default topology (`auto`, resolved to lanes-rt through rtkit). One command, one
# summary table, non-zero exit on any failure. See bench/acceptance/README.md.
#
#   bench/acceptance/phase2.sh [--only m1,m6,...] [--skip ...] [--out DIR | --resume DIR]
#                              [--repeats N] [--quiet-host] [--force] [--dry-run] [--no-build]
#                              [--with-m9 | --without-m9] [--expect-topology ARM|none] [--list]
#
# Suites (run in this order): m1 m3 m4 m6 m7 m8 m9 storm x1 b1.
# Results: bench/results/acceptance/phase2-<UTC>/{host.txt,status.tsv,summary.md,<suite>/<step>.aN/}.
# Nothing is deleted or overwritten: a rerun of a failed step is attempt N+1 beside the old one.
# Everything runs in private sessions (tests/harness/run-isolated.sh and the M6/M7 compositor
# worlds); the live session is never touched and nothing is killed by name (only recorded PIDs).
set -uo pipefail
cd "$(dirname "$0")/../.."
ROOT=$(pwd)
. ./env.sh
export CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=4

ALL_SUITES=(m1 m3 m4 m6 m7 m8 m9 storm x1 b1)
only=""; skip=""; out=""; resume=0; repeats=3; quiet=0; QUIET_ON=0; force=0; dry=0; build=1; m9mode=auto
expect=lanes-rt; list=0

usage() { sed -n 2,14p "$0" | sed 's/^# \{0,1\}//'; }
while [ $# -gt 0 ]; do
  case "$1" in
    --only) only=$2; shift ;;
    --skip) skip=$2; shift ;;
    --out) out=$2; shift ;;
    --resume) out=$2; resume=1; shift ;;
    --repeats) repeats=$2; shift ;;
    --quiet-host) quiet=1 ;;
    --force) force=1 ;;
    --dry-run) dry=1 ;;
    --no-build) build=0 ;;
    --with-m9) m9mode=on ;;
    --without-m9) m9mode=off ;;
    --expect-topology) expect=$2; shift ;;
    --list) list=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "phase2: unknown option $1" >&2; usage >&2; exit 64 ;;
  esac
  shift
done

# ---- scenario sets -------------------------------------------------------------------------
M1_SET=K2,K2s,K3,K4,K4h,R1,R1s,RS
M3_SET=C1,C1h,C1hs,C2,C3,E1,E1s,E2,E2f,S2,SA1,SA2,SA3,SA3a,PW
M4_SET=A1,N1
M6_SET=V1,VT,VH,V2,V3,VP,VR,VF,VB,VW
M7_SET=B0,W1,BR1,BR2,W2,W2b,W3,FM1,FM2,FC1h,FC1,FE1,FE2,FS2,FSA1,FSA3,FCE1,FCE2,FT1,FP1,BSA1,BAR1,BL1,FR1
STORM_SET=F0,L1,L1s,L2,L3-10k,L3-50k,L4,L4c,K1,L2-50k,K1-50k

# Estimated minutes per step for the full run (3 repeats); see README for the basis.
est_m1=20; est_m3=25; est_m4=4; est_m6=50; est_m6v=6; est_m7=60; est_m8=45; est_m9=40
est_storm=12; est_x1=3; est_b1=20

# ---- suite selection -----------------------------------------------------------------------
m9_available() {
  [ -x tests/web/m9/run_m9.sh ] || return 1
  [ -n "${M9_CHROMIUM:-}" ] && [ -x "$M9_CHROMIUM" ] && return 0
  ls -d "$HOME"/.cache/ms-playwright/chromium-*/chrome-linux64/chrome >/dev/null 2>&1 && return 0
  command -v chromium >/dev/null 2>&1
}
m9_default() { [ -f docs/reports/m9-impl.md ] && m9_available; }  # the report marks the M9 merge

selected=()
if [ -n "$only" ]; then
  IFS=, read -r -a want <<<"$only"
  for s in "${ALL_SUITES[@]}"; do for w in "${want[@]}"; do [ "$s" = "$w" ] && selected+=("$s"); done; done
  for w in "${want[@]}"; do
    case " ${ALL_SUITES[*]} " in *" $w "*) ;; *) echo "phase2: unknown suite $w (have: ${ALL_SUITES[*]})" >&2; exit 64 ;; esac
  done
else
  for s in "${ALL_SUITES[@]}"; do
    if [ "$s" = m9 ]; then
      case $m9mode in on) ;; off) continue ;; auto) m9_default || continue ;; esac
    fi
    selected+=("$s")
  done
fi
if [ -n "$skip" ]; then
  IFS=, read -r -a drop <<<"$skip"
  keep=(); for s in "${selected[@]}"; do d=0; for x in "${drop[@]}"; do [ "$s" = "$x" ] && d=1; done; [ $d = 0 ] && keep+=("$s"); done
  selected=("${keep[@]}")
fi
[ ${#selected[@]} -gt 0 ] || { echo "phase2: no suite selected" >&2; exit 64; }

suite_est() { case $1 in m6) echo $((est_m6 + est_m6v));; *) eval "echo \$est_$1";; esac; }
if [ "$list" = 1 ]; then
  total=0
  echo "suite   est.min  contents (full run, $repeats repeats; estimates, see README)"
  for s in "${selected[@]}"; do
    e=$(suite_est "$s"); total=$((total + e))
    case $s in
      m1) c="e2e $M1_SET (M1+M4 strict)";; m3) c="e2e $M3_SET (M3 strict)";;
      m4) c="e2e $M4_SET (M4b strict)";; m6) c="e2e $M6_SET (fake VTE) + VV (real gnome-terminal)";;
      m7) c="Firefox via run_m7.sh: $M7_SET";; m8) c="Firefox via run_m8.sh: full corpus + perf gates";;
      m9) c="Chromium via run_m9.sh m7 + m8 (opt-in until M9 lands)";;
      storm) c="e2e $STORM_SET (L and K under the L3-50k storm)";;
      x1) c="extension hook overhead (x1 example, p99 <= 100 us, recovery < 50 ms)";;
      b1) c="B-1 judge quick session, arm main (bench/b1/run-all.sh --quick)";;
    esac
    printf '%-7s %7s  %s\n' "$s" "$e" "$c"
  done
  printf 'total   %7s  minutes (plus build, about 10 to 25 on a cold target dir)\n' "$total"
  exit 0
fi

# ---- output dir ----------------------------------------------------------------------------
ts=$(date -u +%Y%m%dT%H%M%SZ)
if [ -z "$out" ]; then
  tag=phase2; [ "$dry" = 1 ] && tag=phase2-dryrun
  out=bench/results/acceptance/$tag-$ts
fi
if [ "$resume" = 1 ]; then
  [ -d "$out" ] || { echo "phase2: --resume $out: no such directory" >&2; exit 64; }
elif [ -e "$out" ]; then
  echo "phase2: $out exists; results are never overwritten (use --resume $out)" >&2; exit 1
fi
mkdir -p "$out"
out=$(cd "$out" && pwd)
LOG=$out/phase2.log
exec > >(tee -a "$LOG") 2>&1

quick=""; [ "$dry" = 1 ] && { quick="--quick"; repeats=1; }
mode=ACCEPTANCE
[ "$dry" = 1 ] && mode="DRY RUN (not acceptance)"
[ "$force" = 1 ] && [ "$dry" = 0 ] && mode="ACCEPTANCE ATTEMPT, DEGRADED HOST (--force)"
[ ${#selected[@]} -lt ${#ALL_SUITES[@]} ] && mode="$mode, PARTIAL (${selected[*]})"

# ---- host state ----------------------------------------------------------------------------
gov() { sort -u /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor 2>/dev/null | tr '\n' ' '; }
psi() { grep "^$2" "/proc/pressure/$1" 2>/dev/null | sed -n 's/.*avg10=\([0-9.]*\).*/\1/p'; }
load1() { cut -d' ' -f1 /proc/loadavg; }
mem_avail_kb() { awk '/^MemAvailable:/ {print $2}' /proc/meminfo; }
rtprop() { busctl --system get-property org.freedesktop.RealtimeKit1 /org/freedesktop/RealtimeKit1 org.freedesktop.RealtimeKit1 "$1" 2>&1 | tr '\n' ' '; }
dirty=$(git status --porcelain --untracked-files=no | wc -l)

host_state() {
  echo "mode=$mode"
  echo "utc=$(date -u +%FT%TZ)"
  echo "git_rev=$(git rev-parse --short HEAD) dirty_tracked_files=$dirty branch=$(git rev-parse --abbrev-ref HEAD)"
  echo "kernel=$(uname -r) cpus=$(nproc)"
  echo "loadavg=$(cat /proc/loadavg)"
  echo "psi_memory_some_avg10=$(psi memory some) psi_memory_full_avg10=$(psi memory full)"
  echo "psi_cpu_some_avg10=$(psi cpu some) psi_io_some_avg10=$(psi io some)"
  echo "mem_available_mb=$(( $(mem_avail_kb) / 1024 ))"
  echo "governor=$(gov)"
  echo "rtkit MaxRealtimePriority: $(rtprop MaxRealtimePriority)"
  echo "rtkit RTTimeUSecMax: $(rtprop RTTimeUSecMax)"
  echo "topology: shipped default (--topology=auto); expected arm: $expect (gated by e2e --expect-topology)"
  echo "repeats=$repeats quick=${quick:-no} suites=${selected[*]}"
  echo "quiet-host:"; bench/b1/quiet-host.sh status 2>&1 | sed 's/^/  /'
}
{ [ "$resume" = 1 ] && echo "---- resumed ----"; host_state; } >>"$out/host.txt"
echo "== phase2 acceptance: $mode"
cat "$out/host.txt"

# ---- preflight -----------------------------------------------------------------------------
problems=()
l=$(load1)
awk -v l="$l" 'BEGIN { exit !(l > 4) }' && problems+=("load average $l > 4")
m_some=$(psi memory some); m_full=$(psi memory full)
awk -v a="${m_some:-0}" -v b="${m_full:-0}" 'BEGIN { exit !(a > 10 || b > 2) }' \
  && problems+=("memory pressure high (some avg10 ${m_some}, full avg10 ${m_full})")
[ "$(mem_avail_kb)" -lt $((6 * 1024 * 1024)) ] && problems+=("MemAvailable below 6 GB")
c_some=$(psi cpu some)
awk -v a="${c_some:-0}" 'BEGIN { exit !(a > 25) }' && echo "WARNING: cpu pressure some avg10 ${c_some} is high"
if [ ${#problems[@]} -gt 0 ]; then
  echo "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
  for p in "${problems[@]}"; do echo "!! HOST NOT QUIET: $p"; done
  if [ "$dry" = 1 ]; then echo "!! dry run: continuing, results are NOT acceptance data"
  elif [ "$force" = 1 ]; then echo "!! --force: continuing, summary is marked DEGRADED"
  else echo "!! refusing to start (pause the other agents, or --force / --dry-run)"; echo "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"; exit 2; fi
  echo "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
  printf '%s\n' "${problems[@]}" >>"$out/preflight-problems.txt"
fi
if [ "$expect" != none ] && ! busctl --system get-property org.freedesktop.RealtimeKit1 /org/freedesktop/RealtimeKit1 org.freedesktop.RealtimeKit1 MaxRealtimePriority >/dev/null 2>&1; then
  echo "!! rtkit unreachable: the default topology cannot resolve to $expect (e2e would fail with exit 3)"
  [ "$dry" = 1 ] || [ "$force" = 1 ] || exit 2
fi

# ---- host tuning (optional) ----------------------------------------------------------------
if [ "$quiet" = 1 ]; then
  sudo bench/b1/quiet-host.sh on >>"$out/quiet-host.txt" 2>&1 || echo "WARNING: quiet-host on failed (see quiet-host.txt)"
  QUIET_ON=1
  { echo "after quiet-host on:"; bench/b1/quiet-host.sh status; } >>"$out/host.txt" 2>&1
fi

# ---- build, once ---------------------------------------------------------------------------
# Resume across commits: the binaries are rebuilt only when a product path changed since the rev
# they were built from. Commits touching only bench/, docs/, tests/ or .claude/ (result commits,
# runner fixes) neither force a rebuild nor trip the stale-reader check (e2e reads
# GORCA_E2E_ALLOW_STALE_READER); the rev each attempt ran against is in meta.txt (reader_rev, git_rev).
READER_REV=$("$ROOT/target/release/gorca" --version 2>/dev/null | awk '{print $2}')
if [ "$build" = 1 ] && [ "$resume" = 1 ] && [ -n "$READER_REV" ] \
   && git rev-parse -q --verify "$READER_REV^{commit}" >/dev/null; then
  changed=$(git diff --name-only "$READER_REV" HEAD -- . ':!bench' ':!docs' ':!tests' ':!.claude' | wc -l)
  if [ "$changed" = 0 ]; then
    echo "== resume: only bench/docs/tests/.claude changed since reader rev $READER_REV; no rebuild, stale-reader check relaxed"
    build=0
    export GORCA_E2E_ALLOW_STALE_READER=1
  fi
fi
if [ "$build" = 1 ]; then
  if [ "$(mem_avail_kb)" -lt $((6 * 1024 * 1024)) ] && [ "$force" = 0 ] && [ "$dry" = 0 ]; then
    echo "phase2: less than 6 GB available, refusing to build" >&2; exit 2
  fi
  other=$(ps -eo comm | grep -cE '^(cargo|rustc)$' || true)
  [ "${other:-0}" -gt 0 ] && echo "WARNING: $other other cargo/rustc processes are running (one cargo at a time)"
  echo "== build (release, CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS)"
  {
    cargo build --release -p gorca-bench --bins -p gorca-daemon --offline &&
    cargo xtask build-ext &&
    cargo build -p ext-test --target wasm32-wasip2 --release --target-dir target/ext-wasm --offline &&
    cargo build --release -p gorca-ext-host --example x1 --offline
  } >"$out/build.log" 2>&1 || { echo "phase2: build failed, see $out/build.log"; tail -20 "$out/build.log"; exit 2; }
fi
for b in target/release/gorca target/release/e2e target/release/e2e-m8; do
  [ -x "$b" ] || { echo "phase2: missing $b (build first)" >&2; exit 2; }
done
READER_REV=$("$ROOT/target/release/gorca" --version 2>/dev/null | awk '{print $2}')
echo "git_rev_built=$(git rev-parse --short HEAD) reader_rev=$READER_REV binary=$(sha256sum target/release/gorca | cut -c1-16) mtime=$(stat -c %y target/release/gorca)" >>"$out/host.txt"

# ---- step runner ---------------------------------------------------------------------------
STATUS=$out/status.tsv
touch "$STATUS"
DEV_READER="daemon:$ROOT/target/release/gorca,--impersonate-orca"
# Common e2e options; the wrappers (run_m7/m8/m9) bring their own --reader and --reader-env.
E2E_REPEAT=(--repeats "$repeats")
[ -n "$quick" ] && E2E_REPEAT+=("$quick")
TOPO=()
[ "$expect" != none ] && TOPO=(--expect-topology "$expect")
flags() { local f=() n; for n in "$@"; do f+=(--reader-env "GORCA_$n=1" --reader-env "GORCA_${n}_STRICT=1"); done; printf '%s\n' "${f[@]}"; }

step_ok() { grep -qP "^$1\t$2\t[0-9]+\t[0-9]+\tok$" "$STATUS"; }

sampler_pid=""
start_sampler() { # file
  ( while :; do echo -e "$(date +%s)\t$(load1)\t$(psi memory some)\t$(psi cpu some)"; sleep 5; done ) >"$1" &
  sampler_pid=$!
}
stop_sampler() { [ -n "$sampler_pid" ] && { kill "$sampler_pid" 2>/dev/null; wait "$sampler_pid" 2>/dev/null; sampler_pid=""; }; }
cleanup_all() {
  stop_sampler
  if [ "$QUIET_ON" = 1 ]; then
    QUIET_ON=0
    sudo bench/b1/quiet-host.sh off >>"$out/quiet-host.txt" 2>&1; echo "quiet-host restored"
  fi
}
trap cleanup_all EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

failed_steps=()
# run_step <suite> <step> <cap-seconds> <kind> <builder-function>
# The builder fills CMD (and may set STEP_ENV for exported variables) using $DIR and $LABEL.
run_step() {
  local suite=$1 step=$2 cap=$3 kind=$4 builder=$5
  if [ "$resume" = 1 ] && step_ok "$suite" "$step"; then echo "== $suite/$step: already ok, skipped (resume)"; return 0; fi
  local n=1; while [ -e "$out/$suite/$step.a$n" ]; do n=$((n + 1)); done
  DIR=$out/$suite/$step.a$n; LABEL=acc-$ts-$suite-$step-a$n
  mkdir -p "$DIR"
  CMD=(); STEP_ENV=()
  "$builder"
  local t0 l0; t0=$(date +%s); l0=$(cat /proc/loadavg)
  echo "== $suite/$step attempt $n  load $l0  cap ${cap}s"
  echo "   ${CMD[*]}" >"$DIR/cmd.txt"
  start_sampler "$DIR/load.tsv"
  local rc=0
  ( cd "$ROOT"; for kv in "${STEP_ENV[@]}"; do export "$kv"; done
    exec timeout --kill-after=60 "$cap" "${CMD[@]}" ) </dev/null >"$DIR/stdout.txt" 2>"$DIR/stderr.txt" || rc=$?
  stop_sampler
  local t1 res=ok; t1=$(date +%s)
  [ "$rc" = 0 ] || res=FAIL
  {
    echo "suite=$suite"; echo "step=$step"; echo "attempt=$n"; echo "kind=$kind"; echo "exit=$rc"
    echo "label=$LABEL"; echo "start=$t0"; echo "end=$t1"; echo "seconds=$((t1 - t0))"
    echo "load_start=$l0"; echo "load_end=$(cat /proc/loadavg)"; echo "governor=$(gov)"
    echo "git_rev=$(git rev-parse --short HEAD)"; echo "reader_rev=$READER_REV"; echo "cap_seconds=$cap"
  } >"$DIR/meta.txt"
  printf '%s\t%s\t%s\t%s\t%s\n' "$suite" "$step" "$n" "$rc" "$res" >>"$STATUS"
  echo "   -> $res (exit $rc, $((t1 - t0)) s)"
  [ "$res" = ok ] || { failed_steps+=("$suite/$step"); tail -5 "$DIR/stderr.txt" | sed 's/^/   stderr: /'; }
  return 0
}

# ---- step builders -------------------------------------------------------------------------
e2e_direct() { # <only-set> <env-flag-names...>   builds CMD for a direct e2e run in a private session
  local set=$1; shift
  local fl; mapfile -t fl < <(flags "$@")
  CMD=(tests/harness/run-isolated.sh target/release/e2e run --label "$LABEL" --only "$set"
       --reader "$DEV_READER" "${E2E_REPEAT[@]}" "${TOPO[@]}" "${fl[@]}" --reader-env NO_COLOR=1
       --out "$DIR/res")
  STEP_ENV=("GORCA_PASSTHROUGH=GORCA_E2E_READER_LOG" "GORCA_E2E_READER_LOG=$DIR/reader.log")
}
b_m1() { e2e_direct "$M1_SET" M1 M4; }
b_m3() { e2e_direct "$M3_SET" M3; }
b_m4() { e2e_direct "$M4_SET" M1 M4 M4B; }
b_m6() { e2e_direct "$M6_SET" M6; }
b_storm() { e2e_direct "$STORM_SET" M1; }
b_m6v() {
  local fl; mapfile -t fl < <(flags M6)
  CMD=(tests/term/run_real_vte.sh target/release/e2e run --label "$LABEL" --only VV --reader "$DEV_READER"
       "${E2E_REPEAT[@]}" "${TOPO[@]}" "${fl[@]}" --reader-env GORCA_M6_REAL=1 --reader-env NO_COLOR=1
       --out "$DIR/res")
  STEP_ENV=("GORCA_PASSTHROUGH=GORCA_E2E_READER_LOG" "GORCA_E2E_READER_LOG=$DIR/reader.log")
}
b_m7() {
  CMD=(tests/web/m7/run_m7.sh "$LABEL" --only "$M7_SET" "${E2E_REPEAT[@]}" "${TOPO[@]}"
       --reader-env GORCA_M7_STRICT=1 --out "$DIR/res")
}
b_m8() { CMD=(tests/web/m8/run_m8.sh "$LABEL" "${E2E_REPEAT[@]}" --out "$DIR/res"); }
b_m9_m7() {
  CMD=(tests/web/m9/run_m9.sh m7 "$LABEL" --only "$M7_SET" "${E2E_REPEAT[@]}" "${TOPO[@]}"
       --reader-env GORCA_M7_STRICT=1 --out "$DIR/res")
}
b_m9_m8() { CMD=(tests/web/m9/run_m9.sh m8 "$LABEL" "${E2E_REPEAT[@]}" --out "$DIR/res"); }
b_x1() {
  local n=100000; [ -n "$quick" ] && n=20000
  CMD=(bash -c 'for i in $(seq 1 '"$repeats"'); do target/release/examples/x1 '"$n"' >"$0/x1-run$i.tsv" 2>"$0/x1-run$i.stderr" || exit $?; done' "$DIR")
}
b_b1() {
  CMD=(bench/b1/run-all.sh "$LABEL" --arm "main:$ROOT/target/release/gorca:$(git rev-parse --short HEAD)"
       --out "$DIR/res" --quick)
}

# ---- run -----------------------------------------------------------------------------------
t_start=$(date +%s)
for s in "${selected[@]}"; do
  echo; echo "######## suite $s"
  case $s in
    m1) run_step m1 e2e 5400 e2e b_m1 ;;
    m3) run_step m3 e2e 5400 e2e b_m3 ;;
    m4) run_step m4 e2e 2400 e2e b_m4 ;;
    m6) run_step m6 fake 7200 e2e b_m6
        run_step m6 vv 3600 e2e b_m6v ;;
    m7) run_step m7 firefox 7200 e2e b_m7 ;;
    m8) run_step m8 firefox 9000 e2e b_m8 ;;
    m9) run_step m9 chromium-m7 7200 e2e b_m9_m7
        run_step m9 chromium-m8 9000 e2e b_m9_m8 ;;
    storm) run_step storm e2e 5400 e2e b_storm ;;
    x1) run_step x1 hooks 1800 x1 b_x1 ;;
    b1) run_step b1 judge-quick 5400 b1 b_b1 ;;
  esac
done
t_end=$(date +%s)
echo "loadavg_end=$(cat /proc/loadavg) elapsed_s=$((t_end - t_start))" >>"$out/host.txt"

# ---- summary -------------------------------------------------------------------------------
python3 -I bench/acceptance/summarize.py "$out" --mode "$mode" --suites "${selected[*]}" \
  --all-suites "${ALL_SUITES[*]}"
rc=$?
echo "summary: $out/summary.md (exit $rc)"
exit $rc
