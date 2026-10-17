# Add-on platform math: composition, latency budgets, versioning, store

Date: 2026-10-17. Author role: math-researcher. Status: input to ADR-0015 (extension API stability),
M18 (SDK preview) and the Phase 4 store. Companion work: architect (platform design) and reviewer
(security model) run in parallel; this report covers only the formal and algorithmic side.

Scripts (project venv, `. ./env.sh`; each runs its own self-tests and prints the numbers quoted here):

| Script | Covers | Runtime |
|---|---|---|
| `tools/research/addon_math/compose.py` | ordering, cycles, conflict detection, filter-chain bounds, veto ordering | 0.4 s |
| `tools/research/addon_math/budget.py` | tail composition, union-bound allocation, carry-forward pool, storm queue | 2 s |
| `tools/research/addon_math/resolve.py` | MVS, 3-SAT hardness, Component Model semver window | 0.7 s |
| `tools/research/addon_math/store.py` | Bayesian/Wilson ratings, MinHash on the real `.wasm` components, rollout SPRT | 22 s |

Legend used below: **[P]** proven (argument given, and checked numerically), **[M]** measured in this
repo, **[A]** assumption, **[H]** heuristic or recommended parameter.

---

## 0. Findings about the current host (read from code, 2026-10-17)

1. **Load order is the order of execution today.** `order_matching` (`crates/gorca-ext-host/src/manifest.rs`)
   sorts by `Specificity` (app > toolkit > connector) and keeps input order for ties; the input order is
   `paths.sort()` over the extension directory (`host.rs` ~l.140). So between two add-ons of the same
   specificity, the **directory name** decides who wins `present`/`policy`/`on-event`. That is deterministic
   but arbitrary, and it changes when a user renames a folder or when M18 adds a user directory next to
   the system one.
2. **`on-event` is first-non-pass-wins, not a left-to-right composition.** The WIT comment says verdicts
   "compose left to right"; `AppExts::on_event` breaks on the first non-`Pass` and `ExtVerdict::then` is
   left-biased (`crates/gorca-core/src/ext.rs:53`). Algebraically all three arbitration hooks
   (`on-event`, `present`, `policy`) are the same left-zero monoid (§1.2 K3). The WIT doc should say so.
3. **Extension-vs-extension chord collisions are silent.** `commands/compile.rs` (~l.280) binds an
   extension default chord only if no preset, user or *earlier extension* mentioned it, and logs a
   `tracing::warn`. "Earlier" is again directory order; the user is never told.
4. **`ReEvaluate` cannot loop.** It only sets `resync_needed` (`app_actor.rs:1078`); rewrites go to
   speech, never back into the event stream. Termination is structural today (§1.4).
5. **Vendor profile ties** are planned to break by "file name order, with one diagnostic"
   (`docs/plan/phase-3-features.md` PM-1): same issue as 1.
6. Limits in force: `MAX_EXTENSIONS = 64`, `POOL_INSTANCES = 512`, fuel 3M/ms, `present`/`policy`
   2 ms CPU with 3x CPU backstop, wall ceiling 50x. X1 hook p99 0.2–0.5 µs [M].

---

## 1. Composition semantics

### 1.1 Model

A **hook point** `H` is a place where the core asks every matching add-on for an answer of type `T_H`.
Each add-on `i` contributes `h_i : Input → T_H`. A **combinator** `⊕_H : T_H × T_H → T_H` and an
**order** `σ_H` (a permutation of the participating add-ons) define the result
`R = h_σ(1)(x) ⊕ … ⊕ h_σ(n)(x)`. Requirements for every hook point:

- **D (determinism):** `R` depends only on the *set* of installed add-ons (manifests + the user's
  explicit choices), never on load order, file names, hash-map iteration or timing.
- **T (termination):** each `h_i` runs at most once per input per hook point; no output re-enters the
  same hook point in the same causal chain beyond a fixed depth.
- **W (bounded work):** total CPU and output size per input are bounded by a constant independent of
  what add-ons do.

If `⊕` is commutative and associative, `σ` is irrelevant and D holds for free. Otherwise `σ` must be a
function of the set, and *every place where the order changes the answer is a potential conflict that
must be reported*.

### 1.2 Hook taxonomy (each hook point must be declared as exactly one class)

