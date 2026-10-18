# Phase 2 acceptance run (2026-10-18)

Code: main `1308ae05` (binaries built from it; later commits touched only `bench/`, `tests/term/` and the
VK scenario; reruns of m3 and m6/vv used a daemon rebuilt at `28c9aa47`/`d3df0982`, same sources).
Runner: `bench/acceptance/phase2.sh --quiet-host`, all suites incl. m9, 3 repeats, shipped default topology
(`--topology=auto`, resolved `lanes-rt`). Raw results: `bench/results/acceptance/phase2-20261018T001009Z/`
(`summary.md`, `status.tsv`, per step `<suite>/<step>.aN/`; attempts are never overwritten).
Soak: `bench/results/acceptance/soak-m6-20261018T090718Z-overnight/` (1 h, see below).

## Host conditions
Quiet-host on (performance governor, C2/C3 off), reverted at the end of each run (`quiet-host restored`
in the logs; verified C3 `disable=0` afterwards). The owner's live desktop was running: load1 was 2.2-10.5
(not below 2 as the README asks), MemAvailable 14-15 GB all run, memory PSI some avg10 max 7.3
(m6/fake). Swap is in use on the host (17 GB), see the soak RSS caveat. Every step's load range is in
the "load" column of `summary.md`.

## Result by suite (final attempt)
| suite | result | notes |
|---|---|---|
| m1 (K2 K2s K3 K4 K4h R1 R1s RS) | PASS a1 | K2 p99 0.198 ms (gate 5), K3 1.04 (15), R1 0.275 (3); load 4.0-8.3 |
| m3 (C1.. E1.. SA.. PW) | PASS a4 (a1 FAIL) | C1 p99 0.66 (15), C1h 0.29 (3), E1 0.29 (3), SA3 stop 0.40 (2) |
| m4 (A1 N1) | PASS a1 | A1 5.30 (15), N1 0.45 (15) |
| m6 fake (V1..VW) | PASS a1 | V1 32.6 (35; includes the 30 ms settle), VT 0.53 (3), V3 stop 0.175 (2), VB 0.37 (4); 45 gated rows |
| m6 vv (real VTE) | PASS a4 (a1 FAIL) | key p99 0.51 (3), flood key 0.52 (4), stop 0.14 (2), `vv_server_aborts` 0 |
| m7 Firefox | PASS a1 | 77 gated rows; FC1 p99 0.61 (15), W2b 1.8 (150) |
| m8 Firefox | PASS a1 | 46 gated rows, 0 failing steps; all key p99 < 0.5 ms (5) |
| m9 Chromium m7 | PASS a1 | FC1 p99 7.2 ms (15; Firefox 0.6), rest sub-ms |
| m9 Chromium m8 | FAIL a1, a2 | M8-FL fail 3, M8-FLS fail 1, identical in both attempts |
| storm | PASS a1 | L1/L3-10k/L3-50k/L4 p99 5.3 (15), K1/K1-50k 0.26-0.29, lost 0; load 4.1-8.6 |
| x1 hooks | PASS a1 | hook/recovery gates on the worst repeat |
| b1 judge | FAIL a1 (timeout), a2, a3 | see class 3; no latency gate failed |

Medians over the 3 repeats; every latency p99 is far inside its gate (most by 10x or more), so the
15 ms goal has large headroom at this load. Variance across repeats is small (max within ~10% of the
median on the rows listed in `summary.md`).

## Failing gates, triaged
1. **m3 C1hs.lost (a1): max 5 events, median 0 - FLAKE.** One repeat of three lost 5 of 1200 samples
   (n 1195). Rerun a4: lost 0 / n 1200 in all repeats. Two intermediate attempts (a2, a3) were refused
   by the harness's stale-reader check (my own commits moved HEAD), not a product result. Open: the
   cause of the 5 lost is not understood (no repeat-level log kept); needs a stress loop (below).
