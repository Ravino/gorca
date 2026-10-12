# B-2 blocking fixes review (branch `worktree-agent-a3d5ba691ea04e152`, head 9d4f641)

Reviewer, 2026-10-12. Scope: `git diff main 9d4f641`. Governing documents: ADR-0004, ADR-0001 §6.3,
`docs/bakeoff/b2-ingress/SPEC.md` (AU-1 to AU-8, B-3, T1 to T7).

**Verdict: BLOCK.** There are two regressions that need small fixes (F-1, F-2). Everything else can be a follow-up.

All raw output is in the worktree under `bench/results/b2-ingress/review-r1/`:
`run-all.log`, `20261012T192905Z.{json,txt}`, `*-fakeapp-*.json`, `t7-rerun.txt`,
`t4-threads.txt`, `t4-threads-pinned.txt`, `judge-r2-probes.txt`, `judge-focus-scan.txt`,
`release-audit-gates.txt`, `workspace-debug-tests.txt`, and the probe sources `judge_r2_probe.rs.txt` and
`judge_r2_focus_scan.rs.txt` (they were removed from `tests/` and are not part of the branch).

Host: Intel Core Ultra 9 285H (6 P-cores, 8 E-cores and 2 LP E-cores), `powersave` governor, PREEMPT_RT.
Load average was 6.0 at the start and 11-13 for the whole run (other tenants).

## 1. Gates re-run

| Gate | Result |
|---|---|
| `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings` | pass (clippy exit 0) |
| `cargo test --release -p gorca-ingress -- --include-ignored --test-threads=1` | 31/31 pass (audit_budget, audit_concurrent_budget, audit_fairness, audit_latency, audit_retained_capacity, audit_text_weight) |
| `cargo test --workspace --no-fail-fast` (debug) | all pass. `realbus::broker_fixture_reproduces_f3` failed once in debug and passed on the rerun and in both run-all passes. It is flaky and not touched by this diff |
| run-all conformance and bounds (B-1..B-5, debug and release) | pass |
| T1.sustained_eps | 400k (PASS) |
| T2 p99 @idle/10k/100k | 243 / 195 / 203 µs (PASS) |
| T3 focus p99 mock | 200 µs, unobserved 0 (PASS) |
| T4.cpu_ms_per_10k@10k | **168 (FAIL, <= 25)** |
| T4.drain_core_pct@100k | 18.6 % (PASS) |
| T4.cpu_ms_per_10k@100k | 18.6 (PASS) |
| T5 all | PASS |
| T7.push p99 / p999 | 14 / 21 ns (PASS) |
| T7.push4 p99 / p999, run-all, 3 reps | 4,898 ns PASS / **50,160 ns FAIL** |
| T7.push4 p99 / p999, standalone, 5 reps | **5,193 ns FAIL** / 48,070 ns PASS |

T7.push4 sits on the gate. p99 is 4.9-5.3 µs against a limit of 5 and p999 is 45-50.5 µs against 50, so it passes or fails depending on the run.
Part of the first T7 run overlapped one of my probe compiles, so the standalone rerun is the clean number.
The branch's claimed 4.92 / 48.4 is not robust.

## 2. Findings (ranked)

### F-1 HIGH (regression): a preempted victim loses its newest Focus when it is still in the ring
`crates/gorca-ingress/src/coalesce.rs:416` (`discard_ring`), called from `Shared::shed` (`:427`).
Preemption now pops and drops every ring record. `Table::shed` keeps only the table's focus.
A Focus that has not been folded yet is lost. An app with fewer than `RING_CAP` (1,024) pending
events usually holds all of them in the ring, because a producer folds only when the ring is full or when `held >= EAGER_AT`.
The weight-based victim selection makes exactly those apps eligible.
**Probe** `judge_preemption_discards_focus_in_ring`: the victim pushes 250 windows and then a Focus, and a filler exhausts the pool.
The victim drains `flooded=true dirty_all=true events=0 focus_kept=false`.
This breaks ADR-0004 §8 ("Lossless `Focus` always queues") and the `Table::shed` contract ("the newest focus survives").
On main a ring of this size was never a victim (`held_hint` was 0, with a 128 minimum).
**Scenario:** the user Tabs in Firefox while another app floods. The Firefox actor gets dirty-all and no Focus, so nothing is spoken until the resync.
**Fix:** in `discard_ring` during preemption, remember the last `Focus` popped and re-apply it to the table after `t.shed()`, through the same lossless focus-charge path.
Add the probe as an audit test.

