# Bake-off B-2: event socket drain and ingress coalescer

Status: spec and harness ready, no candidates yet (steps 1 and 2 of `.claude/skills/bakeoff/SKILL.md`).
Decision under test: ADR-0001 section 15, B-2 ("Event drain + ingress coalescer").
Harness: `crates/gorca-bench-b2` and `docs/bakeoff/b2-ingress/harness/` (how to run: its README).
Baseline: `bench/results/b2-ingress/reference/` (the reference arm, numbers in section 9).

## 1. Problem

The a11y bus broadcasts every AT-SPI event of every application to every subscriber. dbus-broker
buffers what a slow reader has not consumed **without bound and then disconnects the reader**
(F-3), and a disconnect loses the whole reader. So gorca needs a component that

1. reads the event socket as fast as the kernel delivers it, whatever the downstream does (D-1);
2. turns the byte stream into per-application `NodeEvent`s (header peek plus the first body fields);
3. coalesces, dedupes and drops superseded events per application, bounded in memory (R-1..R-5);
4. never loses the events whose loss would corrupt the reader's state (focus, removal, windows);
5. does all of it cheaply enough that the keypress-to-speech budget (p99 < 15 ms gorca-side) is not
   spent here, and sustains a storm of 100k events/s on one application (M-3, M-11).

B-2 picks the implementation of (1)-(4). The API is fixed (sections 3 and 4); the candidates differ
only in the internals of `gorca-dbus::drain` and `gorca-ingress`.

Out of scope: method calls and replies (call connections, D-3/D-4/D-7), the P2P pool, `RegisterEvent`
(D-5), `NameOwnerChanged` handling and app lifecycle, the app actor, the mirror. The unicast
`KeyEvent` signal is the input lane's.

## 2. Platform facts and ADR rules that bind the design

| Rule | Consequence for B-2 |
|---|---|
| F-3 dbus-broker queues without bound, then disconnects; same-uid senders are never throttled | Reading must never wait on anything downstream. Overload is handled **after** reading. A disconnect is a hard failure of any scenario. |
| D-1 batches of at least 64 KiB per `recv`, header peek only, `push` O(1) and never awaits | The drain does not deserialise bodies. The bodies of `Focus`, `SelectionChanged`, `Window` and `Document` events are not even looked at. |
| D-2 `SIOCINQ` sampled every batch, shed signal over a watermark | The drain exports its own backlog (`DrainStats::backlog_max`, `shed_starts`). |
| D-3 only `Hello` and `AddMatch` on an event connection | No method call may be issued by the drain, so a hung application can never block it. |
| D-6 own connections, `max_queued >= 4096` | Whatever queue a candidate has between the socket and `push` is bounded and never back-pressures the socket. |
| R-1..R-6, section 6.3 | The coalescing rules of section 4, which the executable model in `crates/gorca-bench-b2/src/model.rs` implements. |
| PC-2 no global event bus | The drain routes by sender to the per-app ingress; no channel carries all events of all apps. |
| PC-4 hot lanes are dedicated OS threads | Drain threads are named `gorca-drain-N` and are not async tasks on the shared runtime. |
| PC-6 / A5 memory is spent for latency; PC-7 every queue bounded | Generous buffers are fine; unbounded growth is a bug and a failing metric (T5). |
| ADR section 11 rule 1 | `gorca-ingress` must not depend on zbus or any transport. |
| ADR section 11 rule 6 | No `unwrap`, `expect`, `panic` outside tests; `unsafe` only in `gorca-dbus` and `gorca-rt`, per module, each block with a `// SAFETY:` comment. |

## 3. The drain interface (`gorca_dbus::drain`)

The public items below exist today (reference arm) and **must keep their signatures**; a candidate
may add items and fields to `DrainConfig` with defaults, never remove or change one.

```rust
pub struct AppRoute { pub app: AppId, pub tx: IngressSender }
pub trait Router: Send + Sync + 'static { fn route(&self, sender: &str) -> Option<AppRoute>; }

pub struct DrainConfig {
    pub address: String,                  // unix:path=...
    pub match_rules: Vec<String>,         // default_match_rules(): Event.{Object,Window,Focus,Document}
    pub recv_bytes: usize,                // >= 64 KiB per recv (D-1)
    pub rcvbuf_bytes: Option<usize>,      // SO_RCVBUF
    pub shed_watermark: usize,            // SIOCINQ bytes (D-2)
    pub connect_deadline: Duration,       // connect + auth + Hello + all AddMatch
    pub max_event_connections: u8,        // 1 = a single drain; arm c may open up to this many
}
pub fn spawn_drain(cfg: DrainConfig, router: Arc<dyn Router>) -> Result<DrainHandle, DrainError>;

impl DrainHandle {
    pub fn stats(&self) -> DrainStats;    // counters, see 3.5
    pub fn end(&self) -> DrainEnd;        // Running | Stopped | Disconnected(reason)
    pub fn stop(self) -> DrainStats;      // join all threads
}
pub fn map_signal(app: AppId, v: &SignalView<'_>, out: &mut SmallVec<[NodeEvent; 2]>) -> Result<(), Malformed>;
pub fn local_id(path: &str) -> LocalId;   // reference mapping; a candidate may inline its own
```

`DrainShared`/`DrainCounters` (also public) carry the counters and the end state; a candidate fills
them and hands them to `DrainHandle::new`.

### 3.1 Identity

`LocalId` of an object is the **FNV-1a 64-bit hash of the object path bytes** (offset
`0xcbf29ce484222325`, prime `0x100000001b3`). It is total, stable and needs no interning. The
`AppId` comes from `Router::route(sender)`; the node of an event is `NodeKey { app, local }`.
The generation rule of ADR I-1 is the connector's business: the harness never reuses a removed
node id.

### 3.2 Mapping table