2. **m6/vv vv_focus_failures / vv_screen_failures (a1): max 1 - FLAKE, known family.** One repeat: "Alt+Tab
   back: output before any key" heard "Terminal, Terminal, terminal" and "blank" instead of the screen.
   The same single-step miss was recorded at the M6 merge (decisions 2026-10-16, load 15-33) and now at
   load 3-8; it is therefore NOT load-driven. Rerun a4 clean (0 of 3). It is the "terminal silent after
   Alt+Tab until the first key" follow-up still open in `docs/reports/m6-impl.md`.
3. **m9/chromium-m8 M8-FL (3 steps) and M8-FLS (1 step) - PRE-EXISTING GAP, deterministic.** Both attempts:
   FL.cur heard "M8 News article document" (wanted "News article" + "Account menu"), FL.next5 "bullet Politics
   link", FL.prev "14 October 2026, 09:30 6 min read", FLS.bottom "The very last line" (wanted "Final heading
   marker"). Firefox passes the same corpus (M8-FL 6/6). The flat-review fix `711fb314` is Firefox-specific
   ("flat review reads Firefox page content"); the last Chromium m8 run in `ff-focus-name-flat-review.md` had
   40 steps, the FL/FLS steps (now 46) were never run on Chromium before. Not a regression of a prior pass.
4. **b1/judge-quick - WRONG EXPECTATION in the runner + harness flake + known gap.** a1: the runner passed
   no `--quick` (bug: `$quick` was only set for `--dry-run`), so the full plan (walk 5, ff 5, idle 3, big 3, step 7)
   hit the 5400 s cap (exit 124); fixed in `b63baa06`. a2: INVALID RUN, "service-time fidelity" failed
   (p50 delta 0.356 ms) even on the quiet host; a1 and a3 passed the fidelity check, so it flaps (cpuidle /
   desktop noise). a2 and a3 both: `B1-L4.announce` has no value (`rankable no`): known harness gap, the same
   NaN is in `bench/results/b1-adopt/adopt-rev-final.log` for r0 and the adopted arm (nothing writes
   `l4_announce_ms`). All 23 other b1 gates pass in a3 (L1c p99 3.85 ms of 15, L4 2.85, S1 3.2 of 5, M6
   audible 0, STALE/LOST 0, HIT.revisit 0.956).

No regression (a gate that passed on an earlier commit and now fails) was found.

## 1 h terminal soak (partial; the 8 h soak is deferred to a later night window per the owner)
New scenario `VK` (`crates/gorca-bench/src/e2e/m6.rs`) and driver `bench/acceptance/soak_m6.sh`: one reader and one
real gnome-terminal-server in private sessions for the whole run; rounds of the VD disturbances (idle, second
app + Alt+Tab, tab switch, notifications, bell, overview, Alt+Tab storm, search bar, context menu) plus
ordinary bash use, and a `yes`/`seq` flood with typed keys and Ctrl+C every third round (flooding producer).
One TSV line per round. Requested 28800 s; stopped by SIGTERM to its own process group at 4130 s
(79 rounds, 26 floods); quiet-host reverted. Results are labeled `LABEL.txt`.

* **Aborts: 0** `gnome-terminal-server` exits, 0 reader deaths, 0 flood runs without the prompt returning
  (26 of 26 prompts back). `vte-server.err` criticals were not counted (the scenario did not finish).
* Flood key latency (gorca-side, mock speech): median of per-flood p50 0.31 ms, worst single key 0.79 ms
  (gate for VV flood keys 4 ms); stop-on-Ctrl+C worst 0.1 ms (gate 2 ms).
* CPU: reader 3948 ticks (39 s) in 4130 s, i.e. about 1% of a core including the floods.
* RSS: reader 100.5 -> 88.9 MB over the first 11 rounds (+0.5 MB), then both processes were paged out by the host
  at round 11 (reader 8.6 MB, server 12.2 MB RSS in the same line, CPU ticks continuous, MemAvailable +900 MB;
  swap in use) and RSS then crept 12.7 -> 13.3 MB over 67 rounds. RSS is therefore NOT a clean series; VK now also
  logs `VmSwap` (`reader_swap_kb`, `server_swap_kb`) for the 8 h run. No growth trend visible in the 1 h window.
  Threads constant at 51.