| Class | Structure of `⊕` | Order matters? | Conflict = | Hook points (now / planned) |
|---|---|---|---|---|
| **K1 Collect** | set union over namespaced keys: commutative, associative, idempotent (join-semilattice) | no | two add-ons using the same *global* key (impossible if keys are `ext.<id>.*`) | `nav-units`, elements-list categories (M15), `Mode::Chooser` sources, command registry, message catalogs, sound-icon themes |
| **K2 Exclusive assignment** | partial function resource → owner, precedence lattice | only for ties | two claims on one resource that can be live at once (§1.5) | gestures/chords, quick-nav letters, the terminal policy of an app, "the" document handler |
| **K3 Arbitration (first-wins)** | `a ⊕ b = a if a ≠ none else b`: associative, **not** commutative, left-zero monoid | **yes** | two add-ons at the same rank both answer for the same input (dynamic) | `present`, `policy`, `on-event` verdict, `command` dispatch |
| **K4 Ordered transformation** | function composition: associative, not commutative | **yes** | an unordered pair that does not commute on the corpus | speech text filters, segment rewriters, extension dictionaries (ADR-0017), braille rewriters |
| **K5 Veto / decider** | boolean AND (or OR): commutative, idempotent | no (only cost) | none (semantics are order-free) | "should this be spoken", "should gorca handle this key", NVDA `Decider` / `AccumulatingDecider` |
| **K6 Observer** | none (unit result) | no | none | "focus moved", "utterance spoken", NVDA `Action`; run off the hot path |
| **K7 Data overlay** | per-key override by a specificity lattice | only for ties | two layers of equal specificity setting one key | vendor profiles (M14a L4), extension setting defaults |

Design rule **C-0**: a new hook point is not accepted into the WIT unless its class is declared. K1, K5,
K6 are preferred because they are order-free; K3/K4 are allowed but pay the ordering and conflict
machinery below.

