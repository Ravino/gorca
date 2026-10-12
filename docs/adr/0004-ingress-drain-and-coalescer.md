# ADR-0004: AT-SPI ingress — raw-socket drain and lock-free per-app coalescer

- **Status:** Accepted (2026-10-12), **conditional** on the blocking fixes BF-1…BF-4 (§5). Until all four are merged and the re-measurement in §9 passes, the decision holds for the drain and the ring, and the budget/reserve model is *specified but not yet conforming*.
- **Date:** 2026-10-12
- **Deciders:** project owner (bake-off approval recorded in `docs/decisions.md`)
- **Resolves:** ADR-0001 §15 bake-off **B-2** ("Event drain + ingress coalescer"). Amends ADR-0001 §5.3, §6.1, §6.2, §6.3 (the amended text is already in ADR-0001; see its changelog).
- **Related:**
  - `docs/bakeoff/b2-ingress/SPEC.md` (SPEC): fixed API, executable model, §3.6 hard bounds, §3.7 audit requirements AU-1…AU-7, metrics T1…T7, FA, C.
  - `docs/reports/2026-10-12-b2-bakeoff-judgement.md` (JUDGE): reviewer's re-runs, rulings, scores, ports.
  - `docs/bakeoff/b2-ingress/candidate-a/RESULTS.md`: winner's own runs. Raw judge output: `bench/results/b2-ingress/judge/` (not committed).
  - `docs/platform/dbus-broker-a11y.md` (F-3: unbounded broker buffering, then disconnect).

## 0. Changelog

| Date | Change |
|---|---|
| 2026-10-12 | Accepted, conditional on BF-1…BF-4. ADR-0001 §6.3 rewritten to carry rule H, `SlotBudget`, the per-app reserve and preemption model (IB-7), bounds IB-1…IB-11 and AU-1…AU-7 as invariants, plus AU-8 (counter exactness, from SPEC §3.6 row B-1). AU-3 is restated precisely (see §6 item 7). |

## 1. Context

A gorca event connection on the a11y bus receives every AT-SPI signal of every application. dbus-broker buffers what a slow reader has not read without bound and then **disconnects** it (F-3, ADR-0001 §1.2), and a disconnect loses every application at once. ADR-0001 therefore requires a dedicated drain that never waits downstream (D-1…D-3) feeding a per-application coalescing ingress (§6.3, R-1…R-6), and left the implementation to bake-off B-2.

The SPEC fixed the public API (`gorca_dbus::drain::{spawn_drain, DrainConfig, Router, AppRoute}`, `gorca_ingress::{ingress, IngressSender, IngressReceiver, Drained}`), made a model (`crates/gorca-bench-b2/src/model.rs`) normative for single-producer output, and added hard bounds against hostile input after a security audit of the reference (SPEC §3.6, §3.7). Three candidates competed:

| Arm | Drain | Ingress |
|---|---|---|
| **A** (ADR arm b) | own D-Bus framing on one `gorca-drain-0` thread, header peek, 1 MiB `recv` | bounded lock-free MPSC ring per app (`crossbeam_queue::ArrayQueue`, `RING_CAP` 1,024); the coalescing fold runs on the consumer, or on a producer in a bounded chunk when the ring is full |
| **B** (ADR arm c) | A's framing plus up to 4 sharded event connections, heavy senders migrated by `AddMatch sender=` with serial watermarks | sharded `parking_lot` slab + reaper thread for large frees |
| **C** | one thread, frames a whole `recv` batch, coalesces into thread-local per-app tables | one merged batch published per app per `recv` |

Baseline: the reference (arm a) is zbus `MessageStream` + `Mutex` coalescer; it costs ~15 µs per message in zbus decoding, disconnects at 400k events/s, uses 94 % of a core at 100k/s and has a 21 µs contended push p99 (SPEC §9).

## 2. Decision

**D-1. Candidate A is the B-2 implementation.** Merged on `main` (`df196a0`).

