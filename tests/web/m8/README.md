# tests/web/m8: Firefox browse-mode parity corpus and gate (M8 prep)

Spec: `docs/plan/m8-spec.md`. Baseline against `main`: `docs/reports/m8-baseline.md`.

```text
tests/web/m8/run_m8.sh <label> [--quick] [--repeats N] [--only M8-QN,M8-TB,...] [--no-fail]
tests/web/m8/run_m8.sh --list          every scenario and step, no rig
tests/web/m8/run_m8.sh --probe <cmd>   run <cmd> inside the same private sessions
GORCA_M8_STEPS=QN.h,QN.k  ...          only the steps whose id starts with one of these
GORCA_M8_DUMP=1  ...                   keep speech and bus tails of every failing step
```

Everything runs in private sessions (the M7 pattern): a private dbus plus a headless gnome-shell
that gives Firefox a display, and a private AT-SPI session with the daemon, the emulated Mutter,
the mock speech server and a throw-away-profile Firefox. Nothing touches the live session. Results
go to `bench/results/e2e/<label>/` as `<UTC>.json`, `<UTC>.txt` (gate rows) and `<UTC>-steps.md`
(one row per step, status per repeat, latency, what was heard).

| Path | What |
|---|---|
| `pages/*.html` | the corpus, one page per matrix area plus real-world shaped pages (title prefix `M8`) |
| `pages/frames/` | the nested and form frames used by `iframes.html` |
| `gen_m8.py` | generates `pages/generated/` (1000x12 table, 100k-node page, long document); git-ignored, produced on first use |
| `run_m8.sh` | the runner (copy of `tests/web/m7/run_m7.sh` with the M8 binary and flags) |
| `crates/gorca-bench/src/e2e/m8.rs` | judge and runner |
| `crates/gorca-bench/src/e2e/m8_steps.rs` | the scenario table: what NVDA does, one step per line |
| `crates/gorca-bench/src/e2e/m8_baseline.rs` | steps expected to fail on `main` and why; delete a line when the feature lands |
| `crates/gorca-bench/src/bin/e2e_m8.rs` | the gate rows and the steps report |

Statuses: PASS, GAP (core words right, NVDA extras missing), XFAIL (expected to fail, reason
recorded), XPASS (expected to fail but passes: remove the mark), FAIL (unmarked failure: the gate
fails). Add a feature, delete its baseline lines, keep the run green.