Only **signals** (message type 4) whose interface starts with `org.a11y.atspi.Event.` are
considered. The body signature is `siiva{sv}`: `detail: s`, `d1: i32`, `d2: i32`, `any_data: v`,
`props: a{sv}`. The properties are never read. Negative `d1`/`d2` become `0` (F-4: never forward a
negative offset). One message yields zero, one or two events, in the order shown.

| Interface . member | Condition | Events | Body fields read |
|---|---|---|---|
| `Focus.Focus` | | `FocusChanged{node}` | none |
| `Object.StateChanged` | `detail == "focused"` and `d1 == 1` | `FocusChanged{node}` | detail, d1 |
| | `detail == "defunct"` and `d1 == 1` | `NodeRemoved{node}` | detail, d1 |
| | otherwise | `PropertiesChanged{node, STATES}` | detail, d1 |
| `Object.PropertyChange` | `accessible-name` / `-description` / `-role` / `-value` | `PropertiesChanged{node, NAME / DESCRIPTION / ROLE / VALUE}` | detail |
| | any other detail | `PropertiesChanged{node, ATTRS}` | detail |
| `Object.ChildrenChanged` | | `ChildrenChanged{node}` | detail |
| | `detail` starts with `remove` **and** `any_data` is `(so)` | additionally `NodeRemoved{node: hash(child path)}` (same app) | detail, any_data |
| `Object.TextChanged` | `detail` starts with `insert` | `TextChanged{node, Insert{offset: d1, len: d2, text: None}}` | detail, d1, d2 |
| | `detail` starts with `delete` | `TextChanged{node, Delete{...}}` | detail, d1, d2 |
| | otherwise | nothing | detail |
| `Object.TextCaretMoved` | | `CaretMoved{node, offset: d1, origin: Unknown}` | d1 |
| `Object.SelectionChanged`, `Object.TextSelectionChanged` | | `SelectionChanged{node}` | none |
| `Object.ActiveDescendantChanged` | `any_data` is `(so)` | `ActiveDescendantChanged{container: node, child: Some(hash(path))}` | any_data |
| | otherwise | `... child: None` | any_data |
| `Object.Announcement` | | `Announcement{node: Some(node), text: any_data if "s" else "", politeness: Assertive if d1 == 2 else Polite}` | d1, any_data |
| `Window.{Activate,Deactivate,Create,Destroy}` | | `WindowEvent{node, kind}` | none |
| `Document.{LoadComplete,Reload,LoadStopped}` | | `DocumentEvent{node, kind}` | none |
| anything else | | nothing (counted as `ignored`) | none |

A body that is truncated, has an invalid string, or does not start with `siiva` for a member that
reads it is **malformed**: counted in `DrainStats::malformed`, no event produced, framing and the
neighbouring messages unaffected. Members that read no body must not fail on a bad body. Text
payloads stay inside the message bytes (`text: None` here; R-6 lazy text is the next step).

### 3.3 Behaviour requirements (conformance tests C-D1..C-D13)

* **Framing.** Messages of either endianness arrive in arbitrary pieces (down to 1 byte), several
  per read, or one larger than the read buffer (tested with 3 MB; the D-Bus limit is 128 MiB).
* **Routing.** `Router::route` is called at most once per distinct sender per drain connection; the
  answer, including `None`, is cached. `None` counts as `unrouted`; the event is dropped.
* **Reading never waits.** `IngressSender::push` is the only downstream call. A consumer that stops
  draining for seconds must not slow the socket read (C-D13: broker-side backlog stays under 1 MiB,
  no disconnect, final state correct).
* **No method calls** other than `Hello` and `AddMatch`, each under `connect_deadline`. A broker that
  never answers `AddMatch` makes `spawn_drain` return `Err` within the deadline (C-D12).
* **Lifecycle.** `spawn_drain` returns after the match rules are installed. `stop()` returns within
  1 s, idle or busy (C-D10). EOF or a socket error ends the drain, sets `DrainEnd::Disconnected`
  and never panics (C-D11). A panic in a drain thread is caught and reported, never propagated.
* **Match rules** `default_match_rules()` are installed with `AddMatch` (C-D9).
* **Counters** (`DrainStats`): `messages` = framed messages, `pushed` = `IngressSender::push` calls
  (must equal the number of mapped events; checked against the truth), `ignored`, `unrouted`,
  `malformed`, `bytes`, `batches`, `backlog_max` (largest `SIOCINQ` seen, 0 if not sampled),
  `shed_starts`.
* **Threads** are named `gorca-drain-N` for the socket readers; any helper thread starts with
  `gorca-`. (The harness counts every thread not named `hx-*`, so naming is for humans.)

## 3.6 Hard resource bounds (hostile input)

A malicious or buggy app on the a11y bus can send unlimited distinct keys (object paths, bus
names), huge strings and floods. Every one of these bounds is a **hard requirement** for every
candidate; overflow always degrades to the ADR resync path (dirty marks, `Flooded`) and never to
unbounded growth, and the lossless classes (the newest `Focus`) still get through. Tests:
`crates/gorca-bench-b2/tests/bounds.rs` plus T5 and C-I5.

