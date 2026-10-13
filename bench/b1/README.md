# B-1 harness (mirror fill bake-off): runner, gates, R0

Spec: `docs/bakeoff/b1-mirror-fill/SPEC.md` (sections 6, 8.3, 9, 11, 12, 14). Code:
`crates/gorca-bench-b1` (verdicts H-4, message accounting H-5, runner H-6, step bench H-7,
host meta H-12). The walk driver and the fake apps (H-2, H-3) are a different share; they plug in
through the **run-directory contract** below.

```text
bench/b1/run-all.sh <label> --arm a:/path/gorca-daemon[:rev] --arm b:... [--quick] [--plan ...]
bench/b1/run-r0.sh                       # the R0 baseline (SPEC 12)
python3 bench/b1/compare.py bench/results/b1-mirror-fill/*/<UTC>.json
target/release/b1 analyze walk <run-dir> # metrics of one recorded run
target/release/b1 limits-md              # SPEC 9 table from limits.rs
```

## How a run is judged

* Repeats are **interleaved**: for every repeat the arms run round-robin, rotated by one each
  round (`runner::schedule`), so no arm always runs first.
* Each run yields one value per gate; a gate reads the **median over repeats** (min..max reported
  in the row). Counts that must be zero (`B1-STALE`, `B1-LOST`, `B1-L3.lost`) read the **worst
  repeat**. A run whose scripted counts were not reached, whose signal counts disagree by > 1 %, or
  whose directory is missing is **invalid**: excluded from medians and it fails `B1-RUNS.invalid`.
* A bold gate with no value at all is a FAIL, not a silent pass.
* `B1-L1c.vs_r0` (cold path <= 1.2 x R0) needs `--baseline <r0 result.json> --r0 r0`.
* Exit code 1 if any row fails (`--no-fail` to still get the tables).

## Run-directory contract

One directory per (arm, kind, repeat): `<root>/<arm>/<kind>/<repeat>/`. Files and keys are
documented in `crates/gorca-bench-b1/src/rundir.rs`; log line shapes in `src/verdict.rs`.
Summary: `truth.jsonl` (fake apps, CLOCK_MONOTONIC ns), `present.jsonl` (PW-10), `counters.json`
(fake-app call counters per app/member, drain `signals_received`, monitor `signals_monitored`,
L3/idle/RSS scalars, `scripted`/`actual`), optional `samples.json` (tap-to-SPEAK ms; without it
latencies are derived as `t_speak - t_stimulus`, which includes bus transit, ~0.2 ms floor).
Paths are the join key between the two logs, so the fake apps need disjoint path spaces (or an
`app` equal to `app_bus` and `KeyMode::AppAndPath`).

## D-Bus message counting (H-5)

Debian 13's `dbus-daemon` (1.16.2) **does** expose `org.freedesktop.DBus.Debug.Stats`, but it is
not usable for traffic counts: `GetStats` is memory/connection gauges and in
`GetConnectionStats` the "message" keys are the current queue depth (0 on an idle bus), not
totals (test `debug_stats_exists_but_connection_counters_are_gauges_not_totals`). dbus-broker has
no Stats interface. So: `msgs::Monitor` (`BecomeMonitor`) on the private bus counts calls, replies
and signals per sender, destination and member; started with gorca's own match rules it counts
exactly the signals gorca receives (cross-check vs `DrainStats.messages`, 1 %); with an empty rule
set it counts everything (idle calls/s, calls to the `storm` app). `msgs = 2 * calls + signals`.
`b1 bus-probe` prints what the bus offers.

## Step bench (H-7)

`cargo bench -p gorca-bench-b1 --bench step` (7 rounds, worst step per round, median gated at
250 us). The inputs (65 536 nodes, dirty-all, 4 096-event batch, single event, tick) are generated
by `step::scenarios`; the subject is a trait. Until an arm's `Candidate` exists it runs the noop
floor (`GORCA_STEP_SUBJECT=noop`) or a deliberate overrun (`spin:400`, sensitivity: must FAIL).
Wiring: one `impl StepSubject` in `benches/step.rs` (see the header there).

## R0 (H-9)

`run-r0.sh`: arm `r0`, `NullPolicy` build from the commit with PW-1..PW-12, plan
`step=7,walk=5,ff=5,idle=3,big=3`; results in `bench/results/b1-mirror-fill/r0/`. Paste SPEC
section 12 from `b1 baseline-md <result.json> r0`. Not runnable end to end before the walk driver
(H-3), PW-3/PW-10 and the fake apps are on the branch.

## Quiet-host recipe (H-12; authoritative runs and the reviewer's re-run)

A run on a loaded host is informative only; `meta.quiet_host` records which.

```bash
uptime                                   # load average must be < 1 before starting
sudo cpupower frequency-set -g performance   # user runs it (sudo needs a password)
# pin the harness and the daemons to a core set; leave the rest for the fake apps and the bus:
taskset -c 2-9 bench/b1/run-all.sh <label> --arm ...
# stop other desktop load for the duration (browser, indexers); do not run two harnesses at once
```

The result meta holds: load average at start/end, governor per cpu, `cpus` and `cpus_allowed`,
kernel, `dbus-daemon` version, build profile, harness git rev (+dirty flag) and the git rev of
every arm (pass `label:path:rev`).
