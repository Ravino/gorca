# B-1 judgement: native mirror fill strategy (arm A lazy vs arm C hybrid)

Judge: reviewer. Date: 2026-10-14. SPEC: `docs/bakeoff/b1-mirror-fill/SPEC.md` (sections 9, 14).
Raw output: `bench/results/b1/judge/`. Nothing was merged; no arm branch was modified.

## 0. Verdict

**Winner: arm C (hybrid), conditionally, medium confidence.** C is the only arm that changes
what the user gets: HIT.all 0.896 vs 0.432 (A is byte-for-byte R0 on hit rate), M-6 1.3 % vs
26.6 %, where-am-I answered without IPC. It pays for it with 14.8x the calls per focus, +33 % CPU
per focus, +6.6 MiB RSS and a 2.4x worse pooled first-focus p99, all inside the gates. Adoption
is conditional on the follow-ups in section 7; **C's Shell `CacheDump` was never exercised by the
authoritative runs** (harness gap, section 5) and must not be adopted on these numbers.

| | weight | A (lazy) | C (hybrid) |
|---|---|---|---|
| Metrics (all bold gates pass for both; ranking HIT.all, FF, MSG, RSS, M6, CPU) | 50 | 22 | 34 |
| Correctness (risks the harness cannot see) | 30 | 25 | 19 |
| Simplicity | 10 | 10 | 4 |
| Extensibility (M4 review/objnav, `ext-*` hints, Newton) | 10 | 5 | 8 |
| **Total** | 100 | **62** | **65** |

The margin is small and comes from the metric that SPEC 9 ranks first: C doubles HIT.all and cuts
corrections 20x; A is R0 on every count (and regresses HIT.ancestry). SPEC 14.4's tie rule ("within
noise on every ranking metric -> simpler arm wins") does not apply: the ranking metrics are not
within noise, they split hard in opposite directions (HIT.all, M6 for C; MSG, CPU, RSS for A).
Choosing A is choosing "no fill policy". So the decision is C, with its cost brought down by the
ports and fixes in section 7 and re-measured before it becomes the product default.

## 1. Scope check (disqualification rule)

`git diff --name-only c3904b2 <arm>` minus result files:

* A: `fetch/strategy.rs`, `fetch/strategy/tests.rs`, `fill/candidate.rs`, `fill/candidate/tests.rs`,
  `docs/bakeoff/.../arm-a/*`. In scope.
* C: `fetch/strategy.rs`, `fill/candidate.rs`, `crates/gorca-mirror/tests/candidate_{audit,behaviour}.rs`,
  `docs/bakeoff/.../arm-c/*`, `.claude/agent-memory/rust-engineer/*` (trivial, agent memory). In scope.
* Neither touched `events/subscription.rs`. `main` (edbcc22) changed none of the arm-owned files
  since c3904b2, so each arm's files apply verbatim on current `main` (M4a, BOUNDS via
  `review::add_bounds` post-processing). Judge trees: `.claude/worktrees/judge-b1-{a,c,r0}`
  (main + arm files), R0 = plain main.
* Code size (non-test, arm-owned, `git diff --numstat`): A +398 / -91; C +1179 / -113 (budget 1200: 98 %).

## 2. Host and validity

* `quiet-host.sh status`: governor `performance`, C2/C3 disabled on all cpus. Harness and daemons
  pinned `taskset -c 2-9`; my builds/tests ran on cpus 10-15 only.
* Load average was **not** < 1: 1.0-17 per slot (other agents); recorded per slot in
  `s1.log`, `s2.log` (`[k/N] arm kind #r (load ...)`), session start/end in `s*.host.txt`.
* Sessions: `s1` (walk 1, ff 2, idle 1, big 1, step 3) and `s2` (walk 2, ff 3, idle 2, big 2,
  step 4), arms A, C, R0 interleaved and rotated by the runner. Combined by symlink into
  `combined/runs` and re-evaluated with `b1 run --rundirs` at the SPEC plan sizes
  (walk 3, ff 5, idle 3, big 3, step 7): `combined/20261014T045305Z.{txt,json}`.
* **Strict fidelity (H-10(a)) failed in every session**: s1 p50 delta 0.692 ms / p99 delta 2.148;
  s2 0.636 / 2.048; probe 0.618 / 2.112 (nominal 0.5 +-20 %). Standalone repeats
  (`fidelity-probe-1.txt`): 3 of 7 pass, failures at 0.61-0.86 ms. The runner therefore marks all
  three sessions `INVALID RUN`. No run directory was invalid (`B1-RUNS.invalid` 0 everywhere).