| Bound | Value | What happens beyond it | Test |
|---|---|---|---|
| Pending slots per app (`max_keys`) | 4 096 | app enters `Flooded`: non-kept classes become dirty marks | C-I1, C-I5 |
| Pending slots per app including lossless classes (`hard_cap`, rule H) | 65 536 | new keys other than `Focus` set dirty-all; an announcement is counted in `announcements_dropped` | C-I1, C-I5, T5 `pending_max` |
| Pending slots across **all** apps (`SlotBudget`, `GLOBAL_SLOT_BUDGET`) | `GLOBAL_SLOT_BUDGET` (262 144) `+ apps x (R + 1)`, R = per-app reserve (`IngressConfig` default 32) | beyond the pool an app may only use its own reserve (AU-3); past that, rule H applies. Every app ingress is created through one shared budget in production, and in the harness router and T7 (`rig.rs`, `scenarios.rs`). See "B-3 bound" below | B-3 |
| Dirty set nodes | 65 536 | the set becomes "everything" | C-I1 |
| Text deltas per node | 64 | collapse to `Replaced` | C-I1 |
| Announcements per drain | 16 | dropped and counted | C-I1 |
| Announcement text | 4 096 bytes | clipped at a char boundary by the mapping (`MAX_ANNOUNCEMENT_BYTES`), so no event copies megabytes | map.rs unit test, B-4 |
| Superseded-slot tombstones | at most `2 * live + 256` | the slot vector is compacted; repeated `Removed(n)` for one node must not grow memory | B-2 |
| Cached senders per drain connection | 4 096 (`MAX_ROUTES`) | the cache is dropped and rebuilt, so churn of bus names is bounded | B-4 |
| Bytes of one incoming message held in memory | 16 MiB | a larger message is skipped while streaming and counted as `malformed` (the D-Bus limit is 128 MiB). The zbus reference cannot do this; test B-5 is `#[ignore]` and run by `run-all.sh` with `--include-ignored` | B-5 |
| Counters vs structures | exact | every cap compares against a counter that must equal the real size of the structure after every push, merge, supersede, flood, drop and drain (`IngressReceiver::check_invariants`, `debug_assert` in debug builds) | B-1 |

**B-3 bound (amended 2026-10-12, judge finding; coordinator decision R = 32; derivation added
after the fixes review, F-3/F-6).** AU-3 gives every app a reserved minimum R of slots that
other apps cannot take; the reserve is held **outside** the shared pool. With a hard global bound
equal to the pool the two requirements contradict: apps registered after the pool is full still
need their reserve, so `SlotBudget::used` rightly exceeds the pool (measured on C: 11 484 used
against a 10 300 bound with 300 apps). The bound is

    used() <= global_budget + apps x (R + 1)        at every instant, for any number of producers

**Derivation.** `used()` is the sum of two counts kept in one atomic word (so a reader never
sees a torn sum): *pool units* P and *reserve units* V.
1. P: every pool unit is taken by a compare-and-swap that refuses when P is already at
   `global_budget`; hence P <= global_budget, except for the newest Focus slot of an app, which is
   always admitted (`force_charge`) and exists at most once per app: P <= global_budget + apps.
2. V: each app has one counter `reserve_used`, shared by its table and its ring credit, that is
   raised by a compare-and-swap that refuses at R; V is the sum of these counters: V <= apps x R.
   (A design with two independent, stale views of the reserve, one for the table and one for the
   ring, overshot to 48..80 units for one app with 2..16 producers; the single CAS counter is what
   makes the bound exact.)
3. Adding: used = P + V <= global_budget + apps x (R + 1). Leased ring credit (a producer takes
   units from the budget in blocks of 16 for its ring) is part of P and V and is bounded by the
   same two rules; it is returned when the ring drains, so `used` equals the sum of the pending
   units of all ingresses whenever the ingresses are observed (`drain`, `pending`,
   `check_invariants`), and exceeds it by at most one lease block per ingress in between.
The reserve is therefore not taken from the pool: a pool of N units still holds N units for apps
above their reserve (an earlier version counted both in one total, which shrank the pool by R
per app and made a failed preemption scan reachable).
R is read from the config (`IngressConfig::reserved_slots`, default 32), not hard-coded in
tests: `limits::APP_RESERVE` is `gorca_ingress::RESERVED_SLOTS`. `a_global_budget_bounds_many_apps`
and `audit_concurrent_pushes_never_overshoot_the_budget` assert both `budget.used()` and the sum
of `pending()` against this bound with 2 producers per app; nothing else was relaxed (focus
survives, the resync mark is required, `used` returns to 0). See
`docs/adr/0004-ingress-drain-and-coalescer.md`.

**B-5 harness note (2026-10-12).** The test must measure the drain, not the harness: the 100 MiB
payload is dropped right after it is copied into the mock link, the mock link releases queue
capacity above 4 MiB once drained, and RSS is sampled only after `Link::flush_all`.

**Feeder write quantum (2026-10-12).** Paced feeders no longer write one message per `write`.
A real dbus-broker under load forwards what it read in one go, so the reader's `recv` returns
many messages; the old feeder (`behind = due - sent` re-evaluated in a spin loop) emitted exactly
one due event per write at 100k/s, i.e. one `recv` and one wake-up per message (~75k
voluntary context switches per second, T4.drain_core_pct 75-77 % on main). `AppPlan::write_quantum`
(default 200 us) makes a paced feeder wait until a quantum's worth of events is due (capped at
`burst`; slow apps still write per event; a due probe is never delayed). The gate `<= 60 %` is
unchanged. Set the quantum to zero to reproduce the old behaviour.

**AU-6 timing tests** (`audit_latency.rs`) are `#[ignore]`d under `debug_assertions`; the
250 us bound is a release-build requirement and is run with `cargo test --release`.

Findings that led to these rules, all in the reference as delivered: (1) repeated `Removed(n)`
left a tombstone per push in `slots` (`live` stayed 1 while the vector grew without bound); (2) the
lossless classes had no cap; (3) `live` was decremented with `saturating_sub`, which would hide a
counter that drifted below the real size; (4) announcement text and the sender cache were
unbounded; (5) there was no bound across apps. (1)-(5) are fixed in the reference; the message-size
bound (B-5) is open for candidates.

### 3.7 Audit requirements (security audit of 74dbd07, binding on every candidate)

These requirements come from the security audit. Each one has a test in `crates/gorca-ingress/tests/audit_*.rs`. The tests are `#[ignore]` on the reference; every candidate must un-ignore them and pass all of them.

