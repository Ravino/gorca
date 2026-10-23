# Plugin facts in FUSE: soundness, staleness and the interactive budget (ADR-0026 MQ-1..MQ-3)

Date: 2026-10-23. Author role: math-researcher. Input to ADR-0026 (Proposed), §2.4, §2.5, §2.6 and §6.
Reads: ADR-0026 §2.4–§2.6 and §6, ADR-0024 (§2.2–§2.4, §2.10, §7), ADR-0022 §2.2–§2.4,
`crates/gorca-fuse/src/{strata.rs,ivm.rs,side.rs}` at `ae0a6d2d`.

Script: `tools/research/plugin_fusion/check.py` (project venv; `python check.py all`, about 6 s,
deterministic seeds). It asserts every claim marked [P] below and prints the quoted numbers.

Legend: **[P]** proven (argument given and checked numerically), **[C]** counterexample (found by the
script), **[M]** measured elsewhere in the repo, **[A]** assumption, **[H]** heuristic or recommended
parameter.

---

## 0. Answers in one page

| Question | Answer |
|---|---|
| MQ-1 | **Not as stated.** Uniqueness and arrival-order independence of `FUSE` over fixed registers hold [P]. Convergence and order independence of the *published model* fail when plugin state depends on the snapshot history [C: 1297/3000 event sets give a batching-dependent result]; fused existence in views oscillates [C: period 2]; a self-trigger loops forever [C]. F12 fails for T3-trusted computed patches even with taint [C: 118 violations] because S3 loses the child's clean name. F18 fails whenever admission is not in the token's turn [C]. Restrictions PF-1..PF-7 restore all of it [P; 0 mismatches in 12 000 runs, 0 F12 violations in 4 000 models]. |
| MQ-2 | Token = `(t, R, M, g)`: one global writer stamp `t`, the read set `R`, the read-class mask `M`, and the target's `BindGen` at `t`. Per node keep one stamp per read class (6 classes). Stale ⇔ `∃ n∈R, c∈M: stamp[n][c] > t` or `gen ≠ g`. Zero false accepts [P], false rejects only on ABA (31 of 1288 fresh, vs 573 for one stamp per node). Update O(1) per changed class, check O(\|R\|), or O(1) with a reverse index. **`sem_rev` must not be in the token** (it rejects 100 % of fresh facts: the plugin's own admission bumps it); **`vis_rev` must not be either** (291 false accepts). |
| MQ-3 | **Progressive filling**: reserve `n·c_max` for the uncharged entry cost, order the n called plugins by predicted fuel (ascending), grant `a_k = min(cap, remaining/(n−k+1))`, return unused fuel. Guarantees `Σ ≤ budget`, a floor `min(cap, P/n)` for every plugin, and max-min fairness when predictions order correctly [P]. Completion 87.7 % vs 69.4 % (equal split) and 67.3 % (fixed-order greedy, 28 % of cheap plugins starved) at n = 16. Bound: `D ≤ n·c_max + ρ·(B − n·c_max)`, ρ = calibrated/worst-case fuel rate; = 0.3 ms only if fuel is calibrated on a memory-bound guest. No budget carry-over across focus changes. |

**Also found:** ADR-0026 §2.5.1 and the spike say "3M units ≈ 138 µs (≈ 22 units/µs)". The
arithmetic gives **21 739 units/µs** (≈ 22 units/ns). A budget built from "22 units/µs" would be
1000× too small (0.3 ms → 6 600 units ≈ 0.3 µs of guest work).

### Restrictions ADR-0026 must adopt

- **PF-1 View-functional S1.** A computed patch's value is a function of the call's view (S0 of R,
  static config, settings) only. Private plugin state may influence S4 facts only. Checked in
  `ext-test` by re-deriving in a fresh instance on the same view (differential); first-party (T2) S1
  plugins must pass it.
- **PF-2 S0-only views, structure included.** W-lane reads see S0 values **and S0 existence and
  structure**: no S1 `hide`, no `flatten`, no S4, no plugin categories. (CR-8 says "S0 only"; it must
  name existence and children explicitly.)
