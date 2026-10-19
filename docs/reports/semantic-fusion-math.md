# Semantic fusion: one merge point for AT-SPI, AccessKit, quirks, mirror, patches and users

Date: 2026-10-18. Author: math-researcher. Companion to draft ADR-0022
(`docs/adr/0022-semantic-model-and-presentation-pipeline.md`, report
`docs/reports/aria-semantics-architecture.md`). Builds on `docs/reports/addon-platform-math.md`
(hook algebra, K7 data overlay) and `docs/reports/aria-test-coverage-math.md` (covering arrays,
properties P0-P10).

Prototype: `tools/research/semantic_fusion.py` (pure Python, no dependencies, self-testing).

```
. ./env.sh
python tools/research/semantic_fusion.py selftest   # test vectors + small runs of every property
python tools/research/semantic_fusion.py all        # all experiments (~12 s)
```

Notation: **[P]** proven (argument given here), **[M]** measured on the prototype (synthetic
workload), **[H]** heuristic / engineering judgement, **[U]** unverified assumption.

---

## 0. Result in one page

1. **Do not merge events; merge state.** Every input is a *fact* owned by exactly one *register*;
   each register is a max over a total order (a join-semilattice). The effective `Semantics` is a
   **pure function** `FUSE(registers, static tables)`. Order independence, idempotence and
   determinism then hold by construction, not by care in every handler **[P]**.
2. **Within a source, order by the source's own sequence number** (actor timeline: ingress order
   for events, issue order for fetches; already the mirror's `issued_at`, LR-1..LR-3).
   **Across sources, never by time.** There is no causal order between an AT-SPI signal and an
   AccessKit tree update, so any cross-source "newest wins" is permutation-dependent. A naive
   arrival-order LWW diverged in **660 of 660** shuffled deliveries; the register design in **0 of
   720** **[M]**.
3. **Precedence is a static, per-property-group layer order** (user > patch > observed > derived >
   default), and inside the observed layer a **trust table** `PI[app profile][group]` chooses
   between AT-SPI and AccessKit. Numeric "confidence" is rejected (uncalibrated, opaque to
   "why did gorca say X").
4. **Stratification removes cycles:** quirks act on one source's record (stratum -1), patch
   *selectors read only the pre-patch observed layer* (CSS-cascade rule), derived names read
   children's final values, repair `R` runs last. The dependency graph is acyclic, so one pass in
   stratum order computes the unique result **[P]**.
5. **Provenance is cheap on the hot path and exact on demand:** a 64-bit per-node tag (winning
   layer/source per group) plus stale/fallback bits; the full trail is produced by re-running
   `FUSE` in trace mode over the retained registers. Completeness check: 0 errors **[M]**
   (after the prototype found one real precedence bug, §2.5).
6. **Incremental maintenance with early cutoff** recomputes ≈ 1 base record and 1.9-2.5 node
   fusions per event (0.75 at batch 512) and publishes on 21-29 % of events; incremental equals
   from-scratch in every checked batch **[M]**. Estimated Rust cost ≤ 1 µs/event, i.e. ≤ 1 % of a
   core at 10k events/s **[H, U: not benchmarked]**.
7. **Two connectors on one app:** GTK selects exactly one backend per process
   (`refs/newton/gtk/gtk/gtkatcontext.c:643-714`, AccessKit first), so double exposure arises only
   when AT-SPI is a *translation* of the AccessKit tree (Newton `atspi-adapter`, or AccessKit's own
   Unix adapter). Then node identity is exact: the AT-SPI path is
   `/org/a11y/atspi/accessible/{adapter}/{u128 node id}`
   (`refs/accesskit/adapters/unix/src/atspi/object_id.rs:14-42`). Fuse only on exact keys;
   structural matching without keys reaches only 68-88 % recall **[M]** and should be used for
   diagnostics, not fusion. Default policy: per-app connector dedupe (Newton wins), node-level
   fusion only where keys exist.
8. **Verbosity / `ReportFlags` are not fused.** They gate fields in `compose` (ADR-0022 §2.6) and
   stay in the presentation cache key (`report_epoch`). Folding them into `Semantics` would bump
   `sem_rev` on every profile switch and invalidate every cache.

Concrete ADR-0022 amendments are in §8.

---

## 1. Formal model

### 1.1 Objects

- `X` node identities (core `NodeKey`); `G` property groups, fused atomically:
  `role`, `name(+name_from)`, `description`, `check`, `press`, `expand`, `select`, `current`,
  `invalid`, `popup`, `avail`, `flags`, `level`, `setpos{pos,size,src}`, `grid`, `live`,
  `text(rare strings)`, `existence`. Atomic groups prevent mixing e.g. `pos` from the author with
  `size` from the mirror.
- For each group `g` a value domain `V_g`, extended with two distinct non-values:
  - `Unknown` (never observed: lazy fetch not done, `PropMask` bit not fetched) - **not a value**,
    a lower layer may fill in, flagged `fallback`;
  - `Absent` (observed, property not present) - **a value**; it wins against lower layers.
  The distinction is load-bearing (§2.5).
- Sources: observed `O = {atspi, newton}` (after quirk + normaliser), `patch(p)` for each add-on
  patch, `user`, `derived` (document mirror), `default`.

### 1.2 Facts and registers

A fact is `(owner register, seq, payload)`. Register kinds (all keyed, all in the fused entry):

| Register | Key | Order (max) | Payload |
|---|---|---|---|
| observed | `(src, x, g)` | `(seq_src, vkey(v))` | `v ∈ V_g ∪ {Absent}` |
| lifecycle | `(src, x)` | `(seq_src, alive)` | alive / gone (tombstone) |
| invalidation | `(src, x)` | `seq_src` | marker; value kept, read as stale if newer than the value |
| patch set | `patch_id` | `(seq_patch, installed)` | installed / removed |
| user override | `(x, g)` | `(seq_user, vkey(v))` | `v` or `Clear` |

`seq_src` is the source's own monotone counter:

- AT-SPI: the app actor's write sequence (ingress order for signals; **issue** order for fetch
  replies). This is exactly the mirror's late-reply discipline (`crates/gorca-mirror/src/store/mod.rs`
  header, LR-1..LR-3), so no new clock is introduced.