* VD steps: 3 failing steps in 79 rounds, all in the "tab switch" round (rounds 2, 12, 22; the opening
  disturbances are replaced after round 30). Also reproduced in a plain 3-round `VD` check
  (`bench/results/acceptance/vd-tabswitch-check-b-*`): after Ctrl+PageUp/PageDown bash receives "5;5~"
  ("bash: 5 5 5 echo: command not found"), i.e. the headless gnome-terminal did not take Ctrl+PageUp as the tab
  accelerator and the key went to the shell as an escape sequence. Probably a rig limit, not the reader; it
  is not a gate (VD is informational) but should be confirmed.
* 1 h is not 8 h: the zero-abort gate of the plan is NOT yet proven; only "no abort in 1.15 h and 26 floods".

## FIX LIST (priority order)
1. **Chromium flat review (M8-FL/FLS, 4 steps)**: extend the `711fb314` fix so review current/next/previous/bottom
   work on Chromium page content; or, if the NVDA-parity expectation is Firefox-only for now, mark the four steps
   XFAIL for the m9 suite. Evidence: `m9/chromium-m8.a1,a2/res/*-steps.md` lines 559-575. Needs the owner's call.
2. **Terminal silent / empty after Alt+Tab (VV intermittent, 1 of 3 repeats at low load)**: make the cause visible
   (keep the reader log of a failing repeat via `GORCA_VV_KEEP_DIR`, add the focus-audit trace), then fix; run VV
   x20 in a loop to get a rate. Evidence: `m6/vv.a1/res/*.txt` note0.
3. **C1hs lost 5/1200 in one repeat**: run C1hs x20 to see whether it recurs; if so keep the reader log. Gate
   semantics (lost <= 0 on the worst repeat) are right; do not relax.
4. **b1 in the acceptance runner**: (done) always `--quick`; still open: `B1-L4.announce` has no producer in the
   harness (known since b1-adopt) so b1 can never be "rankable"; either implement `l4_announce_ms` or drop the
   gate from the acceptance set. The fidelity flap needs a retry policy (rerun fidelity once before INVALID).
5. **Runner hardening**: the stale-reader check turns every result commit into a refusal on resume; let phase2.sh
   pass `--allow-stale-reader` when only `bench/`, `docs/` or `tests/` changed since the build. The README says
   load < 2 but the quiet-host window ran at 2-10 with the live desktop; either relax the doc or accept it.
6. **`cargo clippy --release -D warnings` fails** in `crates/gorca-mirror/src/store/mod.rs:1305`
   (`debug_check(&self)` unused `self` when `debug_assertions` is off); dev-profile clippy is clean. Add
   `#[allow(clippy::unused_self)]` or restructure, since the release build is what ships.
7. **VD tab-switch round**: confirm in the rig whether Ctrl+PageUp is a tab accelerator in the private
   gnome-terminal (rig) or a real input issue; a rig fix keeps the 8 h run free of a known 1-in-10 failure.
8. **8 h soak**: run `bench/acceptance/soak_m6.sh --quiet-host` (default 28800 s) in the night window; the
   series now includes swap. Run it only after items 2 and 7 so its VD failures are meaningful.
9. Chromium focus latency is 12x Firefox (FC1 p99 7.2 vs 0.6 ms); inside the gate, no action, but watch it.

## Reproduce
```
bench/acceptance/phase2.sh --quiet-host                  # full run, about 6.5 h with m9
bench/acceptance/phase2.sh --resume <dir> --quiet-host   # reruns failed steps as attempt N+1
bench/acceptance/soak_m6.sh --secs 3600 --quiet-host     # the 1 h soak; default 28800 is the 8 h one
```