### F-2 HIGH (regression, conditional): O(apps) registry scans under the app's lock break AU-6
`crates/gorca-ingress/src/table.rs:363` (Focus: `(0..32).any(... preempt ...)`) and `table.rs:274` (every
`acquire` while `held < 64`), with the scan at `coalesce.rs:308-313`. The floor is now `max(own, R)`.
When the pool is exhausted and no holder is above the floor, every attempt is a full registry scan
(a `Weak::upgrade` for each app) that returns false. The Focus path does this 32 times.
**Probe** `judge_focus_push_scans_registry_32_times`. The pool is exhausted by reserve-sized holders, because reserve units count in `used`; see F-6.
The worst Focus push plus fold took **657 ns at 100 apps, 837 µs at 2,300 apps and 7.6 ms at 10,000 apps**, against the 250 µs AU-6 limit.
A non-Focus push of an app between R and 63 slots pays one scan each, about 26 µs at 2,300 apps. That cost lands on the drain thread for every such event, which caps throughput.
At the default `GLOBAL_SLOT_BUDGET` the no-victim state needs apps × R ≥ 262,144, about 8,200 connections.
At smaller limits, or with apps sized just at R, it is reachable with the 2,300-connection attack.
On main the floor was 2, so the loop exited at the first shed.
**Fix:** use one scan per push at most, keep the Focus retries from rescanning, and keep a cheap "no eligible victim" flag or epoch (or a max-weight hint) so that failed preemption is O(1).
Extend `audit_preempting_push_is_bounded` with 2,300 and 10,000 reserve-sized apps.

### F-3 MEDIUM: the per-app reserve overshoots with ≥ 2 producers; B-3 is exceeded and the test ceiling was relaxed to hide it
`coalesce.rs:400-406` (`cover`: `room = R - (credit + held_hint)`) and `table.rs` (`admit(own + ring_units)`).
These are two independent, stale views of the same reserve. `held_hint` is published only at the end of a fold, and `ring_units` is a snapshot.
**Probe** `judge_reserve_overshoot_with_concurrent_producers` (pool limit 0, so everything comes from the reserve): peak `used` was
32 with 1 producer and **48-80 with 2-16 producers**, against R + 1 = 33.
It is transient: after a settle, AU-1 equality holds and nothing leaks, because `trim_credit` releases at most the credit it took.
It does not compound across apps beyond about R + 16·P per app.
The branch's own AU-7 test uses `ceiling = limit + apps*(R + 1 + P)`
(`tests/audit_concurrent_budget.rs:31`). That is a bound looser than SPEC B-3, and even that bound is exceeded in my probe.
In production, `max_event_connections` defaults to 1, so there is one producer per app and the issue is latent.
**Fix:** keep a single per-app `reserve_used: AtomicUsize` taken by CAS up to R and shared by `cover` and `admit`.
Restore the test ceiling to `limit + apps*(R+1)`.

Answers to the question: the under-count race is bounded by the number of concurrently pushing threads (each thread can be in only one push), self-heals on the next block, and leaks nothing.
`release` never exceeds charged credit, and `Drop for Shared` returns the remaining credit. AU-1 holds at quiescence (probe asserts `used == pending` after a settle; the branch test does the same).
AU-7 at the pool level holds, because the pool part is CAS-bounded. The reserve part (F-3) is not bounded.