- **PF-3 Plugin facts never enter S0.** A plugin is never a `Secondary` source (ADR-0024 allows
  Secondary for reviewed add-ons; for WASM plugins that would close the feedback loop).
- **PF-4 Trigger rule.** `derive` is invoked on attach, on an S0 change (in class M) of a node in some
  live R of the plugin (reverse index), and on TTL refresh. Never on fact admission or on a fused
  (S1..S4) change.
- **PF-5 Taint-clean derivation.** A T3 computed patch value is tainted (H-5), and S3 name-from-content
  reads each child's **best untainted value** (the child's value below its tainted winners), using
  tainted values only when no untainted part exists. F12 gets the T3-patch clause (§2.4).
- **PF-6 Total S1 rank.** CR-2's key is `(tier, specificity desc, plugin id, anchor key)`. For
  computed patches the "patch id" is the anchor key (the register identity); two registers of one
  instance on one (node, group) then never tie.
- **PF-7 Token, not admission, fixes the basis.** `snapshot-token` resolves to `(t, R, M, g)` with
  `g = BindGen(target, t)`. The writer stamps `g` (not the admission-time generation) and rejects at
  admission when `g ≠` current. Drop `(sem_rev, vis_rev)` from the token definition (§3).
- **PF-8 Interactive budget** as §4: progressive filling, pre-charged imports, the unit fixed, fuel
  calibrated on the slowest guest shape, no OutOfFuel strike below the floor.

---

## 1. Model and assumptions

- [A] One writer per node (ADR-0022 SM-8); a writer turn applies a coalesced batch of primary
  records, recomputes S0, calls plugins, admits their effects, re-fuses and publishes. Nothing else
  runs between a synchronous W-lane call and the admission of its effects in the same turn.
- [A] `S0_t` = S0 of all nodes after the primary writes of turn t. It is a function of primary
  registers only (PF-3).
- Plugin p has private state σ_p. A call is `(σ', F) = derive_p(σ, view_p(S0_t, R))`, F a set of
  facts `(target anchor, group, value, layer)`. The host assigns per-instance seq in issue order.
- Plugin registers are keyed `(instance, anchor, group)`; within a register the newest seq wins
  (ADR-0024 Lemma 1). Registers of different instances are disjoint.
- `FUSE(n)` per node, strata S-1, S0, S1, S2, S3, S4, R as in ADR-0024 §2.2; S1 picks the maximum
  non-stale candidate under the static rank; S3/S4 fill only when the winner so far is Unknown,
  observed-empty or default.
- The script's model: a 7-node tree, a name group with values {"", a, b, c}, user overrides,
  1–6 plugins with random targets, random read sets (target + up to 2 nodes), random S1/S4 layer and
  tier, and a full taint and staleness implementation. It is small but exercises every stratum and
  the cross-node S3 edge, which is where all the counterexamples live.

## 2. MQ-1: soundness of computed patches

### 2.1 What holds unconditionally

**Lemma 2' (unique result, terminates) [P].** For fixed registers, `FUSE(n)` is unique. *Proof.* The
strata are evaluated in a fixed order; each stratum reads only lower strata of the node and final
values of children (S3), which are computed first (depth-descending order, a DAG on a tree). S1 takes
the maximum of a finite candidate set under a strict total order provided the key separates all
registers: with PF-6 the key `(tier, specificity, plugin id, anchor key)` is injective on registers
bound to one (node, group) because `(instance, anchor, group)` is the register identity and plugin id
determines the instance within a scope. Without PF-6 ("patch id" undefined for computed patches) two
registers of one plugin on one node, e.g. a `node` target and a `dom-id` target, can tie, and
`patch-tie` would have to fall back to admission order. ∎

