# B-2 bake-off judgement: AT-SPI ingress drain + coalescer

Judge: reviewer. Date: 2026-10-12. Spec: `docs/bakeoff/b2-ingress/SPEC.md`.
Raw output: `bench/results/b2-ingress/judge/` (not committed).

| Candidate | Branch | Commit |
|---|---|---|
| A: raw drain + lock-free ring, consumer-side fold | `worktree-agent-a51937197a8c8515b` | 7083440 |
| B: raw sharded drain + parking_lot slab + reaper | `worktree-agent-a7220896a4b874d87` | ffb618a |
| C: batch-coalescing drain, one publish per app per recv | `worktree-agent-a0827b96f409accfc` | 3c65594 |

## 1. Method

* `. ./env.sh`, then per candidate: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, `tools/check-deps.sh`, and `cargo test -p gorca-ingress --release --tests -- --include-ignored --test-threads=1`
  three times (`judge/{A,B,C}-gates.txt`).
* Full `run-all.sh judge-<X>-r<N>`, two rounds, interleaved A, B, C, A, B, C, strictly sequential, no other
  job of mine running (`judge/runlog.txt`, logs `judge/judge-*.log`, result files `judge/judge-*/`).
  Side by side with the reference: `judge/compare-all.txt` (`compare.py`).
* The host was **not quiet**: load average 9 to 18 at start/end of every run (recorded below). The spec asks
  for < 4; that machine state was not available. All three candidates saw the same noise band, so the
  comparison between them is fair; absolute T3/T4 numbers are pessimistic.
* Extra probes written by the judge, applied temporarily and removed (worktrees left clean):
  B-5 harness experiment (`judge/b5-experiment.txt`), C against main's `bounds.rs` (`judge/C-main-bounds.txt`),
  many-connection AU-3 attack (`judge/judge_many_conns.rs.txt`, `judge/many-connections-probe.txt`),
  preemption cost (`judge/judge_preempt_cost.rs.txt`, `judge/preempt-cost.txt`), drain-thread context
  switches at 100k/s (`judge/drain-syscalls-100k.txt`).

| Run | loadavg start (bench) | loadavg end |
|---|---|---|
| judge-A-r1 | 14.25 | 14.44 |
| judge-B-r1 | 18.35 | 11.25 |
| judge-C-r1 | 14.84 | 10.14 |
| judge-A-r2 | 9.21 | 13.75 |
| judge-B-r2 | 11.40 | 13.49 |
| judge-C-r2 | 12.50 | 13.25 |

## 2. Gates (SPEC section 6), from the judge's runs