### F-4 MEDIUM: on a full reaper channel the victim is freed on the preempter's critical path, which contradicts ADR-0004 §8
`coalesce.rs:235-249`: `drop(tx.try_send(..))` frees the garbage in place, inside `Table::acquire` under the
preempting app's lock and on the drain thread. ADR-0004 §8 says that on a full channel the table is attached to the victim's next `Drained`.
Other points:
- the reaper thread is spawned lazily inside the first preempting push, inside the timed critical section;
- each budget gets its own `gorca-ingress-reap` thread, while the ADR names `gorca-reaper` and says the watchdog monitors it;
- if `spawn` fails, `OnceLock<None>` makes every later dispose run in place, silently.

Shutdown is safe: the thread exits when the sender drops with the budget. Panics are not an issue either: a dead receiver turns `try_send` into an in-place free.
**Fix:** use the fallback the ADR specifies, spawn at `SlotBudget::new` (or at daemon start), use the ADR thread name, and count fallbacks in stats.

### F-5 MEDIUM (latent): the producer spins without a bound when the ring is full
`coalesce.rs:602`: `None => std::hint::spin_loop()`, with no yield and no fallback to `lock()`.
The drain lanes are planned as `SCHED_FIFO` (`crates/gorca-rt/src/lane.rs`, `Lane::Drain => FifoLow`).
A FIFO drain thread that spins while a normal-priority app actor holds the table lock on the same CPU can spin until the actor migrates, which is a priority inversion.
With RT throttling off, it can livelock.
Today, without FIFO, the spin is bounded by the holder's critical section (at most `DRAIN_CHUNK` folds) and is only wasted CPU.
**Fix:** spin about 64 times, then `yield_now`, then a blocking `lock()` and fold.

### F-6 LOW: reserve units consume the pool
`used` adds reserve and pool units together, and `try_charge` compares the total with `limit`.
The doc comment "a pool of `limit` units on top of the per-app reserves" (`SlotBudget::new`) and ADR D-5 ("outside the shared pool") are therefore not what the code does.
The pool shrinks by R for every app at its reserve. AU-3 still holds. F-2 is the consequence.
**Fix:** use a separate reserve counter, or correct the documentation.

### F-7 LOW: T7.push4 is not a robust pass (section 1). Help chunk 512 and `RING_CREDIT_BLOCK` 16 are tuned to the gate edge.

### Checked and OK
- No `unwrap()` or `expect()` in non-test code in the diff.
- New matches are exhaustive (`TryLockError` arms).
- The only test relaxations are the unit test bound `limit + apps*(R+1)`, which is the amended B-3 and is legitimate, and F-3.
- The audit tests from main are unchanged. B's AU-7 test is ported.
- `audit_fairness.rs` contains the 2,300-connection attack and passes.
- BF-4 wiring is explicit, with no singleton (`app_channel_with_budget`, `ScopeRegistry::new(Arc<SlotBudget>)`, `unbounded()` for tests).
- Victim choice (max of table + ring weight, `try_lock` only, victim lock released before `dispose`) cannot deadlock.
- AU-6 for a preemption against one 65k holder: `audit_preempting_push_is_bounded` passes in release.

## 3. Feeder probe-order fix: legitimate

`crates/gorca-bench-b2/src/feeder.rs:200-222`. Focus is last-writer-wins by design (ADR-0001 §6.3).
A data `FocusSignal` that follows the probe in the same write supersedes it, and that is correct coalescer behaviour.
Real apps that send focus right after focus should have the older one dropped.
The send timestamp is taken after encoding, just before `send`, so the order does not change the stamp.
Putting the probe last makes the drain parse the whole write before it pushes the probe, which is conservative.
No coalescer defect is hidden. A residual measurement caveat: a data focus in the next write can still supersede a probe before the consumer drains it.
T3.focus_unobserved was 0 in every run.

## 4. T4.cpu_ms_per_10k@10k: what it measures, and a recommendation

**What it measures:** the sum of `schedstat` on-CPU ns of all threads not named `hx-*` (procfs.rs `candidate_ns_since`), divided by framed messages / 10k.
The feeder, the mock bus, the consumers and the sampler are excluded. The `b2 threads` breakdown shows that **only `gorca-drain-0` counts**.
No spinning is involved: idle is 0.01 ms/s, and the drain does a blocking `read` (raw.rs `run_loop`).