| # | Severity | Requirement | Tests |
|---|---|---|---|
| AU-1 | High | Dropping an ingress (or its receiver) returns every charged slot. Σ `live` over all ingresses == `SlotBudget::used` at all times, asserted by tests (`check()` must verify equality, not `used ≥ live`). | `audit_budget_released_when_ingress_dropped`, `audit_sender_churn_exhausts_global_budget_permanently` |
| AU-2 | High | A push into an ingress whose receiver is gone charges nothing. A sender that has disconnected (NameOwnerChanged) loses its cached route in the drain. | `audit_dead_receiver_does_not_hold_budget` |
| AU-3 | High | Each app has a reserved minimum of R slots (R = 32, outside the shared pool). An app holding <= R slots is never affected by other apps. An app above R can only be preempted, which floods it and marks it dirty-all, so nothing is lost silently. | `audit_hostile_apps_cannot_starve_a_victim` |
| AU-4 | Medium | The budget and `hard_cap` count queued events, including text deltas (a text slot holding k deltas is charged k). | `audit_text_deltas_are_charged_and_bounded` |
| AU-5 | Medium | Memory held by an idle, drained ingress is ≤ 1 MiB. Capacity above that is freed on drain. | `audit_drained_storm_releases_memory` |
| AU-6 | Medium | No single push or drain critical section exceeds 250 µs worst case at `hard_cap`. Amortised O(1) is not sufficient: compaction and drain must be incremental or lock-free. | `audit_single_push_is_bounded_under_tombstone_compaction`, `audit_drain_critical_section_is_bounded` |
| AU-7 | Low | Concurrent pushes must never overshoot the global budget, including Focus charges. | (candidate adds a test) |
| AU-8 | Medium | Counters equal the real structure sizes after every push, merge, supersede, flood, drop and drain (from section 3.6 row B-1). | `counters_equal_structure_sizes_after_every_operation` (B-1), `check_invariants` |

## 4. The ingress interface (`gorca_ingress`)

```rust
pub fn ingress(cfg: IngressConfig) -> (IngressSender, IngressReceiver);
impl IngressSender   { pub fn push(&self, ev: RawEvent); }          // Clone + Send + Sync, many producers
impl IngressReceiver { pub fn drain(&self) -> Drained;              // never blocks
                       pub async fn notified(&self);                // like tokio Notify::notified
                       pub fn pending(&self) -> usize; }
pub struct IngressConfig { max_keys: 4096, announcement_cap: 16, max_text_deltas: 64, hard_cap: 65_536 }
pub struct Drained { events: Vec<RawEvent>, dirty: Option<DirtySet>, stats: IngressStats }
```

Extension relative to the reference at the start of B-2: `IngressConfig::hard_cap` and rule H below
(found by the harness, T5 `stall_lossless`: the lossless classes were unbounded).

### 4.1 Keys and the closed event set

`CoalesceKey`: `Focus` (one slot per app), `Props(n)`, `Children(n)`, `Text(n)`, `Caret(n)`,
`Selection(n)`, `ActiveDescendant(n)` (container), `Announcement(seq)`, `Window(n)`, `Document(n)`,
`Removed(n)`.

### 4.2 Coalescing rules (the executable model is normative)

Let the pending table hold at most one slot per key, in order of first occurrence.

* **R-LWW** `Focus`, `Children`, `Caret`, `Selection`, `ActiveDescendant`, `Window`, `Document`,
  `Removed`: a later event for the same key replaces the payload in place; the slot keeps the
  position of the **first** occurrence (R-2).
* **R-OR** `Props(n)`: the `PropMask`s are OR-ed into the one slot.
* **R-TEXT** `Text(n)`: deltas append in order, except: a `Delete` followed by an `Insert`, more than
  `max_text_deltas` deltas, or a `Replaced` already present collapse the slot to one
  `Replaced{text: None}` (R-5/T-1; all toolkits). Adjacent-insert concatenation and insert/delete
  cancellation (ADR 6.3) are **not** required in B-2 and the harness never produces such pairs;
  an implementation that adds them must keep every other behaviour identical.
* **R-1 supersession** `Removed(n)` deletes every pending slot of `n` (including an older
  `Removed(n)`), then inserts its own slot. Events for `n` pushed later create new slots after it.
* **Announcements** are never merged; each takes a fresh slot. At most `announcement_cap` per
  drain; the rest are dropped and counted in `announcements_dropped`.
* **R-3 overload (`Flooded`).** When a push of a **new** key finds `live == max_keys`, the app enters
  `Flooded`: every pending slot whose class is not kept becomes a dirty mark of its node, in slot
  order. While `Flooded`, an event of a non-kept class becomes a dirty mark (or dirty-all when it
  has no node) and is not queued. **Kept classes** (queued normally): `Focus`, `Removed`, `Window`,
  announcements, and `Caret(n)` when `n` is the node of the most recent `FocusChanged` (the focused
  node survives drains). `Flooded` ends with the drain; `Drained::dirty` is `Some` exactly when the
  batch was flooded.
* **Dirty set.** `DirtySet` holds at most 65 536 nodes; the next distinct node turns it into
  "everything" (`is_all()`), which clears the node list.
* **Rule H (hard cap, PC-7).** A push of a new key other than `Focus` with `live >= hard_cap` enters
  `Flooded` if not yet, then sets the dirty set to "everything" (an announcement is instead counted
  in `announcements_dropped`) and queues nothing. `Focus` is a single slot and always queues.
  Consequence: pending slots never exceed `hard_cap + 1`.
* **Stats (mandatory):** `pushed` (every `push` call since the last drain, including dropped
  announcements), `focus_sequence` (focus events folded into the slot), `announcements_dropped`,
  `flooded`. `coalesced` and `dirtied` are advisory.

### 4.3 Concurrency and wake-up

* Any number of producers (clones of `IngressSender`) and one consumer. Each producer's pushes are
  applied in its program order. A `push` that returns before `drain()` is called is visible in that
  drain or an earlier one; each pushed event lands in exactly one batch.
* A push into an ingress that is empty after the last drain wakes `notified()` with the semantics
  of `tokio::sync::Notify::notify_one` (a permit is stored if nobody waits). No wake-up may be lost:
  work pending for 500 ms with no wake-up fails C-I3.