**Why the ranking is robust anyway.** (1) Fidelity error is a per-call service-time inflation of
~0.1-0.35 ms in the shared fake apps; every arm runs against the same binaries, bus and host in
interleaved slots, so it shifts all arms' latencies the same way. (2) The ranking metrics that
decide this bake-off are counts, not times: HIT.all, M6, calls per focus, RSS are identical to 3
digits across repeats (min = max for HIT.all, M6, MSG.walk within 1 %). (3) Every latency gate has
>= 3x headroom for both arms (worst: C's L1c.vs_r0 1.15 vs 1.2, see below). A quiet-host rerun can
move absolute ms, not the order.

## 3. Gates (SPEC 9), median over repeats, combined runs

| Gate | limit | A | C | R0 |
|---|---|---|---|---|
| STALE@walk / LOST@walk / L3.lost (worst) | 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| L1h.p99 ms | <= 2 | 0.395 | 0.880 (0.40-1.18) | 0.772 |
| HIT.revisit | >= 0.95 | 0.996 | 1.000 | 0.996 |
| L1c.p99 ms (min-max) | <= 15, <= 1.2x R0 | 3.93 (3.71-4.31) = 1.05x | 4.30 (3.90-5.93) = 1.15x | 3.73 |
| FF.p99 worst model ms (median) | <= 15 | 1.59 (gtk3) | 1.61 (gtk3) | 1.59 |
| S1.p99 ms | <= 5 | 3.30 | 3.56 | 3.39 |
| M6.audible | < 1 % | 0 | 0 | 0 |
| MSG.idle calls/s/app | <= 0.5 | 0 | 0 | 0 |
| MSG.l3 calls/s | <= 500 | 0.014 | 0.014 | 0.014 |
| RSS.big MiB | <= 128 | 1.68 | 8.29 | 1.70 |
| RSS.soak MiB (10->60 min) | <= 8 | -2.48 | +0.55 | - |
| L4.p99 ms | <= 15 | 3.32 | 2.67 | 3.52 |
| L4.announce | <= 500 ms | **no value** | **no value** | **no value** |
| STEP.max_us (median of 7) | <= 250 | 33.5 | 52.7 (one round 338.7 at load 16) | 20.6 |
| CONF: IC-1..23 via selector (`b1_ic candidate`) | pass | pass (185 s) | pass (198 s) | - |
| MA-1/MA-6/MA-9 through the driver (`gates-ma-audit.txt`) | pass | pass; step at caps 4 us rel. | pass; 15 us rel. | - |
| fmt / clippy `-D warnings` on main + arm | clean | clean | clean | - |

`L4.announce` has no value for any arm including R0: the known harness gap (harness-gaps report
section 10). It fails every arm equally and is not an arm defect; I judge on the remaining rows.
With that row set aside **both arms pass every bold gate** and both are rankable.

## 4. Ranking metrics

| Metric | A | C | R0 | note |
|---|---|---|---|---|
| HIT.all | 0.432 | **0.896** | 0.432 | per model: settings 0.28 -> 0.84, files 0.70 -> 0.99, gtk3 0.0 -> 0.97 |
| HIT.ancestry | **0** | 1 | 1 | A issues an Interactive `Ancestry` on where-am-I that R0 does not need |
| FF.p99 pooled (1250 samples) | 1.56 ms, max 6.3 | **3.69 ms, max 21.5** | 1.59, max 6.2 | C: 5 of 6 samples > 5 ms are `shell` (section 5) |
| MSG.walk msgs/s | **34.2** | 505.2 | 31.1 | 14.8x |
| calls per focus | **6.95** | 103.0 | 6.32 | shell: 14 433 calls for 12 focuses in C |
| RSS.big MiB | **1.68** | 8.29 | 1.70 | |
| M6 (corrections / focus) | 0.266 | **0.013** | 0.266 | all inaudible (refine 2-5 ms after hand-off) |
| CPU per focus us | **7 168** | 9 555 | 7 107 | +33 % (the "+19 %" in C's RESULTS was on a different host state) |
| ff daemon CPU s (one ff run) | 0.49 | 5.07 | - | C prefetches every new app after first focus |

The "13x calls" in the brief is 14.8x on this harness (103 vs 6.95 calls per focus).

## 5. Findings (ranked)

1. **High, harness gap that hides C's riskiest mechanism.** The walk/ff/big `shell` model does not
   own `org.gnome.Shell` (`tests/harness/b1/models/shell.json` has no `owns_name`; only IC-22 sets it,
   `crates/gorca-testkit/src/b1/ic.rs:1064`). So `verified_shell`/`dump_complete` is false
   (`crates/gorca-connector-atspi/src/fetch/mod.rs:371`) and **C never issued a `CacheDump` in any
   authoritative run** (no `Cache.GetItems` in any `counters.json`). C's Shell numbers come from its
   generic prefetch, which on Shell costs ~1 200 calls per focus (14 433 calls / 12 focuses).
   Probe (information only, `probe-shell-dump/`, model patched to own the name): C dumps 3 times
   (once per Shell instance), Shell calls fall to 183, Shell hit 4/12 + 2 partial, STALE 0, Shell FF
   p99 0.96-1.00 ms (no tail). Fix: give the walk's Shell `owns_name` (harness, test-engineer) and
   re-run before any decision about the dump.
2. **High (C), Shell main-loop stall is real and unmeasured in timing.** Each dump is one serial
   `GetItems`: 1 765 items x 10 us = ~17.6 ms (3 000 items in overview: ~30 ms) on the compositor
   thread (P-9 live number 17.0 ms). Gating is "first focus presented + 500 ms without a focus move"
   (`candidate.rs` `DUMP_QUIET`), re-armed after a flood/dirty-all resync older than 30 s
   (`CACHE_DUMP_INTERVAL`). Failure scenario: user opens the overview (1-2k `children-changed` ->
   flood -> resync), types an app name with 300-600 ms gaps; the dump fires between two keys and the
   next focus's Interactive fetch waits behind ~17-30 ms of Shell work, breaking the 15 ms keypress
   budget once per overview use and dropping 1-2 frames. Fix: gate the dump on keyboard idle from the
   input lane, not focus-quiet, and on a live-session measurement (SPEC 8.4) of frame time.
3. **Medium (C), first-focus tail from background prefetch of other apps.** Pooled FF p99 3.69 vs
   1.56 ms, one sample 21.5 ms at load 1.6 (s1 `c/ff/1`, shell, first 30 s; 5 of C's 6 samples > 5 ms
   are shell); daemon CPU in ff 10x A's. Mechanism: after each first focus C walks the app
   (ancestry, sibling pages, a 2 048-node `Subtree` after 150 ms quiet) while the next app is
   appearing. Disappears with the verified-Shell probe (no generic prefetch on Shell). Fix: no
   `Subtree` crawl at all in the first port; prefetch only the focused app and yield on any
   `Appeared`.
4. **Medium (C), prefetch cost per focus.** 103 calls per focus; on real GTK4 (0.12-0.15 ms per call,
   P-9) that is ~12-15 ms of the focused app's main-loop time per focus, competing with the app's own
   frame and with the next Interactive call (L1c 1.15x R0, worst repeat 1.59x). Sources: page re-read
   at `REFRESH_AGE` 8 s; `child_snapshots` always calls `GetChildren` for the *whole* list per page
   (strategy.rs, C), and `start_probe` probes up to 8 pages (256 node fetches) to find a node because
   the policy never learns the index from the `GetChildren` reply it already paid for. The fake app
   prices `GetChildren` at base service time; a 10 000-row GTK4 list serialises far more, so the real
   cost is underestimated. Fix: port A's `child_refs` (indexed `GetChildAtIndex` for <= 8, one
   `GetChildren` otherwise) and return the node's index with the page; drop the age-based refresh in
   favour of RV-1 plus `ChildrenChanged`.
5. **Low (C), dirty marking stops at page 8.** `note_dirty` marks only pages `0..MAX_PROBE_PAGES`
   while `ensure_page` follows the focus to any page; rows beyond 256 are refreshed only by age.
   Correctness is still covered by RV-1 (STALE 0), so this is cost, not wrong speech.
6. **Low (C), outstanding-request bookkeeping forgets after 3 s.** `self.out.retain(... < OUT_TTL)`
   drops entries whose reply has not come back; a slow `Subtree` (> 3 s) lets the policy exceed
   `MAX_OUT_BACKGROUND`. Bounded by the driver's slots, so not a leak; replace with the driver's reply
   guarantee (every request gets a `Reply`, `Cancelled` included).
7. **Low (A), where-am-I regression vs R0.** `HIT.ancestry` 0 vs R0's 1: `ask_ancestry` treats an
   ancestor as deficient unless NAME|ROLE|STATES|PARENT are fresh and not aged, so it fetches chains
   the Reader already answered from the mirror; the fetch cannot improve an answer already spoken.
   Fix: ask only for what the where-am-I sentence uses (C's `walk_chain` rule).
8. **Info (A), no measurable effect over R0.** HIT.all, M6, calls are R0's to 3 digits; the focus
   refetch-on-event path did not change any number (also noted by A itself).
9. **Medium, harness: soak is always INVALID as shipped** (section 6): 10 loops x ~224 s < 3 600 s,
   and `B1_SOAK_S` is not passed through `run-isolated.sh`.
10. **Info, shared.** `L4.announce` has no value (harness). Strict fidelity is bimodal on this host
   even with C2/C3 off (passes 3/7 standalone); the in-app service-time amendment of the harness-gaps
   report section 7 is the remaining fix.

## 6. Soak (RSS.soak)

Run once per arm, A and C concurrently (RSS-only gate; A pinned to cpus 2-9, C to 10-15,0-1).

| | A | C | limit |
|---|---|---|---|
| B1-RSS.soak (minute 10 -> 60) | **-2.48 MiB** | **+0.55 MiB** | <= 8 MiB |
| STALE@soak / LOST | 0 / 0 | 0 / 0 | 0 |
| RSS plateau | 9.4-12.3 MiB | 16.6-20.0 MiB | |
| HIT.all over 63 min | 0.434 | 0.895 | |

Both pass (`soak2-{a,c}/`, 3 807 s each; strict fidelity passed for A's session, failed for C's,
p50 delta 0.807 ms). **Harness defect found on the way:** the first soak (`soak-{a,c}/`, kept)
ran only 2 242 s and is INVALID for both arms ("soak run needs rss_series_kib cover"): `b1-walk`
sizes the soak as `B1_SOAK_S / 360` loops (`crates/gorca-testkit/src/bin/b1_walk.rs` "soak" arm)
but one W-1 loop takes ~224 s, and `bench/b1/driver.sh` does not pass `B1_SOAK_S` through
`run-isolated.sh` (`GORCA_PASSTHROUGH`). I added `B1_SOAK_S` to the passthrough in the judge trees
only and ran with `B1_SOAK_S=6120` (17 loops). Fix on `main`: loop until the deadline instead of
a precomputed loop count, and pass `B1_SOAK_S`. The 37-min partial series had suggested C growing
~0.17 MiB/min; the full run shows a plateau (allocator trim at ~35 min), so it was a window artifact.

## 7. Recommendation: what to adopt, what to port

Adopt **C's focus-window prefetch** (ancestry to the window + the focus's sibling page on
`FocusSettled`, `WindowActivated` refresh, `ChildrenChanged`-driven page refetch, health/backoff
gating, `dump_complete`-only eagerness) as the product fill policy, after these changes and a
re-measure with this harness (SPEC 14.5 applies to any port):

From A (port):
* A's `strategy.rs` `child_refs` + bounded `map_concurrent` (JoinSet owned by the op future, abort
  on drop) instead of C's whole-list `GetChildren` per page and its fixed `PAIR` of 2.