**Why it is 160-170:** with 5 apps × 2k ev/s and a 200 µs write quantum, each write carries less than one message, so the drain wakes once per message.
The cost is dominated by the wakeup and depends on which core the thread runs on and at what frequency:

| `b2 threads`, 1 app, 5 s | P-cores 0-5 | E-cores 6-13 | LP E 14-15 | unpinned |
|---|---|---|---|---|
| 2k msg/s, ms per 10k | 561 | 110 | 167 | 417 |
| 100k msg/s, ms per 10k | 14.5 | 8.2 | 13.1 | 18.3 |

The same code varies by 5× with placement. At 100k (batched reads) every placement passes 25.
On the best core, the floor is about 11 µs per wakeup-message. 25 ms per 10k means 2.5 µs per message, including one blocking-socket wakeup per message.
No blocking-recv drain reaches that on this host: the reference measured 225, and A, B and C measured 160-175 at judgement.
Busy-polling would lower the ratio only by burning a core, which contradicts T4.idle.
An "adaptive doze" to batch wakeups trades latency and was already rejected.

**Recommendation: amend the SPEC (confidence high).**
- Gate CPU efficiency at a rate where the drain batches: keep `T4.cpu_ms_per_10k@100k <= 25`, which passes at 18.6.
- Turn `@10k` into an info metric.
- Add `T4.drain_us_per_wakeup` (info, from `batches`), with an absolute budget of `T4.cpu_ms_per_s@10k <= 250` (25 % of one core at a 10k/s storm), which matches M-3 and M-14 spirit.
- Record the governor and core placement in the result meta. Don't accept it as an unexplained known gap; it is a mis-specified gate.

## 5. Required before merge
1. F-1: preserve the newest ring Focus through preemption, and add an audit test.
2. F-2: make failed preemption O(1), with one scan per push, and add an audit test at 2,300 and 10,000 apps.

Follow-ups, in order: F-3 (and restore the B-3 test ceiling), F-4, F-5, F-6, then the SPEC amendment in section 4 and a T7 rerun on a quieter host.

---

# Round 3: fixes-r3 (code fabcfe7, head 2d1c737)

**Verdict: BLOCK** on one item, R3-1. AU-3 is violated under concurrent pool pressure, and the SPEC B-3
derivation makes two claims that the probes refute. F-1, F-2, F-4 and F-5 are verified as fixed.
The T7.push4 result is noise, not a regression.

Raw output is in the worktree under `bench/results/b2-ingress/review-r3/`:
`gates.txt`, `r2-probes-on-r3.txt`, `judge-r3-probes*.txt`, `judge-r3-p5.txt` … `judge-r3-p8.txt`,
`t7-interleaved.txt`, and the probe source `judge_r3_probe.rs.txt`, which is not part of the branch.
Load average was 7-16 throughout.

## Gates
fmt is clean. clippy exits 0. All 40 release ingress tests pass, including `audit_review.rs`. The debug workspace has 0 failures.

## The F-items against my probes
| Item | Probe | Result |
|---|---|---|
| F-1 ring Focus kept | `p1_ring_focus_survives_preemption` (pool limit 230, victim preempted for real) | **fixed**: flooded, Focus kept |
| F-2 O(apps) scans | r2 `judge_focus_push_scans_registry_32_times`, plus `p4` (5k over-R apps, then a Focus push with only ineligible holders in the window) | **fixed**: 2.2-2.7 µs at 100, 2,300 and 10,000 apps (was 837 µs and 7.6 ms); 0.9-1.5 µs in `p4` |
| F-3 overshoot, producers only | r2 `judge_reserve_overshoot_with_concurrent_producers` | **fixed**: peak 32 for 1-16 producers |
| F-3 overshoot, with a concurrent drain | `p3_bound_with_concurrent_drain` | **not fixed**: peak **57-65 vs 33** (R3-2) |
| F-4 reaper | code | fixed: spawned in `new`, named `gorca-reaper`, falls back to parking on the victim, counter `reaper_fallbacks` |
| F-5 spin | code `coalesce.rs:743-765` | fixed: 256 spins, 8 yields, then a blocking `lock()`. The summary says 64/16; the code says 256/8 |
| F-6 reserve outside the pool | `p5_reserve_vs_pool_accounting` | **not fixed** (R3-1) |