* `push` never blocks on the consumer and never awaits; `drain()` never blocks.

### 4.4 What is exact and what is approximate

For a **single producer** the output of `drain()` (events, their order, flood state, dirty set,
mandatory stats) must equal the model's batch for the same input and drain points (C-I1, 120 random
scripts, four configurations including tiny `max_keys`, `announcement_cap`, `max_text_deltas` and
`hard_cap`). With several producers only the state-level guarantees of section 5 apply.

## 5. Correctness

The harness never trusts the implementation's own bookkeeping; it replays the input.

### 5.1 Ground truth and the consumer fold

`model::Fold` is the state a consumer would hold: final focus, per node (OR of property masks, last
caret offset, selection seen, last active descendant, children dirty, text changed, last document and
window event), the set of removed nodes, announcement count, dirty set. Applying the raw event
stream gives the **truth**; applying the implementation's batches (dirty marks first, then events in
order) gives its **view**. A `Removed(n)` clears node `n` in both.

### 5.2 `compare(truth, view)` must report nothing

1. **Focus.** The final focus is identical. (Lossless: the newest focus is never dropped, flooded or not.)
2. **Removal.** The set of removed nodes is identical. (Lossless: every `Removed` is delivered.) The one exception is rule H: once the view is dirty-all, it may lack removals (the resync rediscovers them) but must not invent any.
3. **Windows.** The last window event of every node is identical unless the node is dirty.
4. **Accounting.** `truth.pushed == sum(stats.pushed)`; announcements: `pushed == delivered + dropped`.
   Nothing may be lost silently between the socket and the ingress.
5. **State.** For every node that is **not** in the view's dirty set (nor `dirty_all`): every field
   equals the truth. For a dirty node the view may have anything: the app actor will resync it.
   A node with state in the view but none in the truth is a failure (phantom).
6. **No vacuous pass.** Non-flood runs must report `flooded == false` in every batch (a candidate
   cannot answer by flooding everything), and the comparison must have compared nodes.

### 5.3 Ordering

* **ORD-1 per node, across batches.** The payload counters carried in caret offsets and text offsets
  grow with the sequence number of the event; per node they must not decrease across the batches
  the consumer sees (checked in every T6 run and in C-I4).
* **ORD-2 within a batch** the order is first occurrence of the key (exact, C-I1).
* **Per app.** All batches of an app preserve the order of its pushes (one producer). **Across apps**
  nothing is promised (PC-2).
* **Misrouting** (an event delivered to the ingress of another app) is a failure.

### 5.4 Resync on overflow

Overflow is signalled only through `Drained::dirty`. An implementation that drops anything else
without a dirty mark (or an `announcements_dropped` count) fails 5.2. `dirty_all` is allowed but it
must be earned: it is reported as `flooded` and judged by T6's "no vacuous pass" rule.

## 6. Metrics

All times are wall clock from `CLOCK_MONOTONIC` (`clock::now_ns`) unless noted. A metric passes when
the **median over `repeats` (3) runs** meets the threshold; min and max are reported with it. The
threshold constants live in `crates/gorca-bench-b2/src/limits.rs`; this table is generated from them.