NVDA comparison (`refs/nvda/source/extensionPoints/__init__.py`, `util.py`): `Filter` = K4, `Decider` =
K5 (AND, short-circuit), `AccumulatingDecider` = K5 (OR), `Action` = K6, `Chain` = K1. NVDA orders every
one of them by **registration order** ("maintained in the order they were registered so that they can be
called in a deterministic order across runs", `util.py:93`) and offers `moveToEnd` so add-ons can fight
for position. Registration order = add-on import order: load-order dependence is the NVDA design, and
the source of its "add-on A breaks only when B is installed" reports. gorca should not copy it.

### 1.3 Ordering for K3 and K4 hook points

**Declaration.** Per hook point an add-on's manifest may give `phase` (from a small core-owned total
order, e.g. text filters: `normalize < expand < annotate < finalize`; arbitration: `app < toolkit <
connector` = today's `Specificity`) and soft relative constraints `before = [ids]`, `after = [ids]`
(ignored when the other add-on is absent). **No numeric priorities for third parties** [H]: a numeric
priority is a game where the dominant strategy is the maximum value, so after a year every add-on
declares `priority = 1000` and the tie-break decides anyway.

**Order.** Build the DAG `G` with an edge `a → b` for each constraint inside a phase; edges that point
from a later phase to an earlier one are rejected (publish error, dropped at load). `σ_H` = the
**lexicographically least topological order** of `G` under the key `(phase, ext_id)`.

- **[P] Determinism.** Kahn's algorithm with a min-heap on the key outputs, at each step, the least
  available vertex; the available set at each step is a function of the DAG and the already-emitted
  prefix only, so the output is the unique lexicographically least linear extension: a function of the
  declaration *set*. Checked: 2000 random hook points × 5 shuffles, identical orders, cycles and
  rejections (`compose.py test_load_order_independence`).
- **Complexity:** `O((V + E) log V)`, `V ≤ 64` today; computed once per (hook point, app) when slots are
  built, never per event.
- **Cycles.** Tarjan SCC. All constraint edges *inside* a non-trivial SCC are dropped (symmetric: nobody
  "wins" the cycle), its members fall back to the key order, and the SCC is reported once. 877 of the
  2000 random instances had cycles; all were reported and the remaining edges were respected.
- **User override.** The user may pin an explicit order for a hook point (settings app). It enters as
  additional edges of a higher rank: declared edges that contradict a user edge are dropped before the
  sort, so the user always wins and the result stays deterministic.
- `ext_id` must be a stable global id (`publisher.name`), not a directory name, so the fallback is
  the same on every machine.

Test vector (`compose.py`):

```
decls: org.b.emoji  expand   after={org.a.abbr}
       org.a.abbr   expand
       org.c.numbers normalize
       org.d.cyc1   annotate before={org.e.cyc2}
       org.e.cyc2   annotate before={org.d.cyc1}
       org.f.bad    finalize before={org.c.numbers}
order:    [org.c.numbers, org.a.abbr, org.b.emoji, org.d.cyc1, org.e.cyc2, org.f.bad]
cycles:   [[org.d.cyc1, org.e.cyc2]]
rejected: [(org.f.bad, org.c.numbers, "contradicts phase order")]
```

**K5 cost order [P].** For a short-circuit AND of tests with cost `c_i` and pass probability `p_i`,
expected cost `Σ c_i Π_{j<i} p_j` is minimised by sorting on `c_i / (1 − p_i)` (adjacent-exchange
argument: swapping `i, i+1` helps iff `c_{i+1}(1 − p_i) < c_i(1 − p_{i+1})`). Verified against brute force
on 300 random instances. The host can learn `c_i, p_i` online; the result does not change, only the cost.

### 1.4 Termination and bounded work

**[P] T for the current hook set.** Each hook point calls each slot at most once per input
(`for slot in &self.slots`, no retry loop); `ReEvaluate` sets a flag; rewrites and `speak` produce
speech, which is never an input of a hook. Hence the call graph per input is a DAG of depth 1.

**Required once hooks can act on the application** (M18+: "activate", "set caret", "set text", web
`describe` with side effects). An action can make the application emit events that reach the same add-on
again (ext sets text → `text-changed` → ext rewrites → sets text …). Rule **T-1**: every host action is
tagged with `(origin ext, depth)`; events the connector attributes to that action window (same app, within
a settle window) carry `depth + 1`; the host never dispatches an event of depth `≥ D_max = 2` to K3/K4
hooks of the originating add-on, and every action draws from the add-on's token bucket (§2.4). Since depth
strictly increases along any causal chain and is capped, every chain terminates; the bucket bounds the
rate even if attribution misses (attribution is heuristic [H]; the bucket is the guarantee).

**[P] W for K4 chains.** Each filter runs exactly once (no fixpoint iteration: "rewrite until stable" is
forbidden because its termination is undecidable for arbitrary filters). Every filter output is capped
relative to the chain's **original** input `n0` (a per-stage cap compounds geometrically):
`m = min(2·n0 + 1024, 16 Ki chars)`; an over-cap output is discarded (identity) and counted. Work over
`k` filters is at most `n0 + m + 2(k − 1)m` characters: 47,872 chars for `k = 16`, 195,328 for `k = 64`
from `n0 = 256` (checked with an adversarial "×1000" filter, `compose.py test_chain`). CPU per call is
already bounded by fuel and the host-work meter.

**Idempotence (K4) [H].** Require `f(f(x)) = f(x)` for text filters (checked by the store on a corpus,
§3.5): the same text can be presented twice (re-read, history, braille + speech) and a non-idempotent
filter ("add 'link' before links") then doubles. The host marks filtered segments so a chain never runs
twice on the same segment.

### 1.5 Conflict detection and reporting

**K2 exclusive resources (static, at install and load).** Two claims `(ext, resource, contexts, app
globs)` *conflict* iff same resource, overlapping keymap contexts, and the app-glob languages intersect.
Glob intersection emptiness is decidable exactly in `O(|p|·|q|)` by a search over the product automaton
(`compose.py glob_intersects`; exact against brute force on 3000 random pairs). Example worth knowing:
`*fox` and `gnome-terminal*` *do* intersect (`gnome-terminalfox`), so suffix globs collide more often than
authors expect; the store should warn on leading `*` in app matchers.

Resolution: precedence lattice **user > core preset > extension** (already in `commands/compile.rs`);
among extensions **incumbent wins**, where incumbency is the position in a persisted per-user *install
ledger*, not load order. Properties [P, checked in `test_assign`]:
- permutation invariant (all 120 load orders of the test set give the same grants);
- **stable**: installing an add-on never changes an existing grant (the newcomer loses and is told);
- removal frees the resource for the next claimant in ledger order.

The loser is reported once, spoken at install ("Add-on X: gorca+F9 is already used by Y; X's command is
in the command list, unbound") and listed in the settings app, with a one-step rebind. Never silent.

**K1 collect.** Keys are namespaced, so no conflicts; *display order* of contributed categories in the
elements list is core categories first, then by catalog-resolved display name in the UI language
(collation), never by load order. Two categories with the same display name are disambiguated with the
add-on name.

**K3 arbitration (dynamic).** The host counts, per (hook point, app, unordered pair of add-ons at the same
phase), how often *both* would have answered. It cannot know without calling the later one, so it samples:
with probability `q = 1/64` it calls the next slot after the winner too (off the Reader, on a worker, with
the same input snapshot) and records the overlap. Cost is `q` times one extra call; a pair with overlap
rate `≥ 1 %` over `≥ 100` samples is reported as "X and Y both handle focus presentation in Firefox; X is
used" with a user-order override. Pairs in different phases are not conflicts (the phase is the intent).

**K4 order-sensitive pairs.** At install, for each pair of filters on the same hook point with no
declared constraint between them, run both orders on the store corpus (§3.5); if `f∘g ≠ g∘f` on any corpus
item, warn and show the chosen order. Cost: `O(k²·|corpus|)` filter calls, install time only.

### 1.6 Changes this implies for the current code

1. Replace `paths.sort()` tie order by the `(phase, ext_id)` least topological order; ext_id from the
   manifest.
2. Fix the WIT doc of `on-event` ("first non-pass wins") or implement real composition (K4 for
   `rewrite`) — the first is consistent with `present`/`policy`.
3. Extension chord collisions: install ledger + user-visible conflict report.
4. Vendor-profile ties (PM-1): `(specificity, ext_id)` instead of file name.
5. Before any action-capable hook: causal depth tags (T-1).

---

## 2. Latency budget allocation

### 2.1 Budget

Product goal: keypress → speech start p99 `B = 15 ms` gorca-side. Measured core paths: A1 (Shell focus)
p99 5.5 ms, N1 0.5 ms [M]. Recommended **extension share on one keypress→speech path: `E_hot = 3 ms` CPU
total** across every hook that runs on it (`on-event` of the triggering event, `policy`, `present`,
K4 filters) [H]. Rationale: `15 − 5.5 = 9.5 ms` headroom; scheduling noise on a loaded desktop and the
speech client need the larger part; 3 ms keeps ≥ 50 % of the headroom. Today, `n` add-ons in sequence
may each burn 2 ms (6 ms with the backstop), so the worst case is `2n` ms: the limit is per call, not per
path, and **5 matching add-ons already break the goal in the worst case**. The per-path pool below fixes
that independently of `n`.

### 2.2 Tail composition

For `n` hooks in sequence with costs `X_i`, the path cost is `S = Σ X_i`.

- **[P] Union-bound allocation.** `P(S > Σ q_i) ≤ Σ P(X_i > q_i)`. If each hook's budget is its
  `(1 − α/n)` quantile, `Σ q_i` is a valid `(1 − α)` bound for *any* dependence between hooks (slow
  machine ⇒ all slow). Check, `n = 8`, `α = 0.01`, comonotone worst case: violation rate 0.0011 ≤ 0.01.
- Summing p99s is *not* a valid p99 bound in general (quantiles are not subadditive), but it is
  conservative for independent light tails: measured `n = 64` X1-like hooks (p50 0.25 µs, p99 0.5 µs):
  sequential p99 18.3 µs vs `Σ p99 = 32 µs`.
- **Parallel fan-out does not pay** [A: 10 µs cross-thread wake]. Parallel p99 = `max + handoff ≈ 10.7 µs`
  for any `n ≤ 64`; it beats sequential only beyond ~35 trivial hooks, saving < 10 µs, and a single heavy
  hook (p99 400 µs) dominates both (sequential p99 403.8 µs with 15 light ones). First-wins K3 also needs
  the *first* answer in order, so parallel calls waste the others. **Recommendation: sequential, inline,
  never fan out on the Reader.**

### 2.3 Per-path pool with carry-forward (the hot path)

For a path with `n` participating slots in order `σ`: guaranteed share `g_i = E_hot / n`; call `i` is
offered fuel `o_i = g_i + ℓ_{i−1}` where `ℓ_{i−1}` is what earlier calls left unused (`ℓ_0 = 0`),
capped by the hook's own limit (2 ms).

- **[P]** `ℓ_i = o_i − u_i ≥ 0`, so `Σ u_i = Σ g_i − ℓ_n ≤ E_hot` (telescoping) and `o_i ≥ g_i`: total
  bounded for any `n`, every add-on guaranteed its share, unused share flows to later add-ons
  (work-conserving). Checked: 200k random paths, `n = 12`, `E_hot = 3 ms`, max total 3000.0 µs, min offer
  250 µs.
- **Admission, not cutting.** A fuel trap mid-call forces an instance reset (9–31 µs [M], ADR-0008) and
  loses the add-on's state. So the host keeps a per-(ext, hook, app) cost estimate `ĉ` (max of the last 32
  fuel readings, or a P² p99 sketch) and **skips** the call when `ĉ > o_i` ("no opinion", counted as a
  *budget skip*, not a strike). Exceeding the add-on's own 2 ms limit stays a fault and a strike as today.
- **Degrade ladder [H]:** (1) budget skip; (2) ≥ 20 budget skips in 60 s on one hook point of one app →
  the hook is *demoted* for that app (skipped until the next app start; the user is told once,
  diagnostics show the cost profile); (3) faults → reset → three strikes → disabled (existing).
  Demotion to "async late override" (speak core output now, replace when the add-on answers) is rejected:
  double speech is worse than the default presentation.
- Typical case costs nothing: 64 X1-like add-ons use ~18 µs of the 3 ms pool.

### 2.4 Off the keypress path: event storms in the app actor

`on-event` runs in the app actor. With coalesced arrival rate `λ` and per-event service
`S = c_core + Σ_i c_i`, utilisation `ρ = λ·E[S]`; mean wait (Pollaczek–Khinchine, M/G/1)
`W = λ E[S²] / (2(1 − ρ))` (checked: analytic 3.07 µs vs simulated 3.06 µs). Keeping `ρ ≤ 0.5` gives the
per-event extension allowance `Σ c_i ≤ 0.5·10⁶/λ − c_core` µs: 485 µs at 1k ev/s, **35 µs at 10k ev/s**,
10 µs at 20k ev/s (`c_core = 15 µs` [A]).

Storm simulation at 10k ev/s, FIFO (worst case: no priority lane for focus), waits p50/p99/p99.9:

| Extension load | ρ | wait p99 | wait p99.9 |
|---|---|---|---|
| 16 light add-ons (0.3 µs) | 0.20 | 43 µs | 74 µs |
| + one 60 µs `on-event`, unlimited | 0.80 | 861 µs | 1.2 ms |
| + same, CPU token bucket 50 ms/s, burst 10 ms | 0.25 | 76 µs | 112 µs |
| + one 90 µs `on-event`, unlimited | 1.10 | **3.0 s** (unstable) | 3.0 s |
| + same, token bucket | 0.25 | 100 µs | 138 µs |

One add-on at 90 µs per event — far below its 2 ms per-call limit — makes the actor unstable. Per-call
limits cannot prevent this; a **rate** limit can.

**Recommendation [H]:** a CPU-time token bucket per add-on (global across apps): rate `r = 50 ms/s`
(5 % of one core), burst `b = 10 ms`; when empty, `on-event` calls are skipped with the existing Busy
semantics (repeat last decision for high-rate kinds, pass otherwise). With `N` active add-ons and an
extension CPU pool `R_ext` (suggest 1 core), shares are weighted **max-min fair** (water-filling):
`share_i = min(d_i, λ*)` with `λ*` solving `Σ min(d_i, λ*) = R_ext`; with `r = 50 ms/s` the static rate
already satisfies `N·r ≤ R_ext` for `N ≤ 20`, beyond that `r` becomes `R_ext/N` with borrowing.
This also delivers the fairness invariant of 2026-10-16 (one flooding source cannot starve others) for
the add-on dimension.

### 2.5 Commands and background work

The command worker (20 ms limit) should serve extensions by **deficit round robin** with quantum
`Q = 20 ms` CPU: `O(1)` per decision, and a backlogged add-on waits at most `(N − 1)·Q` behind the others
(classic DRR bound), so one add-on that spams commands cannot delay another's by more than one quantum
per competitor.

### 2.6 Parameters

| Parameter | Value | Kind |
|---|---|---|
| `E_hot` per keypress→speech path | 3 ms CPU | [H] |
| per-call limits | keep 2 / 2 / 16 / 20 ms | existing |
| guaranteed share | `E_hot / n` with carry-forward | [P] |
| cost estimate for admission | max of last 32 fuel readings per (ext, hook, app) | [H] |
| demotion | 20 budget skips / 60 s per hook point per app | [H] |
| per-add-on CPU bucket | 50 ms/s, burst 10 ms, global per add-on | [H] |
| extension pool `R_ext` | 1 core, max-min fair | [H] |
| storm allowance | `Σ c_ext ≤ 0.5·10⁶/λ − c_core` µs per event | [P] given M/G/1 |

---

## 3. Versioning and compatibility

### 3.1 What is versioned

(a) WIT packages (`gorca:ext@x.y.z`, later `gorca:web@…`); (b) the add-on (semver); (c) the manifest
schema; (d) data formats (catalogs, profiles, dictionaries). (a) is the one that broke NVDA yearly
(`refs/nvda/source/addonAPIVersion.py`: `BACK_COMPAT_TO = (2027, 1, 0)`, `isAddonCompatible` = has
`minimumNVDAVersion ≤ CURRENT` and `lastTestedNVDAVersion ≥ BACK_COMPAT_TO`): NVDA's API surface is its
whole Python internals, so compatibility is undecidable and NVDA substitutes a manually declared "last
tested" field and a yearly cut. gorca's surface is a typed WIT world, so **compatibility is mechanically
decidable** by a link check, and no "last tested" field is needed.

### 3.2 Compatibility is subtyping with a direction

- **Imports (host → guest functions):** the host may *add* functions and interfaces freely; old guests
  ignore them. [P] by Component Model linking: a guest links iff every import it names is provided with
  the same type.
- **Exports (guest → host hooks):** the host calls them. Adding a function to an exported interface the
  host requires **breaks every existing add-on**. Today the world exports `hooks` (six functions) and
  `terminal` unconditionally, so each new hook is a breaking change. Rule **V-1:** one exported interface
  per hook group (`event`, `present`, `policy`, `nav`, `command`, `terminal`, `web`), each optional; the
  host inspects the component's export list at load (wasmtime `Component::component_type()`, to verify)
  and treats a missing interface as "hook not implemented" (the manifest `HookSet` already models this).
  New hooks = new interfaces, never new functions in old ones.
- **Records, variants and enums are not extensible in WIT.** Adding a field to `node`, a case to `event`
  or a value to `role` changes the type and breaks linking in both directions. The `node` record already
  says "TBD: text value, attrs, relations, bounds"; `role` will grow with every ARIA role. Rule **V-2:**
  before 1.0, turn growing data into **resources with getter methods** (`resource node { role: func() ->
  u32; name: func() -> option<string>; … }`; new getters are import-side additions, safe), represent open
  enumerations (`role`, `event` kinds) as `u32` ids with a documented registry and an `unknown` fallback,
  and keep closed variants (`event-verdict`) only where the set is truly fixed.

### 3.3 The compatibility window (verified against wasmtime)

wasmtime matches semver-qualified names via `alternate_lookup_key`
(`refs/wasmtime/crates/environ/src/component/names.rs:293`): `1.x.y` tracks by major (`…@1`), `0.x.y`
with `x > 0` tracks by minor (`…@0.x`), `0.0.y` and pre-releases match only exactly; a guest importing
`@g` links against a host providing `@h` on the same track with `h ≥ g`. Reproduced in `resolve.py`.
Consequences:

- During the **0.x preview** (M18's "0.9"), every minor bump is a breaking track change unless the host
  keeps registering the previous minor in its linker. Recommend the host **links two tracks at once**:
  current and previous (`0.9` and `0.10`, later `1` and `2`), the previous implemented by a thin adapter
  over the current host. Policy **V-3 [H]:** a track stays supported for ≥ 12 months *and* ≥ 2 gorca
  minor releases after its successor ships, whichever is longer; removal is announced one release ahead
  with the store's count of add-ons still on it.
- **Release gate:** before each gorca release, link-check every published store add-on against the new
  host (`Linker::instantiate_pre`, no execution: milliseconds each). Metric "add-ons that would stop
  loading" must be 0 for anything still inside its window. This replaces NVDA's manual
  `lastTestedNVDAVersion` with a computed fact.

### 3.4 Dependency resolution

- **[P] Hardness.** With exact pins or upper bounds and one version per package, deciding whether a
  dependency set is resolvable is NP-complete (3-SAT: variable `x` → package with versions
  {false, true}; clause → package with 3 versions, version `k` pins literal `k`). The encoding is checked
  against brute force on 400 random formulas (`resolve.py`). This is why PubGrub/SAT resolvers exist.
- **Option A (recommended for v1): no add-on → add-on code dependencies.** Components are self-contained
  (shared code is composed at build time, `wasm-tools compose`/`wac`); the only dependency is the host WIT
  track, checked in `O(1)` per add-on by §3.3. Nothing to solve.
- **Option B (when "provider" add-ons appear, e.g. an OCR or translation provider): capability
  providers, not version graphs.** An add-on *requires* an interface (`gorca:ocr/recognize@1`); providers
  *export* it; the host binds each requirement to one installed provider on the matching semver track
  (user choice when several; K2 assignment). That is bipartite matching on interface names: polynomial,
  and conflicts are visible to the user.
- **Option C (only if real library dependencies are unavoidable): MVS** (Go modules): lower bounds only,
  each major is a different package. `O(V + E)`, deterministic, and **[P]** the componentwise least
  solution when requirements are monotone in the version (induction over the reachable set; checked
  against brute force on 3000 random instances). PubGrub (full ranges, CDCL, exponential worst case,
  good explanations) only if upper bounds become unavoidable; "incompatible with X" stays a **soft
  warning**, never a resolver constraint, so resolution stays polynomial.

### 3.5 What the store checks on publish

1. Manifest schema, size limits (`MAX_MANIFEST_BYTES`), stable `publisher.name` id, semver strictly
   greater than the previous release.
2. Component validates; imports ⊆ the host tracks in the window; exported interfaces match the declared
   `HookSet`; capabilities requested ⊆ capabilities whose host imports are actually used (flag
   over-asking).
3. Link check against every host release in the window (§3.3).
4. **Cost profile:** run each exported hook on a recorded corpus (Shell, terminal, Firefox/Chromium
   snapshots) under the real fuel meter; publish p50/p99 fuel per hook; reject any hook whose p99 exceeds
   50 % of its limit; show the profile on the store page (feeds §2.3 admission as a prior).
5. Determinism on the corpus: two runs, same outputs (WASI clocks/random are available, so this is a
   warning, not a reject).
6. K4 filters: idempotence on the corpus (§1.4); K2: chord/letter claims vs core defaults and vs popular
   add-ons (§1.5); K3: overlap with popular add-ons on the corpus.
7. Message catalogs: every referenced id present for the declared languages; caps (ADR-0010).
8. Near-duplicate scan (§4.2), signature, provenance (publisher key).

---

## 4. Store math (lighter)

### 4.1 Ratings and ranking

- Display stars as a **Bayesian average** `(C·m + Σ w_j r_j) / (C + Σ w_j)` with prior mean `m` = store
  mean, `C = 20` [H]. Three 5-star ratings on a new add-on score 4.13, not 5.00.
- Rank "recommended" lists by the **Wilson lower bound** (z = 1.96) of the up-vote share: 3/3 → 0.44,
  90/100 → 0.83, 900/1000 → 0.88.
- **Manipulation:** weight `w_j` by rater trust: only raters with a verified install of the add-on
  (signed install receipt from gorca, no telemetry needed) for ≥ 7 days count fully; new or unverified
  accounts weigh 0.05 [H]. A poor add-on (40 honest 2-star) plus 200 sock-puppet 5-stars: unweighted 4.46,
  trust-weighted 3.00 (`store.py`). One rating per account per add-on, and the weight of any single
  account is capped.

### 4.2 Near-duplicate and malicious clones

MinHash (256 permutations) over shingles, LSH banding `b × r` (50 % detection at `J ≈ (1/b)^{1/r}`:
32×8 → 0.65, 64×4 → 0.35), exact Jaccard on candidates. Measured on the two real components
(`ext_gnome_shell.wasm` 79,842 B, `ext_vte.wasm` 586,745 B) with 8-byte shingles:

| Pair (vs ext-gnome-shell) | Jaccard | MinHash est. | containment |
|---|---|---|---|
| clone, 0.5 % bytes mutated | 0.914 | 0.895 | 0.969 |
| clone, 5 % bytes mutated | 0.473 | 0.488 | 0.713 |
| clone + 50 % appended payload | 0.570 | 0.578 | 1.000 |
| unrelated add-on (ext-vte) | 0.105 | 0.105 | **0.760** |

Findings: the estimator is accurate (±0.02); **containment alone is useless** because unrelated add-ons
share the Rust std/`wit-bindgen`/SDK code (76 % of ext-gnome-shell's shingles occur in ext-vte); raw
bytes are fragile (5 % scattered mutations halve `J`). Production design [H]: features =
(per-function normalised opcode-sequence hashes via `wasmparser`, ignoring indices and constants) ∪ (WIT
import/export signature) ∪ (message-catalog strings and manifest text), minus a **background set**
(shingles present in SDK reference builds or in > 1 % of store add-ons, IDF-style). Clone alert = high
similarity to a popular add-on **and** a different publisher **and** a capability diff (the clone asks for
more): manual review queue, not auto-reject.

### 4.3 Staged rollout and automatic rollback

- **Bucketing:** install is in stage `s` iff `H(install_id ‖ addon_id) mod 10⁴ < 100·pct_s`. Monotone
  across stages (nobody flips back) and independent across add-ons (the same users are not always first).
- **Signal:** sessions in which the add-on was fault-disabled (three strikes) or caused a host fault,
  new version vs old version running concurrently.
- **Test [P]:** conditional Wald SPRT. Given a failure, it comes from the new arm with probability
  `π(r) = r·s / (r·s + 1 − s)` (`s` = stage share, `r` = failure-rate ratio) regardless of absolute
  exposure; test `H0: r = 1` vs `H1: r = 2` with `α = 0.01` (false rollback), `β = 0.05`,
  `LLR = n_new ln(π1/π0) + n_old ln((1−π1)/(1−π0))`, rollback at `LLR ≥ ln((1−β)/α)`, promote at
  `LLR ≤ ln(β/(1−α))`. Sequentially valid by construction (checked hourly).

| Scale | stage share | base rate | r = 1: P(rollback) | r = 2: P(rollback), median time | r = 3 |
|---|---|---|---|---|---|
| 20k sessions/h | 5 % | 0.2 % | 0.005 | 0.975, 6 h | 1.0, 3 h |
| 1k sessions/h | 10 % | 0.5 % | 0.003 | 0.963, 25 h | 1.0, 10 h |
| 200 sessions/h | 20 % | 0.2 % | 0.003 | 0.847, 143 h (15 % undecided after 2 weeks) | 1.0, 65 h |

  At community scale the test needs larger stages and days; add **absolute triggers** [H]: a fault
  signature never seen in the old version on ≥ 3 distinct installs → pause; budget-skip/demotion rate
  (§2.3) under the same SPRT for performance regressions. Stages: 5 % → 25 % → 100 % at large scale,
  20 % → 100 % for small add-ons. Opt-in, aggregate counts only.

---

## 5. Notes for porting to Rust

- Ordering: `BinaryHeap<Reverse<(u8 phase, Arc<str> id)>>` Kahn + iterative Tarjan (no recursion on
  untrusted input); computed in `AppExts` construction, stored as `Vec<SlotIdx>` per hook point.
- Glob intersection: iterate the `(i, j)` product with an explicit stack and a `Vec<bool>` visited of
  size `(|p|+1)(|q|+1)`; globs are short (manifest caps).
- Install ledger: `install_seq: u64` per add-on in the state dir (M11), written once at install.
- Carry-forward pool: one `u64` fuel counter in the Reader's per-utterance context; `set_fuel(min(limit,
  g + leftover))`, read `get_fuel` after the call.
- Token bucket: `AtomicU64` pair (tokens, last refill ns) per add-on, CPU time from
  `CLOCK_THREAD_CPUTIME_ID` already read by the backstop.
- Store side (Phase 4, any language): `wasmparser` for function bodies; MinHash with 64-bit
  multiply-shift hashing; SPRT is a few lines of `f64`.

## 6. Proven vs heuristic summary

| Claim | Status |
|---|---|
| least topological order is load-order independent; cycle handling symmetric | [P] + 2000×5 random checks |
| first-wins associative, not commutative; veto order only affects cost; `c/(1−p)` optimal | [P] + brute force |
| glob intersection exact in `O(|p||q|)` | [P] + brute force |
| incumbent assignment permutation-invariant and stable | [P] + permutation test |
| single-pass K4 chains terminate; work bound `n0 + m + 2(k−1)m` | [P] + adversarial test |
| union-bound path budget valid under any dependence | [P] + comonotone simulation |
| carry-forward pool: total ≤ E, each ≥ E/n | [P] + simulation |
| storm numbers, P–K | model [A: Poisson arrivals, `c_core` 15 µs, FIFO]; P–K matches simulation |
| general resolution NP-complete; MVS least under monotone requirements | [P] + brute force |
| wasmtime semver tracks | [M] read from source, reproduced |
| `E_hot` = 3 ms, buckets 50 ms/s, demotion thresholds, `C = 20`, trust 0.05, LSH bands | [H] |
| clone detection quality | [M] on 2 real components only; production features unvalidated |

## 7. Open questions

1. Does the app actor give focus events a priority lane over storm events (ADR-0004 drain)? If yes,
   §2.4's FIFO numbers are pessimistic for the keypress path; the instability at `ρ > 1` remains.
2. wasmtime export lookup: confirm that optional exported interfaces can be probed per interface with
   bindgen-generated hosts (V-1), or whether the host needs one bindgen world per hook group.
3. Cross-thread handoff cost (10 µs assumed) and `c_core` (15 µs assumed) should be measured by
   perf-engineer before fixing `E_hot`.
4. Causal attribution of app events to an add-on's action (T-1) is heuristic; the token bucket is the
   real guarantee.
5. Store telemetry for SPRT needs a privacy decision (opt-in, aggregate) from the user.