**D-2. Drain.** One event connection, one dedicated thread `gorca-drain-0`, our own D-Bus wire reader (`crates/gorca-dbus/src/drain/{raw,wire,map}.rs`), no zbus on the event path:
- blocking `recv` into a 1 MiB buffer (D-1 floor 64 KiB); framing from the 16-byte primary header, partial messages carried over, both endiannesses;
- header peek (sender, path, interface, member, signature); body fields read only for the members that carry a payload the coalescer needs (SPEC §3.2); `Focus`, `Window`, `Document`, `SelectionChanged` bodies are never touched;
- a message over 16 MiB (`MAX_MESSAGE_BYTES`) is skipped while streaming and counted `malformed`; it is never held in memory;
- `SIOCINQ` sampled per batch, exported as `DrainStats::backlog_max`/`shed_starts` (D-2);
- a sender route cache of at most `MAX_ROUTES` = 4,096 entries per connection; the drain adds a `NameOwnerChanged` match internally (the public `default_match_rules()` is unchanged) and retires a route when its sender disconnects (AU-2);
- only `Hello` and `AddMatch` are ever sent, all under `connect_deadline` (D-3).

**D-3. `max_event_connections` stays 1.** Sharded event connections (arm c) are **not** adopted. The field remains in `DrainConfig` for compatibility; values > 1 are unsupported until a future ADR shows a measured need (§4, alternative B).

**D-4. Ingress.** Per app, `push` appends one compact record to a bounded lock-free ring and wakes the consumer if it was idle (one CAS, no lock, no hash, no allocation). The SPEC §4.2 fold (R-LWW, R-OR, R-TEXT, R-1, announcements, R-3 `Flooded`, rule H) is applied in ring order by whoever folds: the app actor in `drain()` (chunks of 256), a producer that finds the ring full (one chunk of 128), or a producer serving an app that already holds ≥ `RING_CAP` slots (eager fold: the app is degraded and exact accounting matters more than push cost). Because the fold is in ring order, a single producer sees exactly the model's sequence of states wherever the fold runs.

**D-5. Bounds and fairness model** (normative text in ADR-0001 §6.3, summarised here):
- per app: `max_keys` 4,096 → `Flooded` (R-3); `hard_cap` 65,536 → rule H (dirty-all, nothing queued except the single `Focus` slot);
- process-wide: one `SlotBudget` (`GLOBAL_SLOT_BUDGET` 262,144) shared by every ingress; slots, text deltas beyond the first of a slot, and ring records are charged;
- per app reserve **R = 32** slots (`IngressConfig` default) outside the shared pool; preemption of the largest holder above its reserve when the pool is exhausted; preempted tables are swapped out in O(1) and freed on a reaper thread.

**D-6. Production wiring.** `gorca-daemon` creates one `Arc<SlotBudget>` at start-up and passes it explicitly to the router / app-channel factory (`gorca-connector-api::app_channel` takes it as a parameter). There is no process-wide singleton (`OnceLock`, `static`) for the budget: tests and the bench rig create their own, and two budgets in one process are legal (isolated harnesses).

## 3. Why A

Judge's re-runs, median of 3 repeats per run, two interleaved rounds (r1 / r2), host load 9–18 for every arm (JUDGE §1, §3):