| Id | What | Exact method | Pass |
|---|---|---|---|
| **T1.sustained_eps** | throughput before the broker disconnects us | Mock broker, quota 64 MiB. One app, fakeapp mix, 5 ms focus probes. Ladder 25k, 50k, 100k, 200k, 400k, 800k, 1.6M events/s, 6 s each, stop at the first failing step. A step passes when: no disconnect; the feeder achieved >= 98 % of the target; broker-side backlog (userspace queue + `TIOCOUTQ`, sampled every 20 ms after the first second) p99 <= 1 MiB (M-11); probe lag p99 <= 20 ms; all fences seen. Value = highest passing step. A shortfall with an empty backlog is `generator-limited`, not a drain failure. | `>= T1_SUSTAINED_EPS_MIN` |
| T1.saturation_eps | consumption rate when always behind | Closed-loop feeder keeps 8 MiB ahead of the drain for 5 s; value = framed messages / s. Reported, ranks candidates. | info |
| **T2.p50/p99/p999_us@{idle,10k,100k}** | drain latency, socket to coalescer output | Mock broker. A load app at 0 / 10k / 100k events/s (fakeapp mix) and a separate probe app sending a focus probe every 500 us (8000 / 40000 samples per run), 4 / 20 s. Latency of a probe = `now_ns()` after the `drain()` that first returned a focus event with id >= k minus `now_ns()` immediately before the `write` that carried probe k. The consumer is the production path: `notified().await` then `drain()`. | p50 <= 300 us; p99 <= 500 us idle, 1 ms at 10k, 3 ms at 100k; p99.9 <= 3 ms at 10k, 10 ms at 100k |
| **T3.focus_p99/p999_us** | focus latency under the 40k/s storm | 4 apps x 10k events/s fakeapp mix (the storm of `fakeapp.py`), probes (every 2 ms, 10 000 samples) in the focus app **sharing its ingress with 10k events/s**, 20 s. Same stamps as T2. Mock: thresholded. dbus-daemon, dbus-broker: reported and must survive (they add forwarding latency). | p99 <= 2 ms, p99.9 <= 8 ms (mock) |
| T3.survived | no disconnect, all fences | any bus | 1 |
| T4.cpu_ms_per_10k@10k | CPU per 10k events at a low rate (info) | 5 apps x 2k events/s, 15 s. CPU = sum of on-CPU ns (`/proc/self/task/*/schedstat`) of all threads not named `hx-*`, over the feeder window, / (framed messages / 10 000). | info (see T4 note) |
| **T4.cpu_ms_per_10k@100k** | CPU per 10k events where reads batch | 1 app at 100k events/s, 8 s, same CPU definition | `<= 25 ms` |
| T4.drain_core_pct@100k | drain thread occupancy | 1 app at 100k/s, 8 s; CPU of `gorca-drain*` threads / wall (M-11) | `<= 60 %` |
| T4.idle_cpu_ms_per_s | idle spin | connected, no traffic, 5 s | `<= 10 ms/s` |
| **T5.rss_delta_mib@{distinct,stall,stall_lossless}** | memory ceiling | RSS peak (`VmHWM` after reset) minus RSS before the drain was spawned. `distinct`: 50k/s of never-repeated keys with the app actor hung from 1 s to 5 s of 8 s; `stall`: 100k/s realistic mix, app actor hung 1 s to 6 s; `stall_lossless`: 100k/s of Removed/Window/Focus/Announcement heavy mix over 1M nodes, same hang. | `<= 256 MiB` (M-8, A5) |
| **T5.pending_max@...** | bound | largest `drain()` batch (events) in the same runs | `<= hard_cap + 4096` |
| T5.survived@... | no disconnect while the actor is hung, fences seen, and `C.violations` = 0 after it resumes | | 1 / 0 |
| **C.failures@{mix}** | correctness at benchmark scale (T6) | 3 concurrent apps per run, 5 s, 50k events/s, mixes fakeapp, terminal, realistic, lossless, plus realistic over 20 000 nodes (overflows `max_keys`, consumer delayed 150 ms per drain so `Flooded` really happens) and distinct. Also fakeapp and realistic on dbus-daemon and dbus-broker. Counts: `compare()` violations, ORD-1 violations, timer wake-ups with work pending (lost wake-ups), misrouted events, apps whose fence never arrived, disconnects. | 0 |
| **T7.push_p99/p999_ns** | cost of one `push` | in process, 2M pushes of mixed events over 1500 nodes, drain every 512, per-call clock pair with the clock overhead subtracted | p99 <= 1 us, p99.9 <= 5 us |
| **T7.push4_p99/p999_ns** | contended push | 4 producers on one ingress, a consumer draining every 200 us | p99 <= 5 us, p99.9 <= 50 us |
| FA.{storm,mixed,hung}.failures | Python fake apps in the isolated AT-SPI harness | `tests/harness/run-isolated.sh b2 fakeapp <s>`: storm 40k/s; mixed = storm 20k/s + form (focus every 50 ms) + terminal (VTE-style); hung = mixed + a hung app + `SIGSTOP` of the storm app for 2 s. A truth tap on the same bus (own decoder) provides the per-sender truth. Zero tolerance for lost events, misrouted events or a disconnect. | 0 |
| FA.*.focus_p99_us | focus latency vs the tap's arrival of the same message | as above | `<= 2 ms` |
| F3.* | fixture self-test | a reader that does not read is disconnected by dbus-broker and loses signals on dbus-daemon; a reader that keeps up is never disconnected | fixture check, not a candidate metric |

**T4 note (amended 2026-10-12, reviewer, coordinator decision).** CPU per 10k events is gated at
100k events/s, where the drain's reads batch. At 10k events/s the feeder's 200 us write quantum
carries less than one message per write, so every message is its own thread wakeup and the cost
is the wakeup, not the code: on this host (Intel Core Ultra 9 285H, `powersave`, PREEMPT_RT) the
same drain costs 110 to 561 ms per 10k messages depending on the core it lands on (E-cores 110,
LP E-cores 167, P-cores 561, unpinned 417; `b2 threads`), a 5x spread from placement alone, and
no blocking-`recv` drain has been below 160 there (reference 225). At 100k/s every placement
passes (8 to 18 ms). Reaching 25 ms at 10k/s would need busy polling, which contradicts
`T4.idle_cpu_ms_per_s`. The 10k figure stays in the report as information (with the governor and
the core placement in the result meta); absolute load is bounded by `T4.drain_core_pct@100k`.

**Gate.** A candidate is rankable only if it passes **every conformance test** (`cargo test -p
gorca-bench-b2`, debug and release), every `C.failures`, `T3.survived`, `T5.survived`,
`T5.pending_max`, `T5.rss_delta`, `FA.*.failures`, `T1.sustained_eps`, `T4.drain_core_pct` and `T4.cpu_ms_per_10k@100k`.
**Ranking** among passing candidates: T1.sustained and saturation (higher), T3/T2 p99 and p99.9,
T4 CPU per 10k at 100k/s, T7 push4 p99, then code size, then reviewer judgment on correctness risks
(section 8).

## 7. Constraints

* **No blocking.** No mutex or channel send that waits for the consumer on the drain path. A lock is
  allowed only if its critical section is bounded and independent of consumer speed (the reference
  `Mutex` qualifies on correctness and is what T7 measures).
* **Bounded memory.** Every queue has a bound and an overflow action defined in section 4 or in the
  candidate's brief; no unbounded `Vec` or channel between the socket and the consumer.
* **Multi-threading is allowed and encouraged** (speed over frugality, PC-6). Any number of threads
  inside the drain, named `gorca-*`, spawned with `std::thread::Builder` and a name; no unnamed
  threads. A candidate may use the tokio runtime internally but may not depend on the application's
  runtime.
* **Unsafe.** Only in `gorca-dbus` and `gorca-rt` (ADR 11 rule 6), one module each, `// SAFETY:` on
  every block. `gorca-ingress` stays `unsafe`-free: a lock-free ring is built from audited crates,
  or lives in `gorca-rt` as its own module with its own tests.
* **Dependencies.** Allowed without asking: crates already in `Cargo.lock` plus `crossbeam-queue`,
  `crossbeam-utils`, `crossbeam-epoch`, `parking_lot`, `dashmap`, `hashbrown`, `ahash`, `rustc-hash`,
  `memchr`, `bytes`, `slab`, `arc-swap`, `mio`, `rustix`, `io-uring`. Anything else needs the
  orchestrator. `cargo xtask deps` must pass (no transport crate under `gorca-ingress`).