| Gate | A | B | C |
|---|---|---|---|
| fmt / clippy -D warnings / check-deps | pass | pass | pass |
| `cargo test --workspace` (debug) | pass (flaky) | **fail in 1st run** (`audit_single_push...` debug) | pass (flaky) |
| debug `audit_latency`, 5 tries at load ~12 | 4/5 | 3/5 | 4/5 |
| conformance debug + release (`gorca-bench-b2`) | pass | pass | pass (with its edited `bounds.rs`; **fails B-3 with main's file**) |
| B-5 giant message (`--include-ignored`) | fail (harness artifact, see 4.2) | fail (harness artifact) | pass (only with its harness edit) |
| AU-1..AU-7 audit tests, release, 3x | all pass | all pass | all pass |
| C.failures, all mixes, 3 buses | 0 / 0 | 0 / 0 | 0 / 0 |
| T3.survived, T5.survived, T5.pending_max (65,536), T5.rss_delta | pass | pass | pass |
| FA.storm / mixed / hung failures | 0 | 0 | 0 |
| T1.sustained_eps (>= 100k) | 400k / 400k | 400k / 400k | 200k / 200k |
| **T4.drain_core_pct@100k (<= 60 %)** | **71.6 / 68.0 FAIL** | **73.4 / 72.1 FAIL** | **73.8 / 73.9 FAIL** |
| Failing metrics of 109 (r1 / r2) | 3 / 3 | 5 / 4 | 5 / 5 |

**No candidate passes the gate strictly**: all fail T4.drain_core_pct, and the reference fails it too (94 %).
The failure is shared physics, not a candidate defect: at 100k paced messages/s every drain thread does
~71k-79k voluntary context switches per second (`drain-syscalls-100k.txt`), i.e. it blocks in `recv`
after almost every message because the feeder writes one message at a time. ~7 us of wake-up cost per
message on a loaded RT kernel is the 70 %. Batching (C) does not help when each `recv` returns one
message. A bounded adaptive spin before blocking, or `SO_BUSY_POLL`-style polling, is the only lever; B
tried and dropped an "adaptive doze". I rank on the remaining metrics and flag T4 as a spec issue (section 6).

## 3. Ranking metrics (median of 3 repeats per run; r1 / r2)

| Metric | Ref | A | B | C | Threshold |
|---|---|---|---|---|---|
| T1.sustained_eps | 200k | **400k / 400k** | **400k / 400k** | 200k / 200k | >= 100k |
| T1.saturation_eps | 306k | **6.04M / 5.75M** | 2.18M / 2.11M | 5.90M / 5.85M | info |
| T2.p99_us@100k | 3,757 | 184 / 179 | 180 / 215 | **173 / 177** | <= 3,000 |
| T2.p999_us@100k | 6,765 | 828 / 703 | 783 / 2,248 | **590 / 621** | <= 10,000 |
| T3.focus_p99_us (mock) | 1,958 | **1,997 / 1,996** | 2,004! / 1,998 | 2,023! / 2,008! | <= 2,000 |
| T3.focus_p999_us (mock) | 2,161 | 2,103 / 2,109 | 2,097 / 2,125 | 2,107 / 3,901 | <= 8,000 |
| T3.focus_p99_us (dbus-broker) | 2,064 | **2,181 / 2,257** | 2,244 / 2,169 | 2,361 / 2,310 | info |
| T4.cpu_ms_per_10k@10k | 225 | **163 / 161** | 174 / 170 | 175 / 167 | <= 25 (all fail) |
| T4.drain_core_pct@100k | 94 | **72 / 68** | 73 / 72 | 74 / 74 | <= 60 (all fail) |
| T7.push_p99_ns | 140 | **6 / 8** | 645 / 689 | 52 / 52 | <= 1,000 |
| T7.push_p999_ns | 657 | **14 / 16** | 2,271 / 2,532 | 87 / 88 | <= 5,000 |
| T7.push4_p99_ns | 21,392 | **4,425 / 4,485** | 18,169! / 18,844! | 18,420! / 18,360! | <= 5,000 |
| T7.push4_p999_ns | 59,857 | **28,038 / 27,705** | 39,605 / 41,855 | 39,075 / 39,543 | <= 50,000 |
| T5.rss_delta_mib@stall_lossless | 21 | 0 / 7 | 15 / 0 | 4 / 8 | <= 256 |

The T3 mock p99 sits on the 2 ms line for every arm including the reference: the probe period is 2 ms and the
consumer-wake lag on a loaded host dominates; the 2-20 us differences are noise. A is the only arm under the
line in both runs.

Reading: A wins or ties every ranking metric except T2 p99.9 (C slightly better). A is the only arm that
meets T7.push4 (4.4 us vs ~18 us for B and C). B's single-producer push is 5x **worse** than the reference
(645 ns p99, 283 us max) and its saturation is a third of A/C. B's sharding is never exercised by the
harness (`runner.rs` sets `max_event_connections = 1`), so its whole extra complexity is unmeasured.

## 4. Rulings

### 4.1 C's edits to `crates/gorca-bench-b2/tests/bounds.rs`

Two edits, both outside C's scope (SPEC section 7: the harness is owned by the orchestrator).

1. `drop(giant)` in B-5: **legitimate**. It removes a harness artifact (4.2) without changing the threshold.
   It is still an unapproved harness edit and must go to main as a harness fix, not via C's branch.
2. B-3 bound raised from `limit + apps` to `limit + apps * (APP_RESERVE + 1)`: **a gate weakening**, though an
   honest one. With main's `bounds.rs`, C fails B-3 three times out of three (`budget used 11484` vs 10,300).
   C's reserve is committed at registration, so late-registered apps get their 4 slots on top of a full
   pool. AU-3 (a per-app reserve) and B-3 (a global hard bound) do conflict as written; the orchestrator must
   amend the spec, either "global bound = limit + apps x (reserve + 1)" or "reserves are carved out of the
   pool". Until then C is non-conforming on B-3. B passes B-3 only because its reserve formula gives
   `10_000 / 16_384 = 0` at the test's limit: with the default budget B has the same relaxation (reserve 16).

### 4.2 B-5 claim of A and B: **true**

As-is, A and B fail with "RSS grew 201 MiB for one 100 MiB message". 100 MiB is the test's own `giant` String,
kept alive, and ~100 MiB is the mock broker's queue: `Link::flush` does `st.q.clear()`, which keeps the
capacity. With `drop(giant)` alone all three pass. With `drop(giant)` plus freeing the mock queue, the
growth is bimodal on **every** candidate (1 MiB or 63-94 MiB over 6 runs each), so the remaining noise is in
the harness sampling, not in the drains; all three skip the giant message while streaming.

Minimal harness fix (main, orchestrator-owned):
* `bounds.rs`: `drop(giant);` right after `f.link.send(...)`.
* `mockbus.rs` `Link::flush`: after `st.q.clear()`, `if st.q.capacity() > 4 * 1024 * 1024 { st.q = Vec::new(); }`.
* Before sampling RSS in B-5, wait until `f.link.pump() == 0` (or `flush_all`) so the queue is freed.
Keep the 150 MiB threshold until the bimodality is understood; then tighten to e.g. 48 MiB (above the 16 MiB
per-message bound, well below a buffered 100 MiB message).

### 4.3 B's `max_event_connections` default 1 -> 4: **not acceptable**

SPEC section 3: candidates may add `DrainConfig` fields with defaults, "never remove or change one". Changing
an existing default is a change of observable behaviour: by default B now issues extra `AddMatch
sender=...` subscriptions and opens secondary connections, doubling broker work for a migrated sender. The
harness pins the value to 1 (`runner.rs:46`), so no benefit was measured. Keep default 1; opt-in only.

### 4.4 A's AU-3 by preempting the largest holder: **unsound against a hostile app with many connections**

Preemption only fires when the largest holder holds >= 128 (`2 * RESERVED_MIN`, `table.rs:244`) and the
requester holds < 64. A hostile process that opens ~2,300 connections and shapes each to hold ~121 slots
(120 distinct Window events, then 1,200 repeats of one key, so ring overflow folds stay below the threshold)
exhausts the pool with no eligible victim. Measured (`many-connections-probe.txt`, shaped case):

| | A | B (reserve 16) | C (reserve 4) |
|---|---|---|---|
| victim events kept, of 20 | **0** | 16 | 4 |
| victim announcement dropped | yes | yes | yes |
| used vs limit 262,144 | 262,144 | 264,272 | 262,676 |

The unshaped attacks (4 big apps; 2,100 or 4,200 apps of ~128) do not beat A. B and C keep their reserve but
also drop the announcement as soon as a victim has more pending events than the reserve, so **none of the
three meets the AU-3 sentence "one app's overflow never drops another app's events"**; the AU-3 audit test
pushes only 3 victim events and does not catch it.

A's preemption also breaks AU-6 under attack: a victim's `drain()` that preempts a 65k-slot holder took
**0.7 / 0.9 / 2.0 ms** (`preempt-cost.txt`; B 5-8 us, C <= 1 us). `shed()` frees the victim's whole table while
the preempting app's table lock is held, so the drain thread pushing into that app stalls too.

A also keeps up to `RING_CAP` (1,024) records per app in the ring **uncharged** to the budget (charged only on
fold), so `SlotBudget::used` under-reports by up to 1,024 x apps. Bounded per app, but outside the global bound.

Fix for whichever arm is merged: a flat reserve sized at least to the announcement cap plus a few lossless
events (B's 16, charged inside the pool or on top with the bound documented), plus preemption of the largest
holder with **no minimum victim size**, done incrementally (shed in chunks, or hand the victim's table to the
reaper) so it stays under 250 us. Long term, fairness should be per client process (`GetConnectionUnixProcessID`),
not per connection.

### 4.5 Is `SlotBudget` wired into the benchmarked path?

**No, in none of the three.** The harness router (`crates/gorca-bench-b2/src/rig.rs:167`) calls plain
`ingress(cfg)`, which is budget-less in A, B and C. The CAS on a process-wide counter, A's preemption and C's
`committed` accounting are therefore not in any T1-T7 number. Production wiring: A provides
`gorca_dbus::drain::IngressFactory`, B `gorca_ingress::IngressFactory`, neither used by a production router;
only C wires it into production (`gorca-connector-api::app_channel` now uses a process-wide
`global_budget()` `OnceLock`), which is the right place but an out-of-scope edit and a hidden singleton.
Harness fix: `rig.rs` creates every ingress through one shared `SlotBudget::new(GLOBAL_SLOT_BUDGET)`;
re-run T7.push4 and T1 after it.

## 5. Correctness, safety, simplicity

* `unwrap`/`expect`/`panic!` in non-test code: none in any arm (clippy enforces). No `unsafe` added.
  Every thread named `gorca-*`; every start-up step (SASL, Hello, AddMatch) runs under `connect_deadline` in all three.
* Wildcard `_ =>` arms on `CoalesceKey`/`TextDelta` matches in A (`table.rs:426,434`) and C
  (`coalesce.rs:412,609,685,933`): low; they default new variants silently. Prefer exhaustive arms.
* C changed the public `default_match_rules()` output (5 rules, adds `NameOwnerChanged`); A and B add that
  rule internally. Low, but A/B's way keeps the API stable.
* B: two-connection hand-over with serial watermarks (`handover.rs`, 251 lines) is the highest-risk code
  in the bake-off and is covered only by B's own unit tests, never by the harness.

Size (non-blank, non-comment, non-`#[cfg(test)]` lines in `gorca-dbus/src/drain`, `gorca-ingress/src`,
`gorca-rt/src`; main = 1,589):

| | A | B | C |
|---|---|---|---|
| net non-test lines vs main | +1,247 | +1,307 | +1,068 |
| raw `numstat` added (incl. tests) | 2,658 | 3,061 | 2,147 |
| new deps | crossbeam-queue (+smallvec use) | parking_lot, rustc-hash | none |

All within the 1,600 budget by the net measure. C is the smallest; B the largest for the least measured gain.

Extensibility: A's split (ring push / table fold on the consumer) makes per-app deregistration and lazy text
(R-6) easy because the producer side carries raw records. C's `BatchTable` gives an explicit batch API
(`publish`) that a future io-uring multishot reader can feed. B's reaper thread is a reusable idea for any
large free on a latency path.

## 6. Scores

Weights: spec metrics 50, correctness/safety 30, simplicity 10, extensibility 10.

| | Metrics (50) | Correctness (30) | Simplicity (10) | Extensibility (10) | Total |
|---|---|---|---|---|---|
| A | 46 | 18 (AU-3 starvation, AU-6 break under preemption, uncharged ring) | 7 | 9 | **80** |
| C | 38 | 22 (B-3 non-conforming, reserve 4 too small, scope edits) | 9 | 8 | 77 |
| B | 30 | 20 (debug audit fail, unmeasured hand-over, default change) | 5 | 6 | 61 |

## 7. Winner and ports

**Winner: A, conditionally.** Merge A's branch as the base. It is the only arm under T7.push4, has the best
T1, T4 and saturation, and ties T2/T3. Before merge (blocking):

1. Replace "preempt only if victim >= 128" with: a flat per-app reserve (from B, 16 slots, sized from the
   announcement cap) plus preemption with no minimum victim size.
2. Make `shed()` O(1) under the lock: swap the victim's table out and free it on a reaper thread (port B's
   `reaper.rs`, 38 lines), so a preempting drain stays under 250 us.
3. Charge ring records to the budget at push (or bound `RING_CAP x apps` in the spec explicitly).

Port from C: the production wiring of the budget in `gorca-connector-api::app_channel` (with an explicit
budget passed in instead of a `OnceLock` singleton), and C's `batch_publish.rs` equivalence tests as a model
for A's fold-equivalence tests. C's one-publish-per-recv batching is worth revisiting only after the harness
feeds multi-message recvs (it gave the best T2 p99.9 but no CPU win here).
Port from B: `reaper.rs`, the `audit_concurrent_budget.rs` AU-7 test (A has only a unit test), and
NameOwnerChanged secondary retirement if sharding is ever revisited. Do not port the sharded connections.

Harness/spec actions for the orchestrator: B-5 fix (4.2); budget in `rig.rs` (4.5); resolve AU-3 vs B-3 (4.1);
strengthen the AU-3 audit test with a victim of > 16 events and a many-connection shaped attack; reconsider
T4.drain_core_pct <= 60 % with one-message-per-write feeders (every arm, ~75k context switches/s); move the
debug-build `audit_latency` out of `make check` or give it a debug threshold.

## 8. Risks / unverified

* Host load 9-18 throughout; no quiet-machine run. T3/T4 margins are within noise.
* Only two full runs per candidate (6 repeats per metric).
* Real-bus T3 numbers are info only; NameOwnerChanged route drop was verified by the candidates' unit tests, not by me.
* B's hand-over correctness under real migrations was not exercised by anyone.
