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