* **Quality gates.** `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo
  test --workspace`, `./tools/check-deps.sh` (that is `make check`) stay green. Lints are not relaxed.
* **Scope of edits.** `crates/gorca-dbus/src/drain/**` and `crates/gorca-ingress/src/**` (and a new
  module in `gorca-rt` if needed). `crates/gorca-bench-b2/**` and `docs/bakeoff/b2-ingress/**` are
  owned by the harness: a candidate that needs a harness change asks the orchestrator.
* **Public API** of sections 3 and 4 unchanged. Defaults of `IngressConfig` unchanged.
* **No gaming.** The drain must not detect the harness (thread names, addresses, event content such
  as the `seq` property) and must not special-case the mock broker.

## 8. Budgets

| Budget | Limit |
|---|---|
| Wall time per candidate | 4 hours from branch to `RESULTS.md`; one implementation pass plus at most two fix passes after the first harness run |
| Code size | at most 1 600 added non-test lines in total across `gorca-dbus/src/drain`, `gorca-ingress/src` and `gorca-rt/src` (`git diff --numstat` of those paths, `#[cfg(test)]` blocks excluded). A candidate over budget is judged on the first 1 600 only if it says which |
| New dependencies | at most 2 |
| Benchmark runs | `run-all.sh <label>` on a quiet machine (load average < 4); the full run takes about 25 minutes |
| Deliverables | the branch, `RESULTS.md` with the raw `run-all.sh` output and the result files copied into `bench/results/b2-ingress/<label>/`, and a 1-page design note (what runs where, what is bounded by what) |

## 9. Baseline: the reference arm

Zbus `MessageStream` on one thread plus the `Mutex` coalescer of `gorca-ingress` (ADR arm a,
improved only by the hard cap). Numbers, host and load: `bench/results/b2-ingress/reference/`.

Run 1917d80, release, 3 repeats, host load average 16 at start (a noisy desktop; the reviewer re-runs). Median (min..max):

| Metric | Reference | Threshold | Result |
|---|---|---|---|
| T1.sustained_eps (mock) | 200,000 (100,000..200,000) events/s | >= 100,000 | PASS |
| T1.saturation_eps (mock) | 306,473 (278,558..308,446) messages/s | info | info |
| T2.p50_us@idle (mock) | 61 (61..61) us | <= 300 | PASS |
| T2.p99_us@10k (mock) | 246 (239..752) us | <= 1,000 | PASS |
| T2.p99_us@100k (mock) | 3,757 (3,315..4,802) us | <= 3,000 | FAIL |
| T2.p999_us@100k (mock) | 6,765 (6,228..10,180) us | <= 10,000 | PASS |
| T3.focus_p99_us (mock) | 1,958 (1,935..2,034) us | <= 2,000 | PASS |
| T3.focus_p999_us (mock) | 2,161 (2,122..2,816) us | <= 8,000 | PASS |
| T3.focus_p99_us (dbus-broker) | 2,064 (2,048..2,081) us | info | info |
| T4.cpu_ms_per_10k@10k (mock) | 225 (223..227) ms per 10k events | <= 25 | FAIL |
| T4.drain_core_pct@100k (mock) | 94 (93..94) % of one core | <= 60 | FAIL |
| T7.push_p99_ns (-) | 140 (114..360) ns | <= 1,000 | PASS |
| T7.push4_p99_ns (-) | 21,392 (17,263..26,204) ns (4 producers) | <= 5,000 | FAIL |
| T7.push4_p999_ns (-) | 59,857 (58,222..63,813) ns (4 producers) | <= 50,000 | FAIL |
| T5.pending_max@stall_lossless (mock) | 65,536 (65,536..65,536) events in one batch | <= 69,632 | PASS |
| T5.rss_delta_mib@stall_lossless (mock) | 21 (18..26) MiB | <= 256 | PASS |
| FA.hung.failures (isolated) | 0.0 | <= 0 | PASS |
| FA.hung.focus_p99_us (isolated) | 619.0 | <= 2000 | PASS |
| FA.mixed.failures (isolated) | 0.0 | <= 0 | PASS |
| FA.mixed.focus_p99_us (isolated) | 398.1 | <= 2000 | PASS |
| FA.storm.failures (isolated) | 0.0 | <= 0 | PASS |

Reading: the reference drain is the bottleneck, not the coalescer. The zbus path costs about 15 us
per message (one core saturates near 65k events/s of drain work, 300k messages/s when the broker is
the only load), so it sustains the 100k ADR floor but disconnects at 400k, spends 225 ms of CPU per
10k events and 94 % of a core at 100k/s, and shows a 3.8 ms p99 at 100k/s. The `Mutex` coalescer
costs 64-140 ns per uncontended push but 21 us p99 with four producers. Everything correctness and
bound related passes (6 failing metrics of 109, all performance).

## 10. Candidate approach briefs

The three candidates differ in where the work happens and what is shared. Each brief states the
idea, the structures, what it risks and which metric it should win. A candidate that finds a better
variant of its own idea keeps the brief's name and says what changed.

### Candidate A: raw drain, lock-free ring per app, lazy dedupe on the consumer side (ADR arm b)

* **Drain.** One `gorca-drain-0` thread, blocking `recv` into a 1 MiB buffer (>= 64 KiB per call, D-1),
  own D-Bus framing (length from the 16-byte primary header, partial message carried over), header
  peek of sender / path / interface / member / signature, FNV-1a of the path, body fields only for
  the members that need them (section 3.2). `SIOCINQ` sampled per batch. A per-connection sender
  cache (`HashMap<sender, Option<AppRoute>>`).
* **Ingress.** The producer side does almost nothing: it appends a compact record
  `{key_kind, node, payload}` to a **bounded lock-free MPSC ring** per app and bumps a per-key
  generation in a fixed-size direct-mapped table. `drain()` pops the ring and does the coalescing:
  records whose generation is stale are skipped (generation-tagged lazy dedupe), the rest are folded
  with the section 4.2 rules in a reusable open-addressing table. Overflow: ring full or `max_keys`
  exceeded sets a `Flooded` flag and per-node dirty bits (atomic bitmap, falling back to dirty-all).