* A's `ancestry` that takes the parent ref from the level's own fetch (one round per level).
* A's refetch spacing (`REFETCH_GAP` + `WakeAt`, one in flight) for the focused-node rename path.
* A's zero-state ethos as a budget: policy state that is exact and bounded (C's is, at ~0.3 MB).

Fix in C before adoption: findings 2, 3, 4 (no `Subtree` crawl; index from `GetChildren`; no
age-based page refresh; target <= 25 calls per focus), 5, 6.

Keep **C's Shell `CacheDump` off** until finding 1 (harness) and finding 2 (keyboard-idle gating +
live frame-time measurement) are done; the probe says it is correct and cheap in calls, not that it
is safe for the compositor.

Measure as ablation (not done here): C with only sibling pages vs pages + `Subtree`, to see which
mechanism buys the 0.43 -> 0.90.

## 8. Evidence index

* Gate tables: `bench/results/b1/judge/combined/20261014T045305Z.txt` (+ `.json`); per session
  `s1/20261014T024414Z.txt`, `s2/20261014T045245Z.txt`; slot loads in `s1.log`, `s2.log`.
* Per-model hit and latency: `perapp-walk.txt` (script `perapp.py`; latency there is fake-app
  focus emit -> presentation, relative only). FF tails: `ff-tails.txt`.
* Shell-dump probe: `probe-shell-dump/`, `probe-shell-dump.{log,host.txt}`.
* Conformance: `gates-{a,c}-tests.txt` (unit + IC via selector), `gates-ma-audit.txt` (MA-1/6/9
  for both arms through C's driver adapter, copied into the A judge tree as `judge_audit.rs`),
  `gates-{a,c}-clippy.txt`. Fidelity: `fidelity-probe-1.txt` and each session log.
* Soak: `soak2-{a,c}/` (valid, 63 min), `soak2-{a,c}.log`, `soak2.host.txt`; first, short and INVALID soak kept in `soak-{a,c}/`, `soak.host.txt`.