## Findings

### R3-1 HIGH: AU-3 is violated; an app at R events gets flooded under concurrent pool pressure
Root cause: on fold, a record's ring credit holds the app's reserve counter while the table charges the same record again.
- `Table::acquire` finds the reserve counter full and takes a **pool** unit.
- The ring's reserve credit is then trimmed, so the reserve goes idle.

`p5` shows this sequentially: one app with 20 or 36 events ends with `pool_used = 20/36` and `reserve = 0`, at limit 100,000 and at limit 100.
Then 3,000 apps × 20 events (all within R) take 49,936 of a 50,000 pool.
The `pool_nearly_full` pre-trim in `fold_into` (`coalesce.rs:602-608`) is a racy heuristic.
If the pool fills in the middle of a group, the table's `try_charge` fails, there is no eligible victim, and the record overflows.
**Probe** `p8_reserve_under_concurrent_pool_pressure`: 3 hostile apps hold the pool at its edge, and a victim pushes exactly R = 32 events and drains.
**45,162 of 917,746 and 31,493 of 1,019,333 rounds kept 31 and were flooded.**
That breaks AU-3 ("an app at or below R is never affected"), and the SPEC text "a pool of N units still holds N units for apps above their reserve" is false.
**Fix:** on fold, *transfer* each popped record's credit unit to the table, reserve first (`res_units += 1` without a new charge), instead of releasing and recharging.
That removes the heuristic and two atomic operations per record. Turn `p5` and `p8` into audit tests.

### R3-2 MEDIUM: the B-3 claim "at every instant" is false while a drain runs (transient, at most about 2R per app)
`release_split` (`coalesce.rs:310-321`) decrements the app counter **before** the global word.
`take_reserve` (`:260-276`) does a CAS on the app counter and then adds to the global word.
A take that lands between the two halves of a release counts its units twice in `used()`.
`p3` (8 producers plus a draining consumer, pool 0) peaked at 57-65 against a bound of 33.
Nothing leaks: it settles to 0.
The branch's AU-7 test runs without a concurrent drain, so it cannot see this.
**Fix:** release the global word first and the app counter second, so the global word never exceeds the sum of the counters. Add a concurrent drainer to `audit_concurrent_pushes_never_overshoot_the_budget`.

### R3-3 MEDIUM: the over-set is not lock-free; it is a `Mutex<Vec<Weak>>`
The over-set has three hazards.
- **Re-entrant lock hazard.** `pick_victim` (`:354-379`) upgrades and drops `Arc<Shared>` while holding the `over` mutex. If such an Arc is the last strong reference, `Drop for Shared` (`:713-722`) calls `over_remove`, which locks the same mutex on the same thread. That is a self-deadlock on a drain thread.
  The trigger is the production drop order: the receiver drops first and runs `over_remove`; a producer push that is already in flight re-inserts the app through `update_over`; then the sender is retired while it is inside a scan.
  This is not reproduced: about 60M app lifecycles in `p6`/`p7` (3 × 20 s, 3 × 30 s) did not deadlock, so it is substantiated by code reading only.
  **Fix:** collect the upgraded Arcs and drop them after the guard is released, or have `Drop` use `try_lock` and a tombstone.
- **Stale slot.** `over_remove` (`:338-349`) does not update the moved entry's `over_slot` when `upgrade` fails because the entry is dying. Its own later `over_remove` then sees `slot >= len` and leaves a dead `Weak` in the set (the ArcInner leaks until the set is cleared). If another removal happened in between, it removes a *live* entry, which stays marked "in set" and is never re-inserted until it next crosses R. That entry is then not preemptible.
  This was not observed in the `p2` churn test (30 rounds × 2,400 apps, hog always preempted).