* **Expected strengths.** Smallest producer cost (T7), no lock on the hot path, the drain thread
  does ~0.3 us per message, so T1/T2/T4 are bounded by the memory copy. Consumer-side batching
  amortises the coalescing.
* **Risks.** Exactness of C-I1 (first-occurrence order, rule H) with deferred coalescing; ring
  sizing versus `hard_cap`; lossless events must never be dropped by a full ring (they take the
  dirty path); the ABA/generation wrap; unsafe code goes to `gorca-rt`.
* **Should win:** T7, T4, T2 at 100k.

### Candidate B: raw drain, sharded connections for heavy senders, sharded coalescer (ADR arm c)

* **Drain.** Candidate A's reader and framing, plus up to `max_event_connections` (4) connections.
  Connection 0 has the catch-all rules. A sender whose rate exceeds a threshold (say 50k/s over
  100 ms) is **migrated** to its own connection by `AddMatch sender=:1.N` (with a deadline) on a new
  connection and a per-sender **serial watermark**: messages with serial <= watermark that arrive on
  the old connection are duplicates and are dropped, since D-Bus cannot exclude a sender from a
  catch-all rule. Migration back when the rate falls. `RemoveMatch` is never used on the catch-all.
* **Ingress.** Several drain threads push into the same app only transiently (migration); the
  coalescer is therefore **MPSC-safe and sharded**: `N` shards by key hash, each a cache-line aligned
  `Mutex<Table>` or a lock-free slot array, with the global order (first occurrence) kept by a
  per-app atomic sequence number assigned at push and merged at `drain()`. `live`/`max_keys` is an
  atomic counter.
* **Expected strengths.** Parallel framing and parsing: a 400k to 1M events/s heavy sender no longer
  saturates one core, so T1 and T3 improve when one app dominates.
* **Risks.** The most complex: migration races, duplicate suppression, ordering across connections
  (a migrating sender may reorder: the watermark must make hand-over exact, C-I4/ORD-1), extra broker
  work (two subscriptions per migrated sender). It must not regress T2 at idle and must still pass
  the C-I1 exactness test for a single producer. If the harness cannot create a heavy sender worth
  migrating (the mock broker has one stream), T1 shows no gain and the candidate loses on size; that
  is a legitimate outcome for the bake-off.
* **Should win:** T1 saturation with a single heavy app; real-broker T3.

### Candidate C: single drain thread, batch coalescing, one publish per app per `recv`

* **Drain.** One thread, `recv` of 1 MiB or more (the optional variant is io-uring multishot receive
  with a provided-buffer ring; without the `io-uring` crate the same idea runs on `recv` or `mio`).
  Frame the **whole batch** first. Then coalesce the batch into **thread-local per-app tables**
  (plain structs, no atomics, no locks, the section 4.2 rules applied once per batch), and publish at
  most **one merged batch per app per `recv`** to the ingress through a single-slot atomic exchange
  (or a tiny SPSC of batches), waking the consumer once.
* **Ingress.** `drain()` takes the published batches and merges them (a batch merge is the same
  rules applied to batch slots), then applies `Flooded` / rule H on the merged result. `push` stays
  available for other producers and the tests as a one-event batch.
* **Expected strengths.** The fewest synchronisations: cost per event is hash + table update in
  thread-local memory; the consumer wakes at most once per `recv` per app, which should flatten
  p99.9 latency and CPU per 10k events. Memory is bounded by the batch size by construction.
* **Risks.** Latency is quantised by the batch boundary, so T2 idle may be a little worse than A
  (a probe waits for its `recv` and for the rest of the batch to be framed: measure it); C-I1
  exactness across the merge of consecutive batches; `Flooded` is decided per merged result, which
  must reproduce the model's trigger point exactly for a single producer; fairness between apps in
  one batch.
* **Should win:** T4, T3 p99.9, the stalled-consumer memory metrics.

### Not allowed

* A tokio task per app connection reading through zbus (the reference's weakness: zbus decodes every
  message into an owned `Message`, 15 us each, which bounds the reference near 65k events/s) and
  `DashMap` as the only change are the ADR's rejected default; they may appear as parts of a
  candidate but not as the whole.
* Dropping events without a dirty mark or a count; detecting the harness; changing the mapping table;
  turning a metric green by shrinking what it measures.

## 11. How the candidates are judged

1. Each candidate agent works in its own git worktree (`isolation: "worktree"`) from the commit that
   contains this spec and the harness, and delivers `RESULTS.md` (raw output of `run-all.sh <label>`).
2. The reviewer **re-runs the harness itself** on each branch (`run-all.sh <label>-review`), without
   trusting `RESULTS.md`, on a quiet machine, and compares with `harness/compare.py`.
3. Gate first (section 6), then ranking metrics, then correctness risks the harness cannot see
   (ordering across producers, wake-up edge cases, panic containment, `unsafe` review), then size and
   extensibility (R-6 lazy text, NameOwnerChanged, P-11 per-app deregistration).
4. The reviewer names a winner and the ideas to port from the losers. The orchestrator presents the
   scored table to the user; after approval an ADR is written, the winner merged, the worktrees
   deleted.

## 12. Open items and known limits

* `hard_cap` and the dirty-everything fallback are new in this spec (ADR 6.3 lists only
  `max_keys`); the ADR should absorb them with the B-2 decision.
* Adjacent-insert concatenation and insert/delete cancellation of ADR 6.3 are deferred to the
  decoded-text step (R-6); the model does not require them.
* The mock broker does not model broker CPU or NameOwnerChanged; the real-bus runs cover the first.
* Host noise: the benchmark machine also runs a desktop. Every result file records the load average
  at start and end; a quiet-machine re-run is the reviewer's job.