| Metric | Ref | **A** | B | C | Threshold |
|---|---|---|---|---|---|
| T1.sustained_eps | 200k | **400k / 400k** | 400k / 400k | 200k / 200k | ≥ 100k |
| T1.saturation_eps | 306k | **6.04M / 5.75M** | 2.18M / 2.11M | 5.90M / 5.85M | info |
| T2.p99_us@100k | 3,757 | 184 / 179 | 180 / 215 | **173 / 177** | ≤ 3,000 |
| T2.p999_us@100k | 6,765 | 828 / 703 | 783 / 2,248 | **590 / 621** | ≤ 10,000 |
| T3.focus_p99_us (mock) | 1,958 | **1,997 / 1,996** | 2,004 / 1,998 | 2,023 / 2,008 | ≤ 2,000 |
| T4.cpu_ms_per_10k@10k | 225 | **163 / 161** | 174 / 170 | 175 / 167 | ≤ 25 (all fail) |
| T4.drain_core_pct@100k | 94 | **72 / 68** | 73 / 72 | 74 / 74 | ≤ 60 (all fail) |
| T7.push_p99_ns | 140 | **6 / 8** | 645 / 689 | 52 / 52 | ≤ 1,000 |
| T7.push4_p99_ns | 21,392 | **4,425 / 4,485** | 18,169 / 18,844 | 18,420 / 18,360 | ≤ 5,000 |
| T7.push4_p999_ns | 59,857 | **28,038 / 27,705** | 39,605 / 41,855 | 39,075 / 39,543 | ≤ 50,000 |
| Correctness C.failures, FA.*, AU-1…AU-7 (3×) | pass | pass | pass (debug audit flaky) | pass (B-3 fails with main's harness) | 0 |
| Scores (metrics 50 / correctness 30 / simplicity 10 / extensibility 10) | — | **80** | 61 | 77 | — |

- A is the only arm that meets the contended-push bound (T7.push4 p99 4.4 µs vs ~18 µs), which is the cost a multi-producer future (several drains, synthetic producers, extensions) would pay on every event.
- A wins or ties every ranking metric except T2 p99.9, where C is ~150–200 µs better at 100k/s, far inside the 10 ms bound.
- A's split (raw records on the producer side, fold on the consumer) keeps per-app deregistration (D-5) and lazy text (R-6) cheap: records carry raw handles and the fold is the only place that interprets them (JUDGE §5).
- A's correctness defects are local to the budget layer and have known fixes (§5); its drain and ring have none.

### 3.1 The shared T4 failure is not a reason to reject the decision

Every arm and the reference fail `T4.drain_core_pct@100k ≤ 60 %`. The feeder writes one message per `write`, so the drain blocks in `recv` after nearly every message: ~71k–79k voluntary context switches per second at 100k/s, ~7 µs of wake-up each on a loaded RT kernel (JUDGE §2, `judge/drain-syscalls-100k.txt`). Batching (C) cannot help when each `recv` returns one message. This is a property of the feeder and of blocking I/O, not of the drain's per-message work (A's framing is ~0.3 µs per message, T1.saturation 6M msgs/s). It is tracked by metric IM-6 and is not a condition of this ADR; a bounded adaptive spin or `SO_BUSY_POLL` is a later, separately measured experiment.

## 4. Alternatives considered

| Alternative | Measured result | Why not |
|---|---|---|
| **Reference (arm a):** zbus `MessageStream` + `Mutex` coalescer | 15 µs/message; T1 200k (disconnect at 400k); 94 % core; T2 p99@100k 3.8 ms (fails); T7.push4 21 µs (fails) | zbus decodes every message into an owned `Message`; the lock serialises producers. Rejected by ADR-0001 as the default and confirmed by numbers. |
| **B:** sharded event connections + sharded slab + reaper | T1 equal to A, saturation ⅓ of A; single-producer push 645 ns p99, 5× worse than the reference; T7.push4 18 µs (fails); debug audit failed in 1st run; 251-line hand-over never exercised by the harness (`max_event_connections` pinned to 1) | All extra complexity is unmeasured; it adds broker work (a second subscription per migrated sender) and changed a `DrainConfig` default, which the SPEC forbids. Kept from B: `reaper.rs`, the AU-7 concurrency test, `NameOwnerChanged` secondary retirement *if* sharding is ever revisited. |
| **C:** batch coalescing, one publish per app per `recv` | best T2 p99.9; smallest code (+1,068 lines, no new deps); T1 200k; T7.push4 18 µs (fails) | No CPU win with one-message `recv`s; its reserve (4) is committed at registration on top of a full pool and fails B-3 with the harness's bound; its production budget is a hidden `OnceLock` singleton. Kept from C: production budget wiring (made explicit, D-6) and the batch-equivalence tests as the model for A's fold-equivalence tests. Batch publishing is to be revisited once the harness feeds multi-message `recv`s (IM-7). |
| `DashMap` coalescer alone | not built (SPEC §10 "Not allowed" as a whole design) | Shard locks still serialise per-key writers; no answer to F-3. |
| io-uring multishot receive | not built | Gains only where `recv` count dominates, i.e. the same T4 physics; deferred with C's batching. |

## 5. Conditions: blocking fixes (the decision holds only with all of them)