**Theorem 1' (admission order independence for fixed calls) [P].** For a fixed set of calls and their
outputs, the published model does not depend on the order in which the writer admits the effects of
different plugins. *Proof.* Effects of distinct instances write disjoint registers; within an instance
the order is the instance's own seq. Register state is therefore the same for every interleaving, and
`FUSE` is a pure function of registers (Lemma 2'). ∎ The script randomises the call and admission
order in every turn (MQ-1a).

These are the statements ADR-0026 §2.4 "Determinism" relies on, and they are fine. The trouble is
upstream: *which calls happen, and what they return*, depends on how events were batched.

### 2.2 Counterexamples

**C-1 History state breaks order independence [C].** Let plugin p keep a counter and emit
`name = h(view) ⊕ counter`. The number of calls equals the number of turns in which R changed, which
depends on coalescer batching (timing), not on the final primary state. Over 3000 random event sets,
4 random batchings each, the final published model differed between batchings in **1297/3000** sets.
The same events in a different batching give a different spoken name: Theorem 1 of ADR-0022 (the
result is independent of cross-source arrival) does not survive. What survives is replay determinism
(same snapshot sequence → same model), which is weaker and useless for "the same screen reads the
same way".

**C-2 Fused existence in views oscillates [C].** Plugin A hides X iff Y is present in its view;
plugin B hides Y iff X is present. If views show fused existence (S1 `hide` applied): turn 1 both
visible → both hide; turn 2 both hidden → both unhide; period 2, forever. With S0 existence in views
the system is at a fixed point after one turn. `flatten` (a structure patch) is the same hazard for
`subtree`/`children` reads.

**C-3 Self-trigger loops [C].** If admitting p's fact dirties p's target and the watch prefilter
treats that as a reason to call `derive` again, a plugin with private state re-derives forever (the
script stops at 1000 calls; the emitted values have no period ≤ 32). Even a pure plugin pays one
useless extra call per admission.

**C-4 F12 fails for T3-trusted computed patches, even with taint [C].** Parent P is named from content,
children c1 = "a", c2 = "b" observed: P = "a b", untainted Derived (protected by F12). A T3 plugin
(trusted by the user, QP-3) patches c1 to "x" at S1. With patch taint (H-5), c1 is tainted, so the
ADR-0024 rule "use only untainted children" yields P = "b": a protected group on a node the plugin
did not target changed. Without taint P = "x b" (worse: an untainted name built from T3 input).
Script, 4000 random models: **175** off-target violations without taint, **118** with taint only,
**0** with PF-5. S4 additions: **0** violations in all modes (the fill rule never overrides a
non-empty child, so the clean part is never lost).

**C-5 F18 fails across an admission gap [C].** The ADR-0024 rule "stamp the current `BindGen` at
admission" is correct only when admission happens in the token's turn. Facts from `reactor.deliver`
of an async `request`, from timers, or from any deferred admission are computed from an old view. If
anchor a moved from u (gen 5) to v (gen 6) between the token and admission, stamping at admission
records 6, so a fact computed about u wins on v: exactly what F18 forbids. Stamping `g` from the
token (PF-7) makes the fact stale. `g` is a deterministic function of `t`, so the ADR-0024 concern
("admission order must not leak into the result", math §10.4 item 5) is still met.

### 2.3 Restored theorem

**Theorem P (convergence and order independence with plugins) [P].** Assume PF-1..PF-4 and PF-7 and
the staleness rule of §3. Let primary inputs be quiescent from turn k on (no primary record, no
request result, no timer effect). Then:

1. no plugin is called after turn k, the registers are constant, and the published model is constant
   (no oscillation);
2. the published model equals `Full(S0_k)`: the model obtained by calling every plugin once on S0_k
   and fusing, whatever the batching and call order before k.

*Proof.* (1) By PF-3 S0 depends on primary registers only, so quiescence fixes S0. By PF-4 a call
needs an S0 change in some R (or attach/TTL, which are inputs), so no call happens; registers
change only through calls (B-6 effects) or inputs; `FUSE` is pure (Lemma 2'), so the publish is
constant once the IVM dirty set drains (F7). (2) Take any plugin p and its last call before k, at
stamp t_p. If some node of R_p changed S0 (in class M) after t_p, PF-4 would have called p again,
contradicting "last". So `view(S0_k, R_p) = view(S0_{t_p}, R_p)`; by PF-1 the S1 values equal what a
fresh call on S0_k returns, and by §3 the fact is not stale (no stamp in R_p exceeds t_p). S4 facts
from stateful plugins equal the fresh ones only if they too are view-functional; otherwise (2) holds
up to S4 values, which are hedged. With PF-2 no plugin reads S1..S4 output, so there is no
cross-plugin dependency, and Theorem 1' gives independence from call order. ∎

Script MQ-1a: 3000 event sets × 4 random batchings × random call order: final model == `Full` in all
12 000 runs, 0 mismatches. A needed detail the script exposed: **attach** must derive every watched
target once (a plugin whose R never changes would otherwise never run).

Timers, `reactor` deliveries and `requests` are inputs. A plugin that keeps asserting from a timer is
not oscillation of `FUSE`; it is a source of events, rate-bounded by FS-10 (timers ≥ 50 ms, ≤ 16).

### 2.4 Invariants

- **F12 restated** (amend ADR-0024 §7): "adding an Inferred, Secondary or Annotation fact, or a T3
  Patch fact, never changes a group whose winner is User, Patch, untainted Derived or Observed-primary
  with a non-empty value, **except, for a T3 Patch fact, the groups it targets**." T2 computed
  patches are first-party author intent, exempt like declarative `[[patch]]` rows (CR-1). PF-5 makes
  it hold [P, script 0/4000]. *Proof sketch.* Off-target groups are reached only through S3 edges
  (content name, relation text, positions). With PF-5 every S3 rule reads a child's best untainted
  value; a T3 patch changes only its target's tainted winner, not its best untainted value, so every
  untainted Derived value elsewhere is unchanged; S4 never overrides a non-empty value (fill rule).
- **F18** holds with PF-7: a fact carries `g = BindGen(target, t)`; any later rebinding makes
  `g ≠ current`, the fact is stale and never wins (FS-5).
- **CR-8** must be read as PF-2 (structure and existence included), otherwise C-2.

Cost of PF-5: the "best untainted value" of a child equals its winner except when the winner is
tainted; then it is the child's S0 or clean S3 value, both already rebuildable from the stored S0 and
the published entry (fuse-memory design). One bit per (node, S3-read group) says "winner tainted";
no new register.

## 3. MQ-2: the minimal staleness bookkeeping

### 3.1 Formulation

A call at writer stamp t reads node set R (the input batch plus every node a host import returned
during the call; the host records them while serving, already fuel-charged) through a read mask M
of read classes allowed by the plugin's permissions and the imports it used. Let `π_M(S0_τ|R)` be
the projection of S0 at time τ onto those nodes and classes. A fact of that call targeting anchor a
is **valid at τ** iff `π_M(S0_τ|R) = π_M(S0_t|R)` and `BindGen(a, τ) = BindGen(a, t)`. "Stale on
arrival" and FS-5 ("never outlives its basis") are this predicate at admission and at every later
fuse. Exact = no false accept (stale accepted) and, as far as possible, no false reject.

Read classes [H] (one per independently permission-gated projection): `struct` (existence, parent,
children list), `core` (role, states, counts, actions), `names` (`read-names`), `text`
(`read-text`), `attrs` (`read-attributes`), `geom` (bounds, showing/visible).

### 3.2 Scheme

- Writer clock `T` (u64), strictly increasing per turn.
- Per node `stamp[n][c]`, c ∈ 6 classes: the T of the last turn in which `π_c(S0(n))` changed.
  Set when S0 recompute reports a changed group mask (`changed_vs` in `strata.rs` already computes
  it): O(1) per changed class.
- Token: `(t, R, M, g)`; R as a sorted `Arc<[NodeIdx]>` shared by all facts of one call
  (|R| ≤ 64 batch + imports; cap at 256 [H], beyond it fall back to a scope-wide stamp, which only
  adds false rejects).
- Stale ⇔ `∃ n ∈ R, c ∈ M: stamp[n][c] > t` ∨ `BindGen(a, now) ≠ g`.

**Theorem S (no false accepts) [P].** If the projection changed after t, then some change event on
some (n, c) with n ∈ R, c ∈ M happened at a turn > t, which set `stamp[n][c] > t`; stamps only
increase. ∎ False rejects arise only when a value changes and returns (ABA) in the interval.

**Equivalence with per-node revision vectors [P].** `stamp[n] > t ⇔ rev_now[n] > rev_t[n]` because
both bump on the same events and T is strictly increasing; checked exhaustively over all 3^6
histories of 3 nodes. So one u64 per token replaces a vector of |R| revisions: the token stores no
per-node numbers at all, only R itself, which the reverse index needs anyway.

**Minimality [P].** Within schemes that keep counters updated by change events (no stored values):
(i) every n ∈ R must be checked, else a change in an unchecked node is a false accept; (ii) any two
classes merged into one stamp give a false reject for a reader whose M contains one but not the
other, on a change of the other. Hence per-(node, class) stamps with the class partition equal to
the permission-distinguishable projections is the smallest exact revision set; finer partitions
(per group) only help plugins whose actual reads are narrower than their permission mask, which the
`view-node` batch does not track. The only way below the ABA residue is storing values: a 64-bit
hash of `π_M(S0_t|R)` gives zero false rejects and false-accept probability 2^-64 per check, but the
check then costs O(bytes read), not O(|R|).

### 3.3 Measured on random histories (script MQ-2, 3000 trials, 24 nodes, small value alphabet so ABA is frequent)

| Scheme | False accepts (of 1712 stale) | False rejects (of 1288 fresh) |
|---|---|---|
| per-(node, class) stamps (recommended) | **0** | 31 (ABA only) |
| one stamp per node | 0 | 573 |
| `vis_rev` (ADR-0024 basis for Inferred) | **291** | 378 |
| `sem_rev` (ADR-0026 §2.6 token mapping) | 0 | **1288 (all)** |
| 64-bit value hash | 0 (p = 2^-64) | 0 |

`sem_rev` is wrong for this purpose, not merely coarse: it bumps when the *published* entry changes,
and the plugin's own fact (and every other plugin's fact in the same turn) changes the published
entry of its target, which is in R. Every fact invalidates itself on admission. `vis_rev` misses
role, state, text and attribute changes. The ADR's `(sem_rev, vis_rev, BindGen)` mapping must be
replaced by `(t, R, M, g)` (PF-7).

### 3.4 Costs

- Update: O(1) per changed (node, class) in S0 recompute; memory 6 × u32 (turn stamps relative to
  an epoch base, rebased on wrap) = 24 B per node, 2.4 MB per 100k nodes; or only for nodes present
  in some live R (sparse map) when memory matters.
- Check on admission: O(|R|·|M|) ≤ 6·|R|, i.e. O(|R|). In a synchronous same-turn admission the
  check is trivially fresh (nothing ran between call and admission) and can be skipped; it matters
  for deferred admission (C-5) and for the lifetime of the fact.
- Lifetime (FS-5) without rescanning: a reverse index `node → live tokens` (built in O(|R|) per
  call, the same index PF-4 needs to retrigger `derive`). On a stamp bump of (n, c), mark tokens with
  c ∈ M stale and dirty their targets: O(tokens reading n). `FUSE` then reads one stale bit per
  register, O(1), and the plugin is re-derived by PF-4 in the same turn.
- Private state is outside any basis: a fact that depends on what the plugin remembered about a node
  not in R cannot be validated by any revision set. That is the second reason for PF-1.

## 4. MQ-3: splitting the 0.3 ms interactive budget

### 4.1 Model

Per focus change, n plugins have watched nodes in the focus chain (n ≤ n_max [H] = 16). Budget
B = 300 µs wall, per-call cap 200 µs (B-2). Plugin i costs `c_i + w_i`: `c_i` = host→guest entry
plus marshalling, **not fuel-charged**, `c_i ≤ c_max = 2 µs` [M, spike]; `w_i` = guest code plus
host-import work, charged in fuel at rate κ units/µs. Calibrated rate κ_cal, slowest real rate
κ_min, ρ = κ_cal/κ_min ≥ 1.

### 4.2 Policy: progressive filling (recommended)

1. Reserve `n·c_max`; fuel pool `P = κ_cal·(B − n·c_max)`.
2. Order the n plugins by predicted fuel ascending (EWMA p95 of their interactive calls; ties by
   plugin id).
3. For k = 1..n: grant `a_k = min(cap, R_k/(n − k + 1))` where R_k is the remaining pool; call; the
   unused part returns to the pool.
4. OutOfFuel: effects dropped (fail-open), re-derive on the background lane (LB-4 refine). A strike
   only if the plugin trapped at the per-call cap, or its p95 exceeded the floor `P/n` in 3
   consecutive focus changes (then it is demoted to background-only). Trapping below the floor
   because of crowding is not misbehaviour.

**Proposition B [P].** (i) `Σ used ≤ P` (each grant ≤ remaining). (ii) Every plugin gets
`a_k ≥ min(cap, P/n)`: by induction `R_k/(n−k+1) ≥ P/n`, since `R_{k+1} ≥ R_k − R_k/(n−k+1)`.
(iii) When the predicted order matches the true order of demands, the allocation equals max-min fair
water-filling (each plugin gets `min(d_i, level)`). (iv) Worst-case added delay

  `D ≤ n·c_max + P/κ_min + δ = n·c_max + ρ·(B − n·c_max) + δ`,

with δ the fuel overshoot: ≈ 0 for guest code (wasmtime checks at loop headers and function entries;
the spike measured overshoot ≈ 0), and **zero for imports only if they pre-charge** (check the cost
estimate before doing the work and trap without doing it); a post-charged import overshoots by its
own full cost (one `subtree` over a large tree). The 64th-import watchdog does not tighten D: a pure
compute loop calls no import. ∎

### 4.3 Numbers (script MQ-3; 20 000 focus changes per n; demand lognormal median 8 µs, σ 1.2, plus 5 % heavy U[100, 400] µs; prediction = demand × lognormal(0, 0.3))

| n | Policy | Completed | Cheap plugins starved | Max-min gap p99 (µs) | Wall p99 / max (µs, ρ = 1) | Floor holds |
|---|---|---|---|---|---|---|
| 4 | equal split B/n | 91.8 % | 0 % | 127.0 | 190.9 / 281.5 | — |
| 4 | fixed-order greedy | 95.6 % | 0.5 % | 60.3 | 296.7 / 299.3 | — |
| 4 | **progressive** | **96.0 %** | **0 %** | **0.0** | 296.7 / 299.1 | yes |
| 10 | equal split | 81.0 % | 0 % | 172.0 | 206.9 / 258.3 | — |
| 10 | fixed-order greedy | 85.0 % | 10.3 % | 141.5 | 293.7 / 296.4 | — |
| 10 | **progressive** | **93.5 %** | **0 %** | 3.3 | 293.7 / 296.4 | yes |
| 16 | equal split | 69.4 % | 0 % | 140.6 | 223.3 / 261.5 | — |
| 16 | fixed-order greedy | 67.3 % | 28.1 % | 171.5 | 289.3 / 292.0 | — |
| 16 | **progressive** | **87.7 %** | **0 %** | 7.2 | 289.3 / 292.6 | yes |

"Cheap" = demand ≤ min(cap, P/n), i.e. a plugin that fits its fair share. Fixed-order greedy (the
current ADR text: "remaining plugins skipped", calls ordered by plugin id) starves a cheap plugin
whenever an earlier-id plugin is expensive; equal split wastes the share of cheap plugins.

### 4.4 Why not carry-over or DRR

The budget is a per-event latency bound, not a throughput share. Carrying unused budget into the next
focus change breaks the per-focus bound (a plugin could spend 2B after a quiet change). Deficit round
robin equalises service over time; here the per-turn floor (ii) already gives every plugin its fair
share in every turn, and the ascending order makes the surplus go where it completes the most
plugins. Priority carry-over (who goes first) is unnecessary for the same reason. Strict per-plugin
cap `B/n` is the equal-split row: bounded but wasteful.

Determinism: which plugin completes depends on predictions, so the *first utterance* can differ
between runs; the *final* model cannot (fail-open plus background refine converges to `Full(S0)` by
Theorem P).

### 4.5 The bound is only as good as κ_min

| ρ = κ_cal/κ_min | D, n = 4 | D, n = 10 | D, n = 16 |
|---|---|---|---|
| 1 | 300 µs | 300 µs | 300 µs |
| 1.5 | 446 µs | 440 µs | 434 µs |
| 3 | 884 µs | 860 µs | 836 µs |
| 10 | 2 928 µs | 2 820 µs | 2 712 µs |

Fuel counts instructions, not time. κ_cal comes from one spin loop. A memory-bound guest (pointer
chasing over a 16–64 MiB instance memory, L2/L3 misses) executes far fewer instructions per µs, so
ρ can plausibly reach 10 or more [A, unmeasured]. With fuel only (C-2) and no imports in the loop,
nothing else catches it. Options, in order of preference [H]:

1. Calibrate the interactive-lane fuel rate on the slowest shape WQ-2 finds (add a pointer-chase
   shape over the instance's maximum memory), i.e. κ_cal := κ_p01. Costs throughput for normal code
   (they get fewer instructions), keeps ρ ≈ 1.
2. Limit interactive-lane instance memory (e.g. 2 MiB working set) so misses stay in L2/L3, and
   calibrate on that.
3. Measure wall time after every interactive call; strike when wall > 2 × grant. Detects, does not
   bound.

**Fix the unit first:** 3 × 10^6 units / 138 µs = **21 739 units/µs** (≈ 22 units/ns), not 22
units/µs as in ADR-0026 §2.5.1/§2.5.2 and `docs/reports/wasm-plugin-cost-spike.md` §3. B-2 at 0.3 ms
is ≈ 6.5 M units at the spike's rate.

## 5. Porting notes (Rust)

- `stamp: Vec<[u32; 6]>` indexed by `NodeIdx` slot, written in the S0 recompute path where
  `changed_vs` produces the group mask; a static `GROUP_TO_CLASS: [u8; 32]` table maps groups to
  classes. Stamps are writer-turn numbers relative to an epoch base; rebase all on wrap.
- `SnapshotToken` (`gorca-model::fact`) resolves in the host to `TokenRec { t: u64, reads:
  Arc<[NodeIdx]>, mask: u8, gen: Option<BindGen> }`; `side.rs` currently rejects `Basis::Snapshot`
  at insert and treats it as always stale (`side.rs:385`, `side.rs:767`): replace with the §3.2 test.
- Reverse index `HashMap<NodeIdx, SmallVec<[TokenId; 4]>>`, the same structure for PF-4 retrigger and
  §3.4 lifetime staleness.
- S1 key `(tier, specificity, plugin_id, anchor_key)` as a tuple `Ord`; no value or seq in the key.
- S3: add a per-(node, group) "winner tainted" bit and read the child's clean value through it.
- Budget: `fn grant(remaining: u64, left: usize, cap: u64) -> u64 { cap.min(remaining / left as u64) }`,
  plugins sorted once per focus change by a per-plugin EWMA kept in the slot table.

## 6. Open / unverified

- ρ (fuel-to-time spread across guest shapes and P/E cores) is unmeasured; the 0.3 ms bound rests on
  it (WQ-2).
- Admission cost per fact and the reverse-index cost are not measured; assumed small next to B.
- The read-class partition (6 classes) is a proposal; it must match the final `read-*` permissions.
- PF-1 is enforced by differential testing, not by the runtime; a T2 plugin that cheats breaks
  order independence only for its own S1 groups.
- The model is a 7-node tree with one group; it covers every stratum and the S3 edge but not
  relations (fan-out cap) or positions, which use the same S3 rule and should behave alike.