- **Window choice.** The 64-entry window with a rotating cursor cannot exclude a holder forever; it visits every entry within ⌈n/64⌉ preemption attempts.
  A requester above R can be refused (flooded) on the attempts in between, which the restated AU-3 allows.
  `update_over`'s check-then-act outside the lock can leave a set membership wrong until the app's next update. An app that stopped pushing and whose actor is stalled keeps its last state.

### R3-4 LOW: the parked table lifecycle
- `park` overwrites an existing `Some(Shed)`, which frees the old one in place on the preempting thread. That needs two preemptions of one victim with no drain between them while the reaper is backed up.
- A victim that never drains keeps one parked table (MBs) outside the budget until it is dropped, and the drop can happen on a preempter that holds the last Arc.

Bounded at one table per victim. **Fix:** if `parked` is occupied, merge into it, or count it and refuse to preempt that victim again.

### R3-5 LOW: test and SPEC hygiene
- `audit_small_holders_are_preempted_for_a_larger_claim` lost its `assert_eq!(hd.events.len(), 100)` for non-shed holders. It was needed only because the filler is in `holders`; when I restored it for the first 15 holders it passes. Restore it.
- The SPEC T4 note says the governor and core placement go in the result meta, but no code records them.
- Single-producer `T7.push_p99` rose from 7 to 17-18 ns (gate 1,000): this is the extra reserve CAS on the hot path.
- `force_charge` writes to the low 32-bit half with no headroom check. `limit.min(POOL_MAX)` leaves 1 unit, so a limit near `u32::MAX` would carry into the reserve half. This is theoretical.

## SPEC changes
The T4 changes match the decision:
- `@10k` is info;
- `@100k ≤ 25` is gated and added to the Gate list;
- the ranking uses the 100k figure;
- nothing else in T4 moved.

The B-3 derivation is unsound as written:
- step 2 (V ≤ apps × R) does not hold at every instant (R3-2);
- the closing paragraph about the reserve not being taken from the pool is false (R3-1).

The bound formula itself is fine as an at-quiescence bound. Both statements must be corrected after the fixes, or rewritten as "at quiescence, plus at most R per concurrently releasing app".

## T7.push4: noise, not a regression
I ran 6 interleaved rounds of `df196a0` (A, no budget), `main` (A with budget wiring), and r3, each round `--only t7 --repeats 3`, in one session at load 8.4-10.9 (`t7-interleaved.txt`).

| Arm | push4 p99, median of rounds (range) | push4 p999, median (range) | push p99 |
|---|---|---|---|
| df196a0 | 4.3 µs (3.0-4.4) | 29.4 µs (26-38) | 7 ns |
| main | 3.7 µs (2.5-6.0) | 36.6 µs (33-49) | 6-11 ns |
| r3 | 2.8 µs (1.4-5.4) | 28.8 µs (26-37) | 17-18 ns |

The same r3 binary measured 1.4 µs and 5.4 µs in consecutive rounds, and main moved between 2.5 and 6.0 µs.
The spread within an arm (about 4×) exceeds the gap between arms, so r3 is not a regression; if anything it is the best arm on p99.
The host is **not** quiet enough for a 5 µs gate on a 4-thread contention percentile.
A real verdict needs:
- load < 1 (no other tenants);
- the `performance` governor;
- producers pinned to P-cores 0-5;
- ≥ 10 interleaved rounds;
- the gate applied to the median of rounds, or as a ratio against the reference arm in the same session.

## Required before merge
1. R3-1: transfer ring credit on fold, then make `p5` and `p8` audit tests.
2. R3-2: swap the release order and add a concurrent drain to the AU-7 test.
3. R3-3: fix the re-entrant drop in `pick_victim`. It is small and closes a self-deadlock path on a drain thread.
4. Correct the SPEC B-3 derivation.

R3-4 and R3-5 can be follow-ups.