- AccessKit/Newton: the tree-update index (updates are ordered per tree on the Wayland
  connection) **[U: verify in the Newton consumer protocol]**.
- Patches and user overrides: the settings/ext-host change counter.

**Lemma 1 [P].** Each register is a join-semilattice under `join = max` over a total order
(lexicographic `(seq, vkey(payload))`; `vkey` = a fixed total order on values, used only to make
an impossible seq collision deterministic and reported). Max over a total order is associative,
commutative and idempotent. Hence the register state after applying any multiset of facts in any
order, with any duplication and any batching, is the max of the set - a function of the *set* of
facts only. (State-based CRDT, G-Set compacted to its maximum.)

The invalidation and lifecycle rules of the mirror are expressible this way: "stale" =
`max_inval_seq > seq(value)`; "gone" = the max lifecycle fact is `gone`; a reply issued before a
tombstone cannot resurrect (its seq is smaller); a reply issued after it is proof of life (LR-3).

**Theorem 1 (order independence) [P].** If `FUSE` is a function of register states and static
tables only, then the effective semantics, the provenance trail and the conflict report are
independent of delivery order, duplication and batching of facts.
Proof: Lemma 1 + function composition. **Scope:** independence is *given the seq assignment*;
seqs are assigned where a source's order is defined (actor ingress for one D-Bus connection, which
preserves per-sender order; issue time for requests). Interleaving across sources, coalescer
reordering, batch boundaries, fetch-reply reordering, retries and resyncs are all covered.

**Corollary (why cross-source time is excluded) [P].** Let `a` (AT-SPI) and `b` (AccessKit) be
facts on the same `(x, g)` with different values and no causal relation. A merge that uses arrival
order returns `a` for delivery `ab` and `b` for `ba`, so it is not a function of the fact set.
Any deterministic merge must therefore rank the two by something order-free: a static priority.

### 1.3 The fusion function

`FUSE` is evaluated per node, in strata (the dependency graph between strata is acyclic):

| Stratum | Reads | Computes |
|---|---|---|
| S-1 quirk | one source's raw record of `x` | `quirk_src(rec)`: closed `QuirkAction`s (ADR-0008), with hit ids |
| S0 observed | quirked records of `x` from all observed sources | per group: first source in `PI[app][g]` whose value is not `Unknown` |
| S1 patch | S0 of `x` and of bounded neighbours (parent, children, descendants ≤ r) | per group: highest-ranked matching patch, rank `(specificity desc, ext_id, patch_id)` |
| S2 user | user registers of `x` | override value if not `Clear` |
| S3 derived | final values of `x`'s children (mirror text) | name-from-content etc., only if the winning layer is observed/default and empty |
| R repair | the merged record | canonical form: SM-1 (coarse role), SM-2 (exclusive states), SM-5 (`Avail::NotApplicable`), role-inapplicable groups masked |

Precedence for one group (highest first): **user > patch > observed > derived > default**.
`R` may mask a winner (e.g. a user sets `check=on` on a node whose final role is `link`); masking
is recorded, never silent.

**Lemma 2 [P].** `FUSE` terminates and has a unique result. S1 selectors read S0 only, never S1
output (no patch can enable or disable another patch); S3 reads children's *final* values, and
the tree is a DAG oriented child → parent; `R` reads the merged record of `x` only. Evaluate S-1..S2
for all dirty nodes, then S3+R bottom-up: each value is computed once from already-final inputs.

**Lemma 3 (idempotence of R) [P, checked].** `R` is a projection: every rule maps a record into
the set where that rule's precondition is false and no rule re-enables another's precondition
(rules depend on `role` only, and `R` never changes `role`). Hence `R∘R = R`. Checked on 500
random records in the self-test.

**Why not the alternatives** (operator choice per layer):

| Operator | Algebra | Verdict |
|---|---|---|
| LWW by arrival | not a function of the fact set | rejected (Corollary; 660/660 divergent runs) |
| LWW by source seq, inside one source | max-semilattice | **adopted** for every register |
| Static priority between sources/layers | left-zero monoid over a *fixed* order (K3/K7 of the add-on report) | **adopted**; order is data, not arrival |
| Max-confidence (numeric) | commutative if ties broken | rejected: scores uncalibrated across engines, explanations become "0.82 > 0.79", and a single global scale cannot express per-group trust |
| Multi-value register exposing concurrent values to presentation | MV-register CRDT | rejected for presentation (speech needs one value); kept as the *conflict report* |

### 1.4 Precedence per property group

The layer order above is universal; what varies is (a) which layers exist for a group and (b) the
observed-layer trust order `PI`. Recommended defaults **[H]**:

| Group | Layers (high → low) | Observed trust `PI` | Notes |
|---|---|---|---|
| role | user, patch, observed | per app: native tree first (Newton for Newton apps, AT-SPI otherwise) | derived never sets role |
| name | user, patch, observed (non-empty), derived, `""` | same | explicit empty from patch/user is final (§2.5) |
| description | user, patch, observed, derived (`aria-describedby` target text from the mirror) | same | |
| check/press/expand/select | patch, observed | same; **one source only per app** if possible | high-frequency; repair SM-2 after |
| current, invalid, popup, flags | patch, observed | same | |
| avail | patch, observed | same | SM-5 in `R` |
| level | patch, observed, derived (h1-h6 from mirror) | per app; e.g. AccessKit carries author level directly | |
| setpos | patch, observed(Author), observed(Engine), derived(Computed) | | identical to ADR-0022 `PosSrc` order: `PosSrc` *is* the layer |
| grid | observed, derived | | |
| live | patch (`live` action), observed, derived (inherited from live root) | | |
| text / value / caret | observed only | primary only | freshness dominates; never fused across sources |
| existence | patch (`hide`), observed lifecycle of the primary | primary | the secondary cannot keep a node alive |

`PI` lives next to the quirk tables (per app profile, data rows, switchable), so a misbehaving
engine is fixed by data, not code.

### 1.5 Conflicts and provenance

**Conflict** = disagreement *within a layer* that the order had to break:

- `observed-disagree(x, g, (s1, v1), (s2, v2))`: both observed sources know a value and they
  differ (after normalisation). Winner by `PI`. Rate-limited diagnostic, counted per
  `(app, g, pair)`; a high count is evidence for a `PI` or quirk row.
- `patch-tie(x, g, [(p, v)...])`: two matching patches of equal specificity set different values.
  Winner by `ext_id` (as K7 in the add-on report) + one diagnostic per pair (shown in the add-on
  manager).
- `seq-collision`: two facts with equal seq and different payload in one register (bug in a
  connector); winner by `vkey`, logged as an error.

Shadowing across layers (a patch over an observed value) is *intent*, not conflict; it is recorded
in the trail only.

**Provenance trail** `explain(x, g)` = ordered list of candidates, each
`(layer, source, rule id (quirk/patch), seq, value, status)` with status in
`won | shadowed | unknown | stale | no-value | masked-by-repair`, plus the repair action and the
`fallback` flag. Requirements (tested, §5):

- **completeness**: every fact present in a register for `(x, g)` appears exactly once;
- **soundness**: the effective value equals the `won` entry of the winning layer, or `R`'s
  output with the mask recorded, or the default.

**Storage [H].** Hot path: `ProvTag: u64` per node = 16 groups × 4 bits (2 bits layer, 2 bits
source slot) + `fallback`/`stale` bitmasks (`u32` each). The full trail is reconstructed by
running `FUSE` in trace mode over the retained registers: exact because `FUSE` is pure and its
inputs are kept. Cost only when the user asks "why" (or a test does). For the end-to-end answer
"why did gorca say X", the chain is: utterance segment (`MsgId` + field rule id, already requested
as a test feature) → `(node, group)` → `explain` → fact `seq` → optional ingress ring buffer.

---

## 2. Temporal consistency

### 2.1 Versions

Per node the fused entry carries a **local version vector**
`V(x) = (seq_atspi(x), seq_newton(x), seq_user(x))` (max seq over its registers) and the
**global epochs** it was fused under: `E = (patch_set, quirk_table, trust_table, catalog of
derived rules)`. Global epochs are app-wide scalars (one `AtomicU64` each), not per-node data,
like the mirror's existing O(1) `invalidate_all` stale epoch.

`sem_rev(x)` is bumped **iff the effective `Semantics` changed** (output-defined). Input-only
churn (duplicates, a fetch confirming the same value, a provenance switch with an equal value)
does not invalidate presentation caches keyed by `(node, sem_rev, name_rev, report_epoch,
lang_epoch)`.

### 2.2 Snapshot reads without locks

The fusion point is the **single writer per app** that already exists (the app actor writing the
mirror; the doc actor writing the web mirror). Connectors send normalised source records; they
never write effective semantics.

- **Per node [P from MS-1/MS-2]:** the effective record is published as one immutable
  `Arc<Fused>` through the entry's `ArcSwap`; readers are wait-free and see a value that is the
  `FUSE` of some register state. No torn records.
- **Web document:** the left-right publication (`crates/gorca-doc/src/web/publish.rs`) already
  gives a whole-document consistent cut; fusion results go into the back copy before `publish()`.
- **Several app-mirror nodes (focus + ancestors) [H]:** per-node consistency is enough for speech
  (an ancestor state one batch older is harmless; ST rules diff per node). If a consistent cut is
  ever needed (tests, P10 determinism), use a seqlock-style validation: the writer bumps an app
  `commit` counter to odd before and even after a batch publish; a reader records it, loads its k
  nodes, re-reads, retries at most twice, then proceeds flagged `torn`. Two atomic loads, the
  writer never waits.
- **Global epoch change** (add-on installed, quirk toggled, `PI` row changed): bump the epoch;
  the writer re-fuses all nodes in background chunks (≤ 1 ms per tick, focus chain first);
  a reader that sees `entry.E < current E` may run `FUSE` itself on the retained registers
  (pure, no write-back) so presentation is correct immediately. 100k nodes × ~200 ns ≈ 20 ms of
  background work **[H]**.

### 2.3 Freshness

The effective group inherits the freshness of its winner: `stale` if the winning register is
older than the source's invalidation; `fallback` if a higher-priority source is `Unknown`.
Rules for presentation **[H]**:

- **F6 provenance-only transitions are silent.** When the winner's *source* changes but the value
  does not (fallback → primary arrives with the same value), nothing is announced; when the value
  differs, ST rules see a normal change. This requires that provenance is **not** part of the
  `Semantics` value compared by the ST differ (ADR-0022 §2.4) - see amendment A1.
- A `stale` winner on the focus path triggers the existing deadline-bounded refetch; presentation
  never waits for it.

### 2.4 Incremental view maintenance

Event on `(src, x, g)`:

1. Register join. **If the register did not change, stop** (duplicates and late replies cost one
   compare - idempotence is also free dedupe).
2. Mark `x` dirty (the coalescer batch makes the dirty set a set: k events on one node in a batch
   cost one recompute).
3. At batch commit, recompute S0 of dirty nodes. If `x`'s S0 record changed, also dirty the nodes
   whose S1 selectors read `x` (reverse selector index restricted to the *properties the selector
   reads*: a `checked` storm never dirties a selector that reads only `role`/`name`).
4. Recompute S1..R for dirty nodes in depth-descending order; **early cutoff**: if the final
   record is unchanged, do not publish and do not propagate. If the final name changed, dirty the
   parent (S3 derived name).

**Cost bound [P for the structure, H for constants].** Per batch:
`Σ_{dirty x} (c_S0 + c_F) + Σ_{x with changed S0 on a selector-read property} fanout_sel(x)·c_F
+ Σ_{x with changed final name} depth_content(x)·c_F`, where `depth_content` stops at the first
ancestor that does not take its name from content (landmarks, documents, groups: typically ≤ 3),
and `fanout_sel` is bounded if ADR-0019 limits selector radius (parent/child/descendant ≤ r).
Global epoch changes are O(n) but amortised in background chunks.

Measured on the prototype (400-node trees, 47k events per configuration, random values - a
pessimistic workload since every write is a real change attempt) **[M]**:

| batch size | S0 recomputes / event | node fusions / event | published / event | early cutoffs / event | incremental ≠ full |
|---:|---:|---:|---:|---:|---:|
| 1 | 1.00 | 2.52 (2.29 without the 26 patch-set epochs) | 0.29 | 2.00 | 0 |
| 16 | 0.91 | 2.23 | 0.29 | 1.76 | 0 |
| 64 | 0.84 | 1.91 | 0.28 | 1.44 | 0 |
| 512 | 0.45 | 0.75 | 0.21 | 0.37 | 0 |

Rust estimate **[H, U]**: S0 for the touched groups ≈ 30-50 ns, a full node fusion over ~18
groups × ≤ 6 candidates ≈ 100-200 ns, a publish (allocate + copy ~200 B + `ArcSwap::store`)
≈ 100-150 ns. Worst row: 1·50 + 2.5·200 + 0.3·150 ≈ 0.6 µs/event → 6 ms CPU per second at
10k events/s (0.6 % of one core), below the ADR-0004 ingress budget. Real storms (`showing`,
`bounds`, repeated `selected`) are mostly no-ops at step 1 and cheaper. **Gate it with a bench**
(B-FUSE, §7) before relying on the number.

**Storm guard [H].** Per-batch work budget (e.g. 2 ms); leftover dirty nodes carry to the next
tick, focus chain and active descendant first. Because the result is order-independent, deferring
work changes latency only, never the final state.

### 2.5 A precedence bug the prototype found

The first prototype filled an empty name from content (S3) whenever the merged name was empty,
including when a **user override explicitly set the name to `""`**. The completeness/soundness
check flagged it on random workloads. Rule adopted: S3 fills only when the winning layer is
observed or default. The same rule must hold for patches (`set name = ""` is a decision). This is
also why `Unknown` and `Absent` must be different values in the registers.

---

## 3. Two connectors on the same app

### 3.1 What actually happens on GNOME (from the sources)

- GTK4 (Newton branch) picks **one** backend per process: the `a11y_backends[]` array lists
  AccessKit first, then AT-SPI, and the first that creates a context wins unless `GTK_A11Y` names
  one (`refs/newton/gtk/gtk/gtkatcontext.c:643-714`). A GTK app never natively exports both trees.
- Double exposure arises when the AT-SPI tree is **a translation of the AccessKit tree**: the
  Newton `atspi-adapter` ("receives accessibility information via the Wayland a11y-consumer-v1
  protocol and uses that information to implement AT-SPI", `refs/newton/atspi-adapter/README.md`,
  and it "inherits that adapter's limitations, which currently include lack of text support"), or
  an app using AccessKit's own Unix adapter.
- In both translation cases the AT-SPI object path encodes the AccessKit node id:
  `"/org/a11y/atspi/accessible/{adapter}/{u128(FullNodeId)}"`
  (`refs/accesskit/adapters/unix/src/atspi/object_id.rs:14-42`).

Consequences:

1. **Information order:** the translated AT-SPI view is a function of the AccessKit tree, so it
   carries no information Newton lacks (it only loses some: text). **[P for translation; U for
   future adapters that add data.]**
