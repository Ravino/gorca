# Phase 2 acceptance run

One command runs every Phase 2 gate set sequentially against the code in this checkout, with the
shipped default topology (`--topology=auto`, which resolves to `lanes-rt` through rtkit), and writes
one summary table. Background: `docs/decisions.md` (2026-10-16): latency gates are unreliable at the
usual host load (25-50), so acceptance is a dedicated low-load window with all agents paused.

```sh
bench/acceptance/phase2.sh                    # the full run, 3 repeats, refuses on a busy host
bench/acceptance/phase2.sh --list             # suites, contents, runtime estimate
bench/acceptance/phase2.sh --only m6,m8       # a subset (the summary says PARTIAL)
bench/acceptance/phase2.sh --resume bench/results/acceptance/phase2-<UTC>   # finish what failed or never ran
bench/acceptance/phase2.sh --dry-run --only m4,x1   # quick, 1 repeat, labelled NOT acceptance
```

Options: `--skip a,b`, `--repeats N`, `--quiet-host` (runs `sudo bench/b1/quiet-host.sh on` first and
`off` on exit, so the governor and C-states are put back), `--force` (run on a busy host; the
summary is marked DEGRADED), `--no-build`, `--with-m9` / `--without-m9`, `--expect-topology ARM|none`
(default `lanes-rt`; the e2e harness fails with exit 3 when a reader resolves to another arm).

## Procedure for the real window

1. Pause every agent and every other cargo/test job. `uptime` should show load below 2.
2. Optionally `--quiet-host` (performance governor, deep C-states off; needs sudo, reverted on exit).
3. `bench/acceptance/phase2.sh` from `main` with a clean tree (the host record has the git rev and
   dirty-file count; run it from a checkout of `main`, not a worktree with local edits).
4. Read `summary.md` in the printed directory. Exit status is non-zero on any failed step, FAIL gate
   row, skipped scenario, or suite that did not run.

## Preflight

The run records load, `/proc/pressure/{memory,cpu,io}`, MemAvailable, governor, idle states, rtkit
limits, git revision and dirty flag in `host.txt`, then **refuses to start** (exit 2) when load1 > 4,
memory PSI is high (`some avg10` > 10 or `full avg10` > 2), MemAvailable < 6 GB, or rtkit is not
reachable. `--force` or `--dry-run` continue with a loud banner and a marked summary. High CPU PSI
(> 25) only warns. The build refuses below 6 GB free and warns when other cargo/rustc processes run
(one cargo at a time, `CARGO_BUILD_JOBS=4`). Every step also records a 5-second sample of load and
PSI (`load.tsv`), and the summary shows the load range during each step.

## Suites

| suite | what | gates |
|---|---|---|
| `m1` | e2e `K2 K2s K3 K4 K4h R1 R1s RS`, `GORCA_M1`/`GORCA_M4` strict (R1/R1s/RS, the M4a review gates, live here) | K2/K3/R1/R1s p99, lost, not_consumed, K4 counts, RS failures |
| `m3` | e2e `C1 C1h C1hs C2 C3 E1 E1s E2 E2f S2 SA1 SA2 SA3 SA3a PW`, `GORCA_M3` strict | per `bench/e2e/README.md` |
| `m4` | e2e `A1 N1`, `GORCA_M4B` strict | A1/N1 p99, a1_wrong, n1_interrupted |
| `m6` | step `fake`: `V1 VT VH V2 V3 VP VR VF VB VW` (strict); step `vv`: `VV` against a real gnome-terminal-server (`tests/term/run_real_vte.sh`) | per README |
| `m7` | `tests/web/m7/run_m7.sh`: Firefox `B0 W1 BR1 BR2 W2 W2b W3 FM1 FM2` + `FC1h FC1 FE1 FE2 FS2 FSA1 FSA3 FCE1 FCE2 FT1 FP1 BSA1 BAR1 BL1 FR1`, `GORCA_M7_STRICT` | per README |
| `m8` | `tests/web/m8/run_m8.sh`: the full NVDA-parity corpus, steps and perf gates (XFAIL/GAP details in `res/<UTC>-steps.md`) | rows of `e2e-m8` |
| `m9` | `tests/web/m9/run_m9.sh m7` and `m8` against Chromium. **Opt-in**: default-on only when `docs/reports/m9-impl.md` exists (the M9 merge marker) and a Chromium is found; force with `--with-m9` | as m7/m8 |
| `storm` | e2e `F0 L1 L1s L2 L3-10k L3-50k L4 L4c K1 L2-50k K1-50k`: L and K scenarios under the 10k/50k storms | L1/L3/L4/K1 p99, L2 p99, lost, superseded run, storm rate |
| `x1` | `x1` example (extension hook overhead) x repeats | hook p99 <= 100 us, trap/loop recovery p99 < 50 ms, on the worst repeat |
| `b1` | `bench/b1/run-all.sh --quick` with one arm, `main` (the shipped daemon); includes the harness fidelity check | B-1 rows. Rows that need an R0 baseline (`B1-L1c.vs_r0`) have none here and can FAIL: judge those by hand |

Every e2e gate is evaluated by the harness on the **median over repeats** (counts that must be zero
on the worst repeat); the summary shows that median and the worst repeat's `max`. A repeat that
disturbs a gate shows in `max` even when the median passes. `*.skipped` rows (a scenario that did
not run) count as failures.

Each step is a child process under `timeout` (TERM, then KILL after 60 s, only that PID); the
private sessions are torn down by their own scripts. Nothing is killed by name, and nothing touches
the live session.

## Results layout

```
bench/results/acceptance/phase2-<UTC>/
  host.txt          host record (and the quiet-host state)
  status.tsv        suite, step, attempt, exit, ok|FAIL per attempt
  summary.md        the table: step results, failing gates, every gated row with load
  phase2.log        everything printed
  build.log         the release build
  <suite>/<step>.aN/{cmd.txt,meta.txt,load.tsv,stdout.txt,stderr.txt,reader.log,res/}
```

A failed step is never overwritten: `--resume` runs the failed or missing steps again as attempt
N+1 beside the old one, and the summary reads the latest attempt (earlier attempts are listed in the
notes column). Steps already `ok` are skipped.

## Runtime estimate (3 repeats, quiet host)

`phase2.sh --list` prints it: about **4.1 h** for m1 m3 m4 m6 m7 m8 storm x1 b1 (245 min), about
4.8 h with m9, plus the build (10-25 min on a cold target directory). Calibration: the dry runs
(quick size, 1 repeat, load 11-33) took m1 44 s, m4 104 s, m6 fake 368 s, m6 vv 99 s, m7 592 s,
x1 13 s (20,000 iterations); the full size is 8x the probes (K4 is 10,000 mode switches, about 3 min
per repeat; E2 and VT are 10,000 actions) times 3 repeats, so the figures are estimates, and m3, m8,
m9, storm and b1 are not calibrated. After the real window, replace the `est_*` lines in
`phase2.sh` with the measured seconds in each step's `meta.txt`.

## Dry runs

`--dry-run` uses `--quick --repeats 1`, continues on a busy host and labels the summary
`DRY RUN, NOT ACCEPTANCE DATA`. Dry-run directories are named `phase2-dryrun-*` and carry the
host load, which is usually 10-50 on the shared development host, so a failing latency gate in a
dry run means nothing.