| # | Defect in A as judged (JUDGE §4.4, §4.5) | Required fix | Acceptance test |
|---|---|---|---|
| **BF-1** | AU-3 starvation: preemption fired only if the largest holder held ≥ 128 slots and the requester < 64. A process with ~2,300 connections of ~121 slots each exhausted the pool with no eligible victim; the bystander kept 0 of 20 events. | Flat per-app reserve R = 32 (2 × `announcement_cap`, so announcements stay protected alongside other pending events) **outside** the shared pool, plus preemption of the largest holder at any size above its own reserve (no 128 threshold). Exact rules: ADR-0001 §6.3, IB-6, IB-7. | AU-3 test strengthened: bystander with > R pending events, and the many-connection shaped attack (JUDGE `judge_many_conns.rs.txt`); bystander keeps ≥ min(pending, R) events and never loses one silently. |
| **BF-2** | AU-6 broken under attack: `shed()` freed a 65k-slot table under the preempting app's lock: 0.7–2.0 ms critical section, stalling the drain. | Preemption swaps the victim's table out under its lock in O(1) and hands it to a reaper thread (`gorca-reaper`, port of B's `reaper.rs`). | `audit_drain_critical_section_is_bounded` extended with a preempting push against a full 65k-slot holder: worst case ≤ 250 µs. |
| **BF-3** | Ring records (≤ 1,024 per app) were charged only on fold, so `SlotBudget::used` under-reported by up to `RING_CAP` × apps and AU-1 equality did not cover them. | Charge every record at `push`; release on fold (the folded slot then holds its own charge, or the record is coalesced and its charge returned). AU-1 becomes Σ(live + ring) == `used`. | AU-1 and AU-7 tests over pushes that stay in the ring; T7.push4 re-measured (§9). |
| **BF-4** | `SlotBudget` was wired into no benchmarked path and no production router. | D-6: one explicit `Arc<SlotBudget>` from `gorca-daemon` through the app-channel factory; the bench rig (`rig.rs`) creates every ingress through one shared budget. | `cargo test` of the wiring; full `run-all.sh` with the budget in the rig (§9). |

Ports that are not blocking but are owed: B's `audit_concurrent_budget.rs` (AU-7 under contention), C's equivalence tests rewritten for A's fold, exhaustive arms instead of `_ =>` on `CoalesceKey`/`TextDelta` matches (`table.rs`, JUDGE §5).

**If a fix fails its test.** BF-3 is the only fix that touches the push hot path (one more CAS on a shared counter). If T7.push4 p99 goes above 5 µs with it, the permitted fallback is per-app credit leasing (an app takes credits from the pool in chunks, e.g. 64, and returns them on drain), which keeps AU-7 exact; AU-1 is then restated as Σ(live + ring + leased-unused) == `used`, an amendment that needs a changelog entry here. Any other failing fix reopens this ADR; it does not silently weaken an invariant.

## 6. Harness and SPEC actions this decision depends on (orchestrator-owned)

From JUDGE §4.1, §4.2, §7. Listed so the decision's preconditions are traceable; this ADR does not edit the SPEC or the harness.

1. **AU-3 vs B-3 conflict.** ADR-0001 §6.3 resolves it: reserves sit **outside** the pool, so the global bound is `GLOBAL_SLOT_BUDGET + apps × (R + 1)` (the +1 is each app's `Focus` slot). The B-3 test bound must be amended to that formula.
2. B-5 giant-message RSS artifact: `drop(giant)`, free the mock broker queue capacity, sample after `pump() == 0`.
3. Budget in `rig.rs` (BF-4).
4. AU-3 audit test strengthened (BF-1).
5. Debug-build `audit_latency` moved out of `make check` or given a debug threshold.
6. `T4.drain_core_pct` either measured with a feeder that writes several messages per `write`, or reported as info until M-11 is measured on a real VTE storm (IM-6).
7. **AU-3 wording.** The SPEC sentence "one app's overflow never drops another app's events" is unattainable with any finite reserve (all three arms violated it, JUDGE §4.4). ADR-0001 §6.3 restates it: an app at or below R is never affected; an app above R can be affected only by preemption, which is flooded + dirty-all, never silent.

## 7. Consequences

**Positive**
- Broker disconnection is pushed far out: 400k events/s sustained with a 1 MiB backlog bound, 6M messages/s saturation, against a requirement of 10k (product goal) and 100k (M-3 burst).
- The push hot path is one CAS: producers never contend on a lock, so additional producers (a second connector, synthetic events, Newton) do not degrade the drain.
- Every resource a hostile app can grow has a hard bound and a degradation path that ends in resync, never in unbounded memory or a dropped lossless event (ADR-0001 §6.3 IB-1…IB-11).
- No new transport dependency in `gorca-ingress`; one new crate dependency (`crossbeam-queue`).

**Negative / costs**
- gorca owns a D-Bus wire reader (~1,250 net lines with the ingress). The SPEC conformance suite (C-D1…C-D13, framing down to 1-byte pieces, both endiannesses, 3 MB messages) is now a permanent regression gate for it.
- The fold runs on whichever thread finds work, so a producer can occasionally pay a fold chunk (≤ 128 records) on the drain thread. AU-6 (≤ 250 µs) bounds it; IM-3 watches it.
- Fairness is per **connection**, not per process: a process can still open many connections. Each one is held to R + `hard_cap` and preemption hits the largest first, so the damage is bounded, but per-process fairness (`GetConnectionUnixProcessID`) is future work. The lookup is a method call, so it must be done by the router on a call connection, never by the drain (D-3).
- `Flooded` and preemption trade precision for safety: a preempted app is fully dirty and resyncs lazily, which costs IPC later. Preemption is expected only under attack or pathological floods (IM-4).
- The single drain thread is the one serial point for all a11y-bus events. At 6M msgs/s saturation this is a non-issue for any foreseeable desktop; if real-bus measurements show otherwise, sharding returns as a new bake-off, not as a toggle.

## 8. Failure modes

| Failure | Behaviour |
|---|---|
| Broker never answers `AddMatch` | `spawn_drain` returns `Err` within `connect_deadline` (C-D12). |
| Socket EOF / error | Drain ends with `DrainEnd::Disconnected`, never panics (C-D11); supervisor reconnects; every app dirty-all (ADR-0001 §6.5). |
| Panic in the drain thread | Caught at the thread boundary and reported; same as disconnect. |
| Malformed or > 16 MiB message | Skipped while streaming, `malformed` += 1; the stream re-synchronises on the next header. |
| App actor stalls | Ring fills, producers fold; table hits `max_keys` → `Flooded`, then `hard_cap` → dirty-all; drain never waits (C-D13: broker backlog < 1 MiB). |
| Pool exhausted | IB-7: reserve, then preemption of the largest holder, else the requester floods. Lossless `Focus` always queues. |
| Reaper thread slow or dead | Its channel is bounded; on a full channel the swapped-out table is attached to the victim's next `Drained` and freed by the victim's app actor on `workers`, never on the drain. The watchdog (ADR-0001 §6.6) monitors `gorca-reaper`. |
| Sender churn (bus names) | Route cache capped at 4,096 per connection, dropped and rebuilt; disconnected senders retired via `NameOwnerChanged` (AU-2). |

## 9. Metrics to validate

| ID | Metric | Target | Source |
|---|---|---|---|
| IM-1 | Re-run of the full B-2 harness on `main` after BF-1…BF-4, quiet host (load < 4) | every gate metric except T4.drain_core_pct passes; T7.push4 p99 ≤ 5 µs **with** the budget in the rig | `run-all.sh`, reviewer |
| IM-2 | AU-1…AU-7 audit tests, release and debug, 3× | all pass, including the strengthened AU-3 and the preemption AU-6 case | `crates/gorca-ingress/tests/audit_*.rs` |
| IM-3 | Worst push / drain critical section, including producer-side fold and preemption | ≤ 250 µs at `hard_cap` | AU-6 tests + tracing on the real bus |
| IM-4 | Preemptions per hour in a normal desktop session (Shell, Terminal, Firefox, Chromium) | 0; any non-zero count is logged with the holder's app | telemetry counter |
| IM-5 | Global bound | `SlotBudget::used ≤ GLOBAL_SLOT_BUDGET + apps × (R + 1)` at every sample | `check()` + B-3 |
| IM-6 | M-11 drain occupancy on a real VTE storm (`yes \| head -c 1G`) plus the 100k ev/s synthetic storm | < 60 % of one core, backlog p99 < 1 MiB, no disconnect | ADR-0001 M-11 |
| IM-7 | Batching revisit trigger | if the real-bus `recv` returns ≥ 4 messages on average under storm, re-evaluate C-style batch publishing against IM-6 | drain stats `messages / batches` |