2. **Default: per-app connector dedupe.** When an app is visible through Newton, the AT-SPI view of
   the same app is suppressed (not fused). App identity: AT-SPI bus name → PID
   (`GetConnectionUnixProcessID`) vs the Wayland client PID on the Newton side **[U: the Newton
   protocol/mutter must expose it; and the atspi-adapter serves many apps from one bus name, so
   the mapping key there is the adapter's per-app root, not the PID]**.
3. **Node-level fusion** is kept for the general case (an app with two genuinely independent
   exports, a future engine, or the transition period) and runs only on nodes matched with an
   exact key.

### 3.2 Identity matching

A matching is a partial injection `μ: X_B ⇀ X_A`. Fusion only joins registers of `x_A` and
`μ(x_B)`; unmatched nodes of the non-primary source are not presented. A false match is worse than
a miss (it merges two different controls' states), so tiers are ordered by precision:

| Tier | Key | Cost | Precision |
|---|---|---|---|
| 0 | AccessKit id parsed from the AT-SPI path (translation case) | O(1) per node | exact **[P]** |
| 1 | author id unique in the window (AccessKit `author_id` ↔ AT-SPI `AccessibleId`/`id` attribute) | O(n) hash join | exact when unique and present **[U: per toolkit]** |
| 2 | top-down alignment: for each matched pair, align the children lists (unmatched transparent generic nodes of A expanded) by LCS on `(coarse role, name)`, accept exact-label pairs and role-only pairs unique between anchors | O(Σ k_a·k_b) ≈ 1-2 label comparisons per node **[M]** | heuristic |
| - | tree edit distance (Zhang-Shasha / APTED) | O(n² … n³): ~1e9-1e12 at 10k nodes | not usable online |

Measured (2000-node trees, 50 % of generic nodes dropped in B, 40 trials per row) **[M]**:

| key coverage | renamed nodes | recall | false-match rate | label comparisons / node |
|---:|---:|---:|---:|---:|
| 100 % | 0-20 % | 1.000 | 0.000 | 0 |
| 50 % | 0 % / 5 % / 20 % | 0.962 / 0.986 / 0.943 | 0.0006 / 0.0006 / 0.0005 | 0.8 |
| 0 % | 0 % / 5 % / 20 % | 0.880 / 0.845 / 0.685 | 0.0008 / 0.0015 / 0.0012 | 1.6-2.0 |

Reading: without keys, 12-32 % of nodes stay unmatched and ~0.1 % are matched wrongly. That is
acceptable for diagnostics (a "connector diff" tool, the P9-style engine-equivalence test), not
for merging states into what gorca speaks. **Recommendation: fuse only on tiers 0-1.**

### 3.3 When to trust which

- The connector whose tree is **native** (not a translation) is primary for every group; the
  other may only fill groups the primary leaves `Unknown` *and* declares in its capability mask
  (fallback flagged).
- Groups where freshness dominates (text, caret, value, states) are taken from one source per app,
  never mixed, to avoid alternating sources under a storm.
- Persistent `observed-disagree` counts on a matched pair are a signal to add a `PI`/quirk row,
  not to switch automatically (no adaptive trust: it would make output depend on history).

---

## 4. Proven vs heuristic

| Claim | Status |
|---|---|
| Registers are ACI; FUSE over registers is order-, duplicate- and batching-independent | **[P]** + [M] 0/720 mismatches incl. trails and conflict reports |
| Cross-source arrival order cannot be used deterministically | **[P]** + [M] naive LWW 660/660 divergent |
| Stratified FUSE terminates, unique result | **[P]** (acyclic strata) |
| `R` idempotent | **[P]** + [M] 500 random records |
| Incremental = from-scratch | [M] 0 mismatches over ~330k events, 4 batch sizes |
| Override dominance, locality, reversibility | [M] 200 runs: 121 dominant, 79 masked by `R` (recorded), 0 locality violations, 195/195 reversible |
| Exact identity across connectors in the translation case | **[P]** from the AccessKit path format |
| Structural matching quality | [M] synthetic trees only |
| Cost constants, 10k ev/s headroom | [H] / **[U]**: no Rust benchmark yet |
| Trust table defaults | [H] |
| Newton seq per tree update; PID mapping for Newton clients | **[U]** |

---

## 5. Testable properties (input to the covering-array plan)

Fusion properties are **run-factor relations** like P1-P10 in
`docs/reports/aria-test-coverage-math.md`: they compare runs that differ only in delivery, sources
or overlays, so these factors are crossed per page row, not covered pairwise.

| ID | Property | Oracle |
|---|---|---|
| F1 | **Order independence**: canonical, reversed and k shuffled deliveries (with duplicates and random batch cuts) give identical effective semantics, trails and conflict reports | equality across runs |
| F2 | **Idempotence**: replaying any suffix of the log (resync) changes nothing; no-op events publish nothing (`sem_rev` unchanged) | equality + publish counter |
| F3 | **Provenance completeness and soundness** (§1.5) | per `(x, g)` check against the registers |
| F4 | **Override dominance**: after a user override `v` on `(x, g)`, effective = `v`, or `R` masked it and the trail says so | value + trail |
| F5 | **Locality**: an override on `(x, g)` changes only `D(x, g)` = `{(x, g)}`; for `role`, all groups of `x` (repair); for `name`/`role`, ancestors' `name` (derived) | set inclusion |
| F6 | **Reversibility / no hysteresis**: override then `Clear` restores the previous state exactly; provenance-only transitions produce no utterance | equality; ST output count = 0 |
| F7 | **IVM correctness**: incremental state = `FUSE` from scratch after every batch | equality (test feature `fuse_full_check`) |
| F8 | **Source equivalence**: the same fixture through AT-SPI only, Newton only, and both (matched by key) gives equal effective semantics modulo a declared capability table | equality modulo table (the fusion analogue of P9) |
| F9 | **Freshness propagation**: effective `stale`/`fallback` flags equal the winner's; priority, not freshness, picks the winner (a stale primary beats a fresh secondary, flagged and refetched) | flag check |
| F10 | **Invariants after fusion**: SM-1, SM-2, SM-5 hold on every published record, including after patches re-role nodes | predicate |

Run factors to add to the generator: `delivery ∈ {canonical, reversed, shuffled×2 with dup}`,
`connectors ∈ {atspi, newton, both}` (Newton fixtures once M19 exists; until then a fake
AccessKit source in the test kit), `fetch ∈ {eager, lazy, invalidate-then-refetch}`,
`overlay ∈ {none, patch, user, patch+user}`. Crossing all is 4·3·3·4 = 144 per row; F1 needs only
`delivery` crossed with everything else fixed, F8 only `connectors`, F4-F6 only `overlay`, so a
"one factor at a time around the row's base config" design costs 4+3+3+4-3 = 11 runs per row.

---

## 6. Measured results (reproduce with `semantic_fusion.py all`)

```
selftest ok
order-independence: runs 720, reg/sem/prov/conflict mismatches 0/0/0/0, provenance_errors 0,
                    naive_runs 660, naive_mismatch 660, 4.9 s
override:  runs 200, dominance_ok 121, masked_by_repair 79, violations 0, reversible 195/195
ivm:       mismatches 0, events 141,632 (batch 64): base 0.845, fuse 1.89, published 0.281 per event
match:     see §3.2 table
```

Workload caveats: random trees of 40-400 nodes, uniform random values (pessimistic for publish
rate), 6 random patches, two observed sources disagreeing often (2,792 `observed-disagree` reports
in 60 trials: real engines disagree far less). Structure changes (`children-changed`, reparenting)
are not simulated; the static tree is the main modelling gap.

---

## 7. Porting notes (Rust)

- `gorca-model::sem::fuse` (pure, no tokio): `fn fuse(inputs: &FuseInputs, tables: &FuseTables,
  children_final: impl Fn(NodeKey) -> Option<&Semantics>) -> (Semantics, ProvTag, Flags)` and
  `fn explain(...) -> Vec<TrailEntry>` sharing one generic implementation with a `Tracer` type
  parameter (no-op tracer on the hot path, monomorphised away).
- `FuseInputs` per node: `[Option<SrcRecord>; 2]` (observed, post-quirk, with per-group `seq` and a
  `known: GroupMask` - `Unknown` = bit clear), `inval_seq: [u64; 2]`, `life: [(u64, bool); 2]`,
  user overrides as a small sorted vec (rare). Patches: global `Arc<PatchSet>` with an epoch.
- Register join is `if (seq, key) > cur { replace; true } else { false }` - the `false` branch is
  the dedupe fast path.
- `ProvTag(u64)`: 4 bits per group. Keep it **outside** `Semantics` (in the fused entry) so
  `Semantics: Eq` compares values only (F6).
- Enumerate groups as a `#[repr(u8)] enum Group` with `const ALL: [Group; N]`; `PI` as
  `[[SourceSlot; 2]; N]` per app profile.
- Tests: port F1-F10 as proptest properties over a fake two-source connector; vectors TV1-TV6
  from `selftest()`.
- Bench **B-FUSE**: 10k-50k events/s of mixed `state-changed`/`property-change` on a 100k-node
  mirror with 100 patches; gate: fusion + publish p99 ≤ 2 µs/event, no ingress lag regression.

---

## 8. Implications for draft ADR-0022 (suggested amendments)

- **A1** Replace the single `provenance: Provenance` field inside `Semantics` (report §3.1) by a
  per-group `ProvTag` stored next to it in the fused entry, plus `explain()`. A field inside
  `Semantics` makes provenance-only switches look like semantic changes to the ST differ.
- **A2** Name the **one fusion point**: the per-app single writer (app actor / doc actor) runs
  `FUSE`; connectors and the doc source deliver normalised *source records*, never effective
  semantics. SM-4 (one normaliser per connector family) stays; add **SM-8**: effective
  `Semantics` is produced only by `FUSE`.
- **A3** SM-2 conflict resolution moves from "connectors resolve once" to two places: the
  normaliser canonicalises its own record, and `R` re-establishes SM-1/2/5 **after** patches and
  overrides (a patch can re-role a node).
- **A4** ADR-0019 patch semantics: selectors match the pre-patch (observed) layer (cascade rule);
  equal-specificity ties broken by `ext_id` with a diagnostic; explicit empty values are final.
  Bound selector radius so the reverse index has bounded fan-out.
- **A5** `sem_rev` bumps iff the effective value changes (output-defined).
- **A6** Registers are three-valued (`Unknown` / `Absent` / value); `PropMask` fetch state maps
  to `Unknown`.
- **A7** Verbosity, `ReportFlags` and language stay out of fusion (compose-time, cache key).
- **A8** Two connectors: per-app dedupe (native tree wins) by default; node fusion only on exact
  keys (AccessKit id in the AT-SPI path, unique author ids).

## 9. Open questions

- Newton protocol: per-tree update ordering and how a consumer learns the client PID / maps an
  `atspi-adapter` app to its Newton tree (verify in `refs/newton/consumer`, `refs/newton/mutter`).
- Whether any target app will expose two independent trees at once; if none, node-level fusion
  across connectors can ship later than the layer machinery.
- Structural events (subtree replacement) and their interaction with derived names and selector
  indices are not simulated.
- Rust constants (B-FUSE) are unmeasured.

---

## 10. Fact sources (ADR-0024 r2): reference oracle, properties F11–F19, entry size (M22a-A3)

Added 2026-10-19. Reproduce: `. ./env.sh && python tools/research/semantic_fusion.py all2`
(≈ 26 s) and `... corpus --out tests/fixtures/fuse`. Sections 1–9 are unchanged and `all` still
prints the §6 numbers byte for byte (checked by diff before/after).

### 10.1 Entry size: 104 vs 128 vs 160 B (owner: ≈ 120 B or more is acceptable)

`SemEntry` lives behind `Arc` (dual-carry in `NodeSnapshot` and `NodeMeta`), one glibc
allocation each (no global allocator is set in the workspace). `ArcInner` adds 16 B, glibc
rounds `n + 8` up to 16 (measured with `malloc_usable_size`, `research/semsize/malloc_chunk.c`):

| `SemEntry` | heap chunk | lines touched | 1k | 10k | 50k | 100k |
|---|---|---|---|---|---|---|
| 104 B (r2) | 128 B | 2–3 | 0.13 MB | 1.3 MB | 6.4 MB | 12.8 MB |
| 128 B | 160 B | 3 | 0.16 MB | 1.6 MB | 8.0 MB | 16.0 MB |
| 144 B | 176 B | 3–4 | 0.18 MB | 1.8 MB | 8.8 MB | 17.6 MB |
| 160 B | 192 B | 3–4 | 0.19 MB | 1.9 MB | 9.6 MB | 19.2 MB |

Host caches (Core Ultra 9 285H): L1d 48 KiB per P-core (no layout fits 1k entries), L2 3 MiB per
P-core (10k entries fit at every size), L3 24 MiB shared with the desktop (50k fits at every
size; 100k at 160 B uses 80 % of it, i.e. spills under load).

Measured scan cost (`research/semsize/scan.c`, pinned core, caches evicted, live desktop, not a
quiet host; raw rows in `research/semsize/results.txt`):

| Operation | N | 104 B | 128 B | 160 B |
|---|---|---|---|---|
| hot-field read (role + mask), cold | 50k | 0.19–0.33 ms | 0.22 ms | 0.17–0.20 ms |
| full-entry copy (CoW re-fuse), cold, allocation order | 50k | 0.27 ms | 0.33 ms | 0.39 ms |
| full-entry copy, cold, random order | 50k | 0.35 ms | 0.41 ms | 0.47 ms |
| full-entry copy, cold, random order | 100k | 0.79 ms | 1.22 ms | 1.34 ms |

Hot-field reads show no consistent size dependence (prefetch and noise dominate, ±50 %
between runs). Full copies scale ≈ 7 / 8–9 / 9.5–11.5 ns per entry.

What this means for the latency goals:

- **Key → speech** reads the focus chain and its context: ≤ 50 entries, < 0.6 µs at any size;
  the 160 B penalty is < 0.3 µs against 15 ms (2·10⁻⁵).
- **Quick nav** does not scan entries: `gorca-doc` answers `next(Cat)` from the treap's subtree
  category aggregates in O(log n) (`crates/gorca-doc/src/web/doc.rs:983,1024`) and reads one
  target entry. Elements-list style scans read k ≤ n entries: ≤ 0.2 ms for 50k at any size.
- **Fusion (M-11, ≤ 2 µs/event)**: ≈ 1.9 FUSE calls per event (§6) × one entry copy; 160 vs
  104 B adds ≈ 5–9 ns per event, < 0.5 % of the budget.
- **Synchronous revocation re-fuse** (FS-7, ≤ 200 nodes, ≤ 0.5 ms): copy share 1.4 / 1.7 / 2.3 µs.
- **Background full re-fuse** (epoch change, 50k doc): copy share 0.35 → 0.47 ms, in chunks.
- **RSS gate** (100k nodes ≤ 25 MB, all carriers): 12.8 / 16.0 / 19.2 MB for the entries alone;
  160 B leaves 5.8 MB for `SemText`, names and side tables, 128 B leaves 9 MB.

Headroom value. With 16 groups, `orientation` and `sort` share `flags`' provenance and hedge, and
the next enrichment (an add-on `layout.css_hidden`-derived flag, landmarks/`roledescription`
split, ML `state` guesses, per-group taint for WIT `prov-info`) needs a layout change plus an ADR
amendment (§2.12 "a 17th group requires a layout change"). A 32-group capacity costs 24 B:

| Layout | Semantics | ProvTag | hedge | taint | masks ×3 | size (rustc, x86_64) |
|---|---|---|---|---|---|---|
| r2-16 | 64 | u64 | u32 | — | u16 | **104 B** |
| **x32** | 64 | `[u64; 2]` | u64 | u64 (2 bits: weakest input tier) | u32 | **128 B** |
| x32, `Semantics` 80 B | 80 | `[u64; 2]` | u64 | u64 | u32 | 144 B |
| x64 | 80 | `[u64; 4]` | u128 | u128 | u64 | 192 B |

(`research/semsize/layout.rs`; a `u128` ProvTag gives the same 128 B here but forces 16-byte
alignment on every container, so prefer `[u64; 2]`.) A separate taint field is worth its 8 B: in
the oracle `ObservedSecondary`, `Inferred` and `DerivedTainted` winners carry tiers 2 or 3 and
ProvCode alone cannot say which (needed by LB-5, §2.3.3 act-on-what-was-spoken, WIT `prov-info`).

**Recommendation (high confidence on the latency claim, medium on the exact gate):** layout x32
— `GROUP_CAP = 32`, `GroupMask = u32` for `known`/`stale`/`fallback`, `ProvTag([u64; 2])`,
`hedge: u64`, `taint: u64`; register 18 groups now (append `orientation` = 16, `sort` = 17 after
the frozen 16, codes unchanged, append-only); `const` gates `size_of::<Semantics>() <= 80`,
`size_of::<SemEntry>() <= 160` (expected 128 today), `GROUP_COUNT <= GROUP_CAP`; keep the 100k-node
RSS ≤ 25 MB gate as the real ceiling. Margin to 15 ms p99 is > 10⁴× on every per-key path. The
oracle and corpus are parameterised (`--layout` via `LAYOUTS`; both files are emitted), so the
ADR can pick either without rework.

### 10.2 Oracle model (what was added)

Same discipline as §1: every input is a register with a max-join; `FUSE` is a pure function of
registers, static tables and explicit inputs.

| Register | Key | Join | Purpose |
|---|---|---|---|
| fact | (source, anchor, group) | max (seq, record) | Secondary / Inferred semantic and evidence facts; record = (value \| retract, p_milli, basis vis_rev, bind_gen, admitted_t, ttl, scope) |
| attr | (primary, x) | max (seq, dom_id) | primary-reported HTML id; drives `DomId` binding |
| vis | x | max rev | `vis_rev` as an explicit input |
| clock | — | max t | TTL is a lifecycle input (no timer inside FUSE) |
| epoch | source | max (n, state) | mask / revoke / detach as timeline inputs (M-10) |

- **Admission** (`admit`) is pure in one fact and static tables: class, scope pinning (FS-11),
  grant group, anchor kind, value well-formed, `p_milli` range, page strings over the cap
  truncated and counted, Inferred default TTL. Rejected facts are never stored.
- **Binding** is recomputed from registers: `Node`/`Exact` → Exact, `Region` → Geometric,
  `DomId` → Unique iff `dom_count == 1` **and** exactly one live mirror node has the id, else
  Heuristic, none → Unbound (parked, listed under `parked`).
- **BindGen definition (proposal):** the primary seq at which the anchor's current target
  acquired it (life seq for Node/Exact/Region, attr seq for DomId). It changes exactly when the
  anchor resolves to another node or the node is re-created and is monotone, so a Rust `u32`
  counter bumped on the same events is equivalent. Facts carry the generation the writer
  stamped at admission (the differential test feeds stamped facts; admission order otherwise
  leaks into the result).
- **Validity of a non-primary register** (trail status in this order): `retracted`, `masked`
  (Paused/Quarantined/Revoked), `detached`, `unbound` / `weak-binding` (below `min_bind` of the
  group class), `rebound` (stamped gen ≠ current), `stale` (Inferred or `visual.*` basis <
  `vis_rev`), `expired` (t + ttl ≤ clock), `low-confidence` (below floor), then `ok` with
  `ConfClass` from the grant thresholds (High ≥ 970, Probable ≥ 900, Possible ≥ floor).
- **Strata:** S-1 quirk → S0 primaries by `PI`, secondaries appended only for the profile's
  static `inexpressible` groups (else trail `not-candidate`) → S1 patch → S2 user → S3 derived
  (content name; then `hint.title_attr` evidence for button/link/img/checkbox) → S4 inferred
  (Inferred `name`/`description` facts and the toy rule `visual.ocr_text` → name; rank
  `(ConfClass, kind order, source id, direct-before-rule, value, anchor, seq)`) → defaults → R.
  S3 and S4 fill only Unknown, observed-empty or default (A6).
- **Hedge and taint per group:** primary / patch / user / untainted derived / default → 0/0.
  Secondary or Inferred winner → its class, capped at Probable when the source is T3; taint = its
  tier. DerivedTainted → weakest input class (T3 cap), taint = max input tier. A repair-masked
  group becomes `Default`, plain.
- **Utterance model (LB-5, H-3):** a state-group change speaks only when the new winner is
  Observed-primary, Patch or User.

### 10.3 Results

`sources` runs 150 random workloads per configuration (trees of 30 nodes, 500 events, two
primaries, one Secondary T3, two Inferred T2, two Inferred T3, 4 patches, users, vis bumps,
ticks, DomId moves, epochs, 3 % out-of-scope facts, 2 % malformed values).

| Property | r2-16 | x32 | x32, content rule "all" |
|---|---|---|---|
| F1 order independence (reversed + shuffled with dups), runs / mismatches | 600 / 0 | 600 / 0 | 600 / 0 |
| Predicate checker (F11 floor, F12 secondary/hedge/taint, F13, F15, F18, FS-5) violations | 0 | 0 | 0 |
| F11 p moved below floor: runs / non-local changes | 136 / 0 | 132 / 0 | 132 / 0 |
| F12 protected cells checked / changed | 11,713 / 0 | 12,824 / 0 | 12,824 / **23** (all `name`) |
| F13 vis bump: runs / inferred survived / utterances | 136 / 0 / 0 | 132 / 0 / 0 | 132 / 0 / 0 |
| F14 revoke/pause/quarantine/detach ≡ run without source: runs / mismatches | 3000 / 0 | 3000 / 0 | 3000 / 0 |
| F15 ambiguous DomId registers checked / bound Unique | 5,488 / 0 | 5,504 / 0 | 5,504 / 0 |
| F16 (pure) flood 1,500 facts, cap 64: drops / other-source registers changed / protected changed | 42,262 / 0 / 0 | 46,645 / 0 / 0 | 46,645 / 0 / **23** |
| F18 DomId moved: runs / old-gen facts won | 58 / 0 | 52 / 0 | 52 / 0 |
| F19 out-of-scope facts: runs / result differs from run without them | 150 / 0 | 150 / 0 | 150 / 0 |
| LB-5 utterances caused by non-primary facts | 0 | 0 | 0 |

Winners over all cells (x32): ObservedPrimary 13,664, Default 23,575, User 713, Derived 320,
Inferred 312, Patch 127, ObservedSecondary 71, DerivedTainted 62. Admission: 23,027 accepted,
963 truncated, 734 scope rejections, 417 group, 330 value.

**Mutation check** (`research/semsize/mutants.py`, 30 smaller workloads each): ignoring BindGen,
epochs, `vis_rev`, the floor, the binding minimum, the T3 hedge cap, the inexpressible rule (H-3)
or scope pinning is each caught (by F18, F14, F13, F11, F15, F12, F12, F19 respectively). The
content-rule mutant is not caught at that size (23 hits needed 150 × 30-node runs): F12 needs the
full run.

### 10.4 Findings that change or sharpen ADR-0024

1. **Name-from-content breaks F12 as written (proven by counterexample, 23 cells).** If S3
   concatenates all children's final names, an S4 or tainted name on one child changes an
   untainted Derived parent name ("Close" → "Close Logo", now tainted). Rule adopted in the
   oracle: **content derivation uses untainted children's names; tainted names are used only if
   no untainted part exists** (then the result is DerivedTainted). With it F12 holds in all runs.
   Proof sketch: by induction bottom-up, protected child names are unchanged by adding
   non-primary facts, so the untainted part list is unchanged whenever it is non-empty.
2. **F12 scope needs the fill rule.** An Observed-primary *empty* name or description is
   fillable by S3/S4 (A6), so it cannot be "protected". F12 should read: "never changes a group
   whose winner is User, Patch, untainted Derived, or Observed-primary with a non-empty value
   (for fillable groups)".
3. **S4 ranks by the capped class.** A T3 extension at p = 990 (High) ranks as Probable, below a
   T2 OCR fact at High (TV10). Otherwise a hedged T3 value could outrank a better-calibrated
   first-party model. ADR §2.2 should say "ConfClass after the tier cap".
4. **`ProvCode::Fallback` is never produced:** the oracle encodes fallback as
   `ObservedPrimary` + the `fallback` mask bit. Either drop the code or define when it is used.
5. **BindGen must be stamped at admission and carried with the fact** (10.2) for the result to be
   order-independent; ADR §2.3.2 implies it, the spec should state it for B1b.
6. **F17 is not checkable in a pure oracle**: speech reads a published snapshot, so "removing
   in-flight non-primary facts gives the same first utterance" holds by construction. It must be
   tested in C5 against the real pipeline (refine tails ≤ 1, M-20).
7. **F16 with a register cap is order-dependent** (which keys get in). The oracle checks only
   isolation (other sources' registers and protected cells unchanged, cap respected). Corpus
   traces never hit the cap, so B1's differential test can stay order-free.

### 10.5 Corpus (`tests/fixtures/fuse/`)

`fuse-r2-16.json` (1.33 MB) and `fuse-x32.json` (1.43 MB), byte-identical across runs (sha256
`5d7df744…`, `21c225d1…`). Each holds a schema header (full text in the file), the static tables
(group codes incl. evidence bit 15, ProvCode, ConfClass, BindQuality, sources, grants, floors,
`min_bind`, `inexpressible`, `PI`, thresholds, defaults) and 30 cases: TV7–TV20 (one rule each:
inexpressible-only secondaries, dynamic Unknown, taint through derivation, S4 ranking, S4 below
S3, floor, `vis_rev`, BindGen rebinding, ambiguous DomId, TTL, revocation, scope pinning,
user/patch over S4, truncation) and 16 seeded random workloads. Per case: events in canonical
order, four deliveries (canonical, reversed, two shuffled with duplicates and batch cuts, given
as event indices), admission result per event, and per node the expected values, ProvCode,
hedge, taint, `known`/`stale`/`fallback`, the layout bit encodings (`prov_tag`, `hedge`,
`taint`, masks) and the full `explain` trail. The generator asserts that every delivery gives the
expected state before writing.

### 10.6 Porting notes (B1a/B1b/B3)

- Port `admit()` and `World2.status()` as two pure functions; keep the status order of 10.2 so
  trail statuses match the corpus.
- The side table key is `(source, anchor, group)`; binding is resolved at `FUSE` time from the
  anchor index, never cached in the register.
- Sort keys are total orders (10.2); Rust must use the same tuple order, with source id = the
  source's name string (the corpus `sources` table also gives slots).
- Differential test: for each case and each delivery, ingest in order, then compare entries,
  encodings and trails; `known` is set by any layer before defaults, cleared by nothing.

### 10.7 Unverified

- Toy tables: `inexpressible` rows (`live`, `orientation` for the `firefox` profile), grants,
  floors and kinds are illustrative, not engine facts (D-4 needs real rows from A2).
- Only `Node`, `DomId` and a toy `Region` anchor are modelled; `Structural` and `Content` are not.
- Static trees (as in §6): structural events and synthetic nodes are not simulated.
- Size numbers come from a C model of `Arc` + glibc on a live desktop host; the Rust A4 bench on
  the real layout supersedes them.
