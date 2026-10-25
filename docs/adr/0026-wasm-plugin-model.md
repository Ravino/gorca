# ADR-0026: WASM plugin model on the fused semantic model (M18a)

Status: **Proposed** (r5 implementation notes, 2026-10-25; r4, 2026-10-24; r3 2026-10-24; r2 2026-10-23; r1 2026-10-23). Owner: architect. Changelog: §8.
r4 applies the r3 review (`docs/reports/adr-0026-review.md`, section r3): deadline from the turn start, timer slack,
wait mechanism, one wait per keypress path, tighter benign gates. r3 follows the owner decision of 2026-10-24 (canonical actor model: plugins are actors, **B' is the design
target**, §2.4.6, §5) and records QP-1..QP-5 as confirmed (§6).
Refines: ADR-0019 (extension platform: lifecycle, versioning V-1..V-8, packaging). ADR-0019 stays the
umbrella for packaging, lifecycle states and versioning; this ADR **supersedes** its §2.2 world table,
its `mirror`/`effects` interface sketch, its I-1 budget and its C-1 question (ADR-0019 carries
pointers). Builds on: ADR-0022 r2/r3 (registers, `FUSE`, SM-8, `PresIr`, §2.9 extension text paths,
§2.10 order), ADR-0024 r3 (fact sources, admission, `SourceState`, grants, taint, LB-1..LB-5,
FS-1..FS-12), ADR-0016 r3.1 (profile layers L3/L4), ADR-0001 (actors, R3: results as messages),
ADR-0025 (helper `pointer` capability).
Draft WIT (paper only, not built): **`wit/gorca-ext-0.10/`** (r1's `wit/gorca-ext-0.9/` draft was
never shipped and is replaced; see §8).
Inputs: `docs/reports/wasm-plugin-cost-spike.md` (wasmtime 49.0.3, loaded host),
`docs/reports/adr-0026-review.md` (review r1: H-1..H-5, M-1..M-6, L-1..L-4),
`docs/reports/plugin-fusion-math.md` (MQ-1..MQ-3, restrictions PF-1..PF-8),
`docs/reports/wasm-hook-overhead.md`, `docs/reports/ext-telegram-web-demo.md` (branch 98cb234).
Milestone: M18a (slices §2.9). Order fixed by the owner: **M22b1 → M18a runtime → first plugins**.

## 1. Context

The owner's rule: the core stays generic; everything app-specific lives in plugins (WASM components),
so core special cases go away. Today they live in three places:

| Where | What | Status |
|---|---|---|
| `crates/gorca-config/product/web-apps.toml` | `vscode` row: `browse.enabled = false` (NVDA does the same, `refs/nvda/source/appModules/code.py:23-30` `VSCodeDocument` blocks the tree interceptor) | L4 product row, compiled in, applies in safe mode (ADR-0016 M-5) |
| `crates/gorca-core/src/builtin_ext.rs` | `AppProfile::for_app`: Shell-only switches `speak_selection`, `watch_showing`, policy routing | "stand-in for per-app extension hooks until the WASM host is wired" (file header) |
| decisions 2026-10-19 (Signal plan item 3), 2026-10-17 (Telegram demo), 2026-10-23 (Telegram A study) | a `signal-desktop` row (mode default focus), the `ext-telegram-web` demo (WIT 0.3 `web`, unmerged), Telegram A/K polish | planned or on a branch |

The existing contract (`crates/gorca-ext-api/wit/gorca-ext.wit`, 0.3.0) is built on the legacy model:
a closed `role` enum, a `node` record with a toolkit-shaped `states: u64`, `present -> option<list<segment>>`
and `policy -> say(...)` that bypass presentation, `host.speak`, and a blocking-shaped `fetch`. The
owner forbids building the plugin API on legacy speech paths. ADR-0022 §2.9/§2.10 and ADR-0024 §2.10
already fixed the direction (plugins are fact sources; text paths go through the pipeline; the new
WIT reads semantics through accessors); this ADR turns it into a contract.

### 1.1 Prior art and what to avoid

- **NVDA app modules** are Python modules bound by executable name
  (`refs/nvda/source/appModuleHandler.py:96-117` `_getPossibleAppModuleNamesForExecutable`,
  `EXECUTABLE_NAMES_TO_APP_MODS`), one instance per process (`getAppModuleFromProcessID`, :241),
  with `sleepMode` (:494) and overlay classes (`chooseNVDAObjectOverlayClasses`, :646), plus
  `GlobalPlugin` singletons (`refs/nvda/source/globalPluginHandler.py:62-81`). Strengths: binding by
  app, sleep mode, per-object behaviour. Failures: no isolation (an add-on hang freezes NVDA), the
  whole Python namespace is the API so add-ons break every year (`addonAPIVersion.py` `BACK_COMPAT_TO`
  bumped 8 times since 2019, ADR-0019 §1), app modules mutate objects in place, so "why was this
  said" has no answer, and any add-on may bind any gesture including plain letters (keylogging and
  hijack are possible; review H-2). `code.py` itself is mostly data plus one feature (status-bar
  line/column read).
- **Orca scripts** (`refs/orca/src/orca/scripts/apps/*`) subclass the default script: every app script is
  a fork of the speech generator; this is the "legacy speech path" we must not rebuild.
- **ext-telegram-web demo** (98cb234): declarative selectors evaluated natively cost 30–36 ns/node; a
  guest call per node 35–70 ns; `describe` p99 8.5 µs on 3000 chats; the URL and route-change were
  missing in the core. Lesson: selectors as data, guest code only for nodes it claims.
- **wasmtime** offers fuel (`Store::set_fuel`, `refs/wasmtime/crates/wasmtime/src/runtime/store.rs:1019`),
  epoch deadlines (`set_epoch_deadline` :1094, `epoch_deadline_trap` :1125, `epoch_deadline_callback`
  :1157), `ResourceLimiter` (`runtime/limits.rs:69` `memory_growing`, :113 `table_growing`, :146
  `instances`, :164 `memories`), pooling limits (`config.rs:4283` `total_component_instances`, :4440
  `total_core_instances`, :4549 `max_memory_size`), `call_async` (`component/func/typed.rs:167`), and
  lazy lifted views of guest strings and lists (`WasmStr`, `component/func/typed.rs:1503`;
  `WasmList<T>`, :1755) that let the host check a length before copying or validating anything.
  Bulk memory operators are fuelled per byte in 49.0.3 (`VariableOperatorCost`, review §1), so
  `memory.fill`/`copy` loops are not a fuel escape. **Fuel counts operators, not time**: a cache
  miss, a page fault or host-side argument lifting costs no fuel (review §1, H-3).

### 1.2 Measured costs (spike, wasmtime 49.0.3, loadavg 9–14, tails pessimistic)

| Quantity | Value |
|---|---|
| compile (Cranelift) | 8 ms warm, 31 ms first in process |
| `.cwasm` deserialize (from a read buffer) | 0.1–0.2 ms |
| instantiate from `InstancePre` (pooled) | 3–6 µs p50/p99; rebuild after trap 9–22 µs |
| call, trivial | 46–50 ns p50, 51–57 ns p99 |
| call, record of 10 strings (200 B) | 460 / 570 ns |
| call, list of 20 records | 1.83 / 2.13 µs |
| guest → host import, trivial | 26 ns (a string-returning accessor is closer to 0.5 µs, by the 10-string record row) |
| `call_async` vs sync | 120–150 ns vs 46 ns |
| `spawn_blocking` hop per call | 5–14 µs p50, 0.14–0.19 ms p99 |
| memory per instance | 14–17.5 KiB + touched state (1 MB state ≈ 1 MiB) |
| core instances per Rust component | ≥ 3 |
| infinite loop → trap | epoch 1 tick (1 ms): 1.1 ms; fuel 3 M units: 138 µs, i.e. **≈ 21,739 units/µs** on one ALU loop (r1 said "22 units/µs": a 1000× slip, math §0) |
| hot-loop overhead | fuel 2–6 %, epoch 5–10 %, **fuel + epoch 34–36 %** |
| trap / panic | returned as `Err` in 13–35 µs, host unaffected |

Conclusion of the spike, adopted here: the boundary is negligible for our budgets; **guest compute is
the thing to budget**, and since fuel is not wall time, wall time is measured too (§2.5.2).

## 2. Decision

### 2.1 Principles

- **PM-1 Plugins contribute facts, not speech.** App knowledge enters the model as facts (ADR-0024),
  through the single `FUSE` in the node's writer (SM-8). Every surface (focus, browse, review, quick
  nav, elements list, braille) then benefits from one fix. A plugin never replaces an utterance.
- **PM-2 Data before code** (ADR-0019 PA-1). Profile rows, selectors, patches, categories, keymaps and
  settings schemas are manifest data evaluated natively. Code runs only for nodes a manifest
  `[[watch]]` selector matched (the core prefilters).
- **PM-3 Stratified reads.** Code that produces facts reads only the **observed view** (S0 after
  quirks, with S0 existence and structure); all selector evaluation uses the observed view in every
  lane. No plugin ever sees another plugin's patch, `hide` or flatten through a W-lane read or a
  selector (§2.3.2, PF-2). A plugin's facts therefore never depend on any plugin's facts.
- **PM-4 Bounded, in place (proposed; C-3 bake-off decides).** Fact rules run in the node writer's
  turn under a fuel grant **and** a wall-clock check, so the facts are in the first publish of a
  focused node. Everything that may need IPC is a request whose reply arrives as observed data or as
  a message (R3), never as a blocking return.
- **PM-5 Backend-agnostic.** No connector type or connector vocabulary crosses the boundary (FS-1):
  opaque handles, registry codes and host-minted anchors only. The same plugin runs over AT-SPI and
  Newton/AccessKit, and a plugin scope is per application, not per connector (§2.4).
- **PM-6 Fail open, fail alone.** A fault, overrun or violation costs that plugin's contribution for
  that call; the core presentation goes on. Faults and wall overruns are counted per (plugin, scope).
- **PM-7 Third parties cannot impersonate the system.** Untrusted plugin output is hedged or
  attributed, never plain author intent; system scopes are out of reach (§2.7).

### 2.2 Plugin kinds, worlds and what each may do

A **plugin** is an add-on package (ADR-0019 §2.8) with a manifest and zero or one component.

| Kind | Component world | Binding | Contributes | Example |
|---|---|---|---|---|
| **K0 data-only** | none | manifest matchers | `[[profile]]` rows (L3/L4), `[[selector]]`, `[[patch]]` (S1 for T2/trusted, S4 for untrusted T3, §2.6), `[[category]]`, `[[keymap]]` to core commands, `[settings]` schema, catalogs | `vscode` |
| **K1 app module** | `app-module` | app matcher (`AppMatcher`, ADR-0016) | K0 + `rules` (facts), `commands`, `reactor`, `present` (unstable) | `signal-desktop` |
| **K2 site module** | `site` (= `app-module` + `site-hooks`) | browser app matcher + URL glob (`DocMeta.url`) | as K1, scoped to one document; `route-changed` | `telegram-web` |
| K3 terminal policy | `terminal-policy` (WIT 0.3 `terminal`, unchanged) | terminal apps | ADR-0018; ported later | `ext-vte` |
| K4 global | `global` (ADR-0019) | none | not in M18a (needs a privacy design) | clipboard history |

**Presentation hooks** exist only in the IR-edit form of ADR-0022 §2.9 (`present.edit`: drop, replace,
insert-after, move-before on `PresIr` fields, then PR-1 dedupe and render). They are behind
`@unstable(feature = present-edit)` until M22b1 freezes `PresIr`, and none of the first three plugins
needs them. The 0.3 `present -> segments`, `policy -> say`, `on-event -> rewrite` and `host.speak`
forms do **not** exist in 0.10; the 0.3 world is served by the legacy host until its last user
(`ext-gnome-shell`, `ext-vte`) is ported (§2.8).

**Activation.** A plugin is *enabled* (user/first-party state, ADR-0019 §2.5) and *matched* per scope:
app matcher on `AppInfo` (verified identity first, as `profiles.toml`), for K2 also the document URL
read only when a loaded manifest has a URL glob (demo rule, privacy). Profiles never activate plugins
and plugins never activate profiles (ADR-0019 I-8, ADR-0016 PF-3); a profile may only carry the
plugin's own `ext.<id>.*` settings. NVDA's `sleepMode` maps to manifest `browse.enabled = false` rows
(data). A T3 matcher never matches a system scope (SS-1..SS-4, §2.7).

**Versioning.** ADR-0019 V-1..V-8 unchanged: `gorca:ext@0.10.0` is the preview, additive within a
major, open registry codes, `@unstable` for experiments, compatibility computed from imports/exports.

### 2.3 WIT 0.10: host interfaces and plugin exports

Draft: `wit/gorca-ext-0.10/{types,host,plugin}.wit`. Summary:

| Interface | Direction | Lanes | Content |
|---|---|---|---|
| `types` | — | — | `node-ref` (opaque, per instance, unforgeable), `anchor-ref` (opaque, host-minted), registry codes (`role-code`, `group-code`, `action-code`, `category-code`, `field-code`, `reason-code`), `prop-value`, `view-node` (frozen batch record), `prov-info` (u8 codes), `app-info` |
| `model` | import | W: observed view; R: published values over observed structure (§2.3.2) | `role`, `prop(n, group)`, `name`, `states`, `parent`, `children`, `subtree(root, depth ≤ 8, nodes ≤ 256)`, `text` (cache only), `attr` (neutral attribute keys, clipped), `matches`/`select` (manifest selectors, native, observed view), `anchor-of(n, kind)`, `prov`, `focus`, `cursor`, `doc-url`, `app` |
| `facts` | import, buffered | W only | `assert(target, group, value, layer, conf, ttl)`, `retract`, `hide`/`unhide`, `category`; `fact-target` = node / anchor / selector; `layer` = `patch` or `inferred`. No guest-supplied basis: the host uses the call's token (PF-7) |
| `output` | import, buffered | R only | `say(parts, urgency)` where a part is localised `text` or `node(n, reason)` = "present this node through the core pipeline"; `cue` (core cues and declared theme files only) |
| `actions` | import, buffered | R, command context only (AC-3) | `activate`, `focus`, `move-cursor`, `request-mode`, `pointer-click` (T2 only), `pass-key` |
| `settings` | import | `get` both lanes; `set` R lane inside a command only | own namespace |
| `timers` | import, buffered | R only | ≥ 50 ms, ≤ 16 armed |
| `requests` | import | W: observation fillers without delivery; R: delivered to `reactor` | `request(query, budget) -> id` (text, children, refresh), ≤ 8 in flight (§2.4.4) |
| `diag` | import | `log` both; `now-ms` R only (`none` in W) | rate-limited log, monotonic clock |
| `rules` | export | W | `attach(ctx)`, `derive(ctx, batch ≤ 64 dirty nodes)`, `removed(nodes)` |
| `commands` | export | R (ext worker) | `run(id, cmd-ctx)` |
| `reactor` | export | R (ext worker) | `deliver(id, reply)`, `tick`, `settings-changed` |
| `site-hooks` | export | R | `route-changed(url)`: resets plugin state, never speaks |
| `present` | export, `@unstable` | R inline (Reader) | `edit(ctx, fields) -> list<ir-edit>` (≤ 32 edits) |

#### 2.3.1 Backend neutrality (review L-3)

- `node-ref` is the host's handle for a `NodeKey` of the instance's scope; roles, groups, actions and
  causes are `gorca-model` registry codes with neutral names (`structure`, `text`, `rebound`, not
  toolkit event names).
- `attr` keys are lowercase neutral names that each connector maps: `tag`, `class`, `id`,
  `aria-role` (AT-SPI/Gecko `xml-roles`, AccessKit `role` of the HTML element), `aria-*`. A plugin
  that needs `attr` is web- or toolkit-aware by its own choice, never by the API.
- `rect` is window-relative logical pixels whatever the connector can report.
- `app-info.name` is the core's application identity (ADR-0016 matcher key): the connector-verified
  application id, normalised by the core (AT-SPI application name and Newton/Wayland `app_id` map to
  the same key through the identity table). **A scope is per application, not per connector**: when
  AT-SPI and Newton both serve one application, the plugin gets one scope and one instance pair over
  the merged nodes (ADR-0024 §2.3.5), never two sets of facts.

#### 2.3.2 Lane read rules (normative; review H-4, math PF-2)

| Read | W lane (`rules.*`) and admission | R lane (`commands`, `reactor`, `present`) |
|---|---|---|
| values (`role`, `prop`, `name`, `states`, `text`, `attr`, `view-node`) | **S0 only** (observed after quirks, plus cached text) | published fused entries (`Ivm::entry_view`, silenced sources masked) |
| existence, `parent`, `children`, `subtree` | **S0 existence and structure**: no S1 `hide`, no flatten, no S4, no plugin categories | same (observed structure); a cursor action on a node the published model hides fails at apply time with a spoken "not available" |
| `matches`, `select`, `[[watch]]`, `fact-target::selector`, declarative `[[patch]]`/`[[category]]` selectors | **S0 view**, every lane, every evaluation | S0 view |
| `prov`, `focus`, `cursor` | `none` | published |
| `settings.get` | yes (a change bumps the plugin's source epoch → re-derive, review L-2) | yes |
| `diag.now-ms` | `none` (no clock in derivation, review L-2) | yes |

Consequences: a plugin's W-lane inputs never contain any S1..S4 output (CR-8), selectors cannot
chain through other plugins' patches (no order dependence, no oscillation: math C-2), and R-lane
reads cannot reach a W lane except through a user command (`settings.set` is command-only, timers
cannot set settings).

#### 2.3.3 Handles and anchors are unforgeable (review M-3)

- `node-ref` is minted per **instance**: `index ‖ generation ‖ MAC(instance key, index, generation)`
  packed into the u64 (a keyed 24-bit MAC is enough: a forgery succeeds with p = 2^-24 and every
  failure is counted; 16 failures in a call = malformed-effect fault). Handles are minted only by
  the host (batch, accessor results). A forged, foreign-instance or dead handle reads `none` and its
  effects are dropped; it is never a read of another node.
- Anchors (`dom-id`, `structural`, `region`) are minted by the host through `model.anchor-of(n, kind)`
  and returned as an opaque `anchor-ref` with the same MAC scheme; the guest never computes a path
  hash, a capture fingerprint or a DOM count.
- The snapshot token is host-side state (§2.6.1); WIT 0.10 has no `snapshot-token` type and no
  `basis` parameter, so a plugin cannot pass an older token to weaken stale-on-arrival.

C-1 of ADR-0019 is resolved: handles + accessors for everything that grows, plus one frozen batch
record (`view-node`) for bulk reads. A list of 20 records costs 1.8 µs; 20 trivial accessor calls
cost ≈ 0.5 µs, but 20 string accessors cost ≈ 10 µs (≈ 0.5 µs each, spike 10-string row), so name-
and text-heavy code should use `subtree`/the batch record. No `resource` type (no drop protocol
across lanes; the MAC gives the unforgeability a resource would).

### 2.4 Lanes, instances and scheduling

```text
            app actor / doc actor (node writer, SM-8)                Reader / ext worker
 events -> coalescer -> S0 registers -> stamps[n][class] -> [trigger: reverse index + watch prefilter]
                                         |                     -> W instance.derive -> facts buffer
                                         +-- admit (gorca-fact-api) <---+  (token check, PF-7; same turn or later)
                                         v
                                   FUSE S-1..R -> publish SemEntry ---------> R instance: commands,
                                                                               reactor, present.edit
```

This is the arm-A picture (W instance called inline). **The design target is B' (r3, §2.4.6):** the
`W instance.derive` box is a message to the scope's plugin actor, the writer waits at most 0.3 ms on
the interactive slice, and the reply is a message that enters at `admit` under the same token check.

- **Instances.** Per (plugin, scope) at most two instances of the same component: **W** (a **plugin
  actor** with its own mailbox, run on the plugin thread pool, §2.4.6; the fallback arm A runs it
  inline in the writer: the app actor for K1, the document actor for K2) and **R** (ext worker;
  `present.edit` inline on the Reader). They share no memory; R learns what W knows only through the
  model. A `Store` is owned by exactly one actor and never shared between threads.
- **Pre-load (review H-3e).** At daemon start and on enable, a background task reads every enabled
  plugin's `.cwasm` into memory and deserializes it **from the buffer** (`Component::deserialize`,
  never `deserialize_file`, which maps the file and page-faults code in on first execution), then
  builds the `InstancePre`. Code then lives in anonymous memory that was written at load; `mlock` of
  the code range is an option decided by WQ-5. No deserialize, compile or file read ever happens on a
  writer turn. A plugin whose `InstancePre` is not ready is skipped (fail-open) for that turn.
- **Pool.** Pooling allocator: `total_component_instances = 512`, `total_core_instances = 2048`
  (≥ 4 × instances, spike §3), `max_memory_size` 32 MiB virtual per memory. Idle eviction: R after
  10 min without a call, W when its scope closes. `.cwasm` cache keyed by package hash + `Engine::precompile_compatibility_hash()` (wasmtime version,
  compiler flags, **host ISA flags**, features, tunables; ADR-0027, ADR-0019 §2.5).
- **Attach (review H-3c, math §2.3).** `attach` runs eagerly when the scope attaches, on the writer's
  background lane under the B-1 grant, never on the interactive slice; it is followed by one `derive`
  of every currently watched node (a plugin whose read set never changes would otherwise never run).
  `removed` runs on the background lane under B-1, batches ≤ 64 handles. A W slot that is not
  `Ready` at a focus change is skipped for that turn; its facts arrive through the background refine.

#### 2.4.1 Trigger rule (normative; review H-5, math PF-4)

`derive` is called for a node only on: (a) attach; (b) a change, in a read class of the call's mask
M, of the S0 projection of any node in the read set R of a live token of this plugin whose target is
that node (reverse index `node → tokens`, §2.6.1), or a new match of a `[[watch]]` selector on the S0
view; (c) TTL refresh of an `inferred` fact; (d) a source-epoch bump (settings change, hot reload).
**Never** on fact admission, on any fused S1..S4 change, or on the plugin's own facts: a plugin's
output cannot re-trigger it (math C-3). Dirty nodes are coalesced per (plugin, node), latest cause
wins.

#### 2.4.2 Interactive slice and budget split (normative; math PF-8, MQ-3; review M-4)

On a focus change, before publishing the focus chain, the writer calls `derive(interactive = true)`
for watched nodes of the focus chain and active descendant only (≤ 16 nodes, ≤ 16 plugins: beyond
16 the rest go to the background slice). The aggregate budget B-2 is split by **progressive filling**:

1. Reserve `n · c_max` (c_max = 2 µs: entry and marshalling are not fuel-charged); fuel pool
   `P = κ_int · (B − n · c_max)`, κ_int = the interactive calibration rate (§2.5.1).
2. Order the n plugins by predicted fuel ascending (per-plugin EWMA of the p95 of its interactive
   calls). **Ties are broken by a rotating offset** (the focus-change counter mod n over plugin ids),
   never by fixed plugin id.
3. For k = 1..n grant `a_k = min(cap, R_k / (n − k + 1))` fuel (R_k = remaining pool, cap = B-2 per
   call); unused fuel returns to the pool. Per-call fuel is therefore always `min(cap, remaining)`.
4. On `OutOfFuel` the call's effects are dropped (fail-open) and the nodes go to the background
   slice (LB-4 refine). A **strike** only if the plugin trapped at the per-call cap, or its p95
   exceeded the floor `P/n` in 3 consecutive focus changes (then it is demoted to background-only
   for 60 s). Trapping below the floor because of crowding is not misbehaviour.

Guarantees (math Proposition B): `Σ used ≤ P`; every plugin gets at least `min(cap, P/n)`; max-min
fair when predictions order correctly; worst-case added delay
`D ≤ n·c_max + ρ·(B − n·c_max) + δ` with ρ = κ_int/κ_min (calibrated over slowest real rate) and δ = 0
when imports pre-charge (B-8). **No carry-over** between focus changes (the budget is a per-event
latency bound, not a throughput share; deficit round robin was considered and rejected for that
reason, math §4.4). Simulated at n = 16: 87.7 % of plugins complete vs 67.3 % for fixed-order greedy
(28 % of cheap plugins starved) and 69.4 % for an equal split.

#### 2.4.3 Background slice

Batches of ≤ 64 nodes on the writer's background lane, interleaved with ADR-0024 LB-2 slices, B-1
grant per call. The same wall-clock accounting applies (§2.5.2).

#### 2.4.4 Requests from the W lane (normative; review H-5)

- A W instance may issue `text`, `children` and `refresh` requests; **replies never come back to the
  plugin as values**. They enter the observed view as observations of the primary source: `text`
  fills the core text cache, `children` fills the mirror, `refresh` refetches the node. If S0 changes,
  the class stamp bumps and the trigger rule (§2.4.1) re-derives with a normal token; if nothing
  changed, nothing is re-derived. A `refresh` that returns identical data therefore ends the loop.
- Bound: at most one outstanding request per (node, query kind) per instance, and at most one per
  (node, query kind, class stamp): a second `refresh` of a node whose S0 did not change since the
  previous one is dropped and counted. ≤ 8 in flight per instance (ADR-0019 I-2), host deadlines
  as I-2. So a `derive → refresh → derive` chain costs at most one refetch per real application change.
- **Decision: request replies can become facts, only through the observed view.** A W-issued reply is
  observed data like any other S0 value, so facts derived from it are ordinary view-functional facts
  with an exact basis (fixes Signal P-2: uncached message text). R-lane replies (`reactor.deliver`)
  can never become facts: the R instance has no `facts` import. They may be spoken or used to act
  inside the issuing command (AC-3).

#### 2.4.5 Determinism

Which plugin completes in the interactive slice depends on predictions, so the *first utterance* may
differ between runs; the *final* model cannot: with PF-1..PF-4 and PF-7, once primary inputs are
quiescent the published model equals `Full(S0)` (every plugin called once on the final S0, fused),
whatever the batching and call order (math Theorem P; 0 mismatches in 12,000 randomized runs).

#### 2.4.6 The plugin actor: B' as the design target (r3; owner decision 2026-10-24)

The owner follows the canonical actor model: a plugin is an actor with a mailbox, it talks to the
writer only by messages, and as many plugins as there are cores run in parallel. Arm A (inline
`derive` in the writer) is the **fallback**, not the baseline.

```rust
// Writer -> plugin actor (bounded mailbox: 1 job running + 1 coalesced pending dirty set)
enum PluginMsg {
    Derive(DeriveJob),          // { job_id, slot_epoch, t, interactive, rows: Arc<FrozenRows>,
                                //   grant: FuelGrant, deadline: Instant }
    Cancel { job_id },          // superseded or scope closing; cooperative (checked at every import)
    Shutdown,
}
// Plugin actor -> writer actor (the writer's own mailbox, never a shared structure)
enum PluginReply {
    Done  { job_id, slot_epoch, token: Token /*(t,R,M,g), host-minted*/, facts: Facts, taint: TaintBits,
            exec: Duration, queued: Duration, fuel_used: u64 },
    Fault { job_id, slot_epoch, kind: FaultKind, exec: Duration },
    Cancelled { job_id, slot_epoch },       // not a fault, no strike
}
```

- **Ownership.** The writer (app or document actor) owns the `Slot` (§2.5.4), the clock `T`, the stamps
  and admission. The plugin actor owns its `Store`/`Instance`, the frozen view and the per-call token
  under construction. Nothing is shared mutably; the only shared data is the immutable `FrozenRows`.
- **Plugin thread pool.** Plugin actors run on a dedicated pool of OS threads (not tokio workers: a
  guest call is CPU-bound and only fuel stops it). Size `T = max(2, cores - 2)`, to be set by the
  M18a-0 numbers; an actor prefers the thread it last ran on (warm cache) and the pool prefers idle
  threads. R-lane work keeps its existing ext worker.
- **Interactive handoff.** On a focus change the writer (0) reads the clock: **`t0` = start of the writer
  turn, and the deadline is `D = t0 + 0.3 ms`** (r4; it is not measured from the send), (1) recomputes S0
  and bumps `T`, (2) builds one `FrozenRows` per plugin batch (the batch rows plus the immutable snapshot
  of S0 reachable within the import bounds, taken **after** step 1 so the token's `t` is the snapshot
  turn; eager only: the WQ-6 lazy variant is rejected because the plugin thread would read writer state
  concurrently, which breaks PF-1/PF-2 and I-P8), (3) sends `Derive{interactive}` to every plugin actor
  in the focus chain at once (parallel), (4) waits for the replies until `D` (mechanism below), (5) fuses
  and publishes with the facts that arrived and the rest missing, (6) late `Done` messages are admitted in
  the writer's later turns and trigger the LB-4 refine. Snapshot, send and fuse are charged **inside** the
  0.3 ms: if the snapshot alone exhausts it, the wait is zero and the writer publishes without facts.
  The wait is the **only** blocking call in a writer and is bounded by B-2 by construction (an explicit,
  documented exception to "actors never block": `tokio::time` has about 1 ms granularity and cannot
  express 0.3 ms). The wake cost (about 5-20 µs, to be measured) is charged inside the 0.3 ms.
- **Wait mechanism (normative, r4).** Never a blocking receive on a tokio channel (`blocking_recv`
  panics inside a runtime, and a park stalls the worker together with the tasks in its local queue).
  Per handoff the writer owns an **atomic countdown (initial value = number of plugin actors asked) and a
  futex word** (`futex_wait` with an absolute `CLOCK_MONOTONIC` deadline `D`, after spinning for about
  10 µs); each plugin actor decrements the counter after it has posted its `PluginReply` to the writer's
  mailbox and wakes the futex at zero. The writer returns early at zero, otherwise at `D`, then drains
  the mailbox (replies are always delivered through the mailbox; the counter only ends the wait). The
  writer's worker thread is blocked for at most `D`. The alternative with the same bound that blocks no
  worker, an async `select` over (countdown notify | `timerfd` deadline through `AsyncFd`, microsecond
  resolution), is measured against the futex in M18a-0 (WQ-6) and replaces it if it meets the gates;
  `tokio::time` is not used for `D`.
- **Timer slack (normative, r4).** A timed futex or `timerfd` wait on a SCHED_OTHER thread is subject to the
  thread's timer slack, 50 µs by default (`/proc/self/timerslack_ns` = 50000 on the dev host), which alone
  would make "p99 <= 0.31 ms" fail for an adversarial plugin that always runs out the clock. Every
  thread that can run a bounded wait (writer worker threads, and the plugin pool threads that wake the
  futex) calls `prctl(PR_SET_TIMERSLACK, 1)` at start (1 ns; the value 0 means "reset to default"). If a
  tokio worker cannot be configured per thread, the runtime's `on_thread_start` hook does it. M18a-0
  measures **late wakeups** (actual wake minus `D`, p50/p99/max, benign and loaded host) with and
  without the slack setting (WQ-6).
- **One bounded wait per keypress path (normative, r4).** A keypress path waits for plugins at most once:
  when both the app actor (K1) and the document actor (K2, Firefox) have a focus change for the same
  keypress, only the writer that owns the utterance waits, on the shared deadline `D` computed from the
  turn start of the **first** writer in the path; the other writer sends its `Derive` and publishes
  without waiting (late facts follow under LB-4). The total plugin stall of a keypress is therefore
  at most 0.3 ms, not 2·D. A flapping-focus app cannot make its writer wait on every turn: a writer
  takes an interactive wait at most once per `N` ms (default 20, to be set in M18a-0), otherwise its
  focus handoffs run as background class.
- **Background slice.** No wait at all: `Derive{interactive: false}` is sent and the reply is admitted
  when it arrives. This is the only path for batches over 16 nodes and for attach/removed.
- **Backpressure and coalescing.** At most one job running and one pending dirty set per plugin actor
  (latest cause wins per node, §2.4.1). If a new interactive batch finds the actor busy (previous job
  not finished), the writer does **not** wait for that plugin: it merges the nodes into the pending set
  (they run next, background class), counts `pa_busy`, and publishes without its facts (fail-open).
  An interactive job pre-empts a running **background** job of the same actor: the writer sends `Cancel`
  and re-queues the background nodes, so a long background batch never turns every focus change into
  `pa_busy`.
  A superseded running job is sent `Cancel`: the next import traps with `Cancelled` (free in the strike
  rules); a pure-compute loop is stopped by fuel as before.
- **Fuel grants under parallelism.** Grants stay as in §2.4.2 (progressive filling with `P = κ_int·(B -
  n·c_max)`), which is the **serial-equivalent bound**: even if all n jobs ran on one thread they would
  fit in B-2. This is conservative; with free threads the true bound is the shared deadline `D`. A
  tighter grant for the parallel case (`a_k` up to the cap whenever `n` is at most the idle pool
  threads, progressive filling over `T` threads otherwise) is **MQ-4** for the math researcher and
  WQ-6 for the bake-off; the serial-equivalent rule applies until MQ-4 is proved.
- **Wall accounting** (§2.5.2) is measured on the plugin thread around the guest call (`exec`); the
  mailbox delay (`queued`) is a separate metric and never an overrun. The writer's wait is bounded by
  `D` regardless of the guest.
- **Slot machine under messages (delta to §2.5.4).** `Ready` gains `busy: Option<JobId>` and the slot
  an incarnation counter `slot_epoch` (bumped on every transition out of `Ready`, on package swap and
  on scope close). The events "call ok", "fault" and "wall overrun" are the corresponding
  `PluginReply` messages processed by the writer; a reply whose `slot_epoch` differs from the slot's is
  **dropped and counted** (its facts are never admitted; it cannot revive a `Backoff`/`Disabled` slot
  or re-open a closed scope). "Call needed" while `busy` = coalesce into the pending set. Every cell of
  the §2.5.4 table stays as written; the new cells ("stale reply" per state, "call needed while busy")
  are tested with the same exhaustive harness (M18a-2).
- **Plugin-actor faults that are not guest faults** (host panic in the actor, a closed reply channel, no
  heartbeat for 10 × the longest grant): the supervisor drops the actor, records a strike (`kind =
  ActorLost`) and spawns a replacement from `Cold`; a wedged thread is detached (it holds no writer
  state; imports are non-blocking host calls on immutable data) and the pool grows by one up to a hard
  cap, after which the slot is `Disabled`.
- **Shutdown and scope close.** `Shutdown` is sent; the writer does not wait; the actor drops its
  `Store` after the current call ends (bounded by fuel).
- **One result path.** Arm A would deliver `Done` synchronously inside the same turn; the admission
  code is the same function in both arms, so the fallback is a different *runner* (`InlineRunner` vs
  `ActorRunner` behind one `WLaneRunner` trait), not a different model.

**Restrictions PF-1..PF-8 under asynchronous delivery (re-checked for r3; none is relaxed).**

| Rule | Holds under B' because | Extra test |
|---|---|---|
| PF-1 view-functional S1 | the call's view is the immutable `FrozenRows` at turn `t`; the result is a function of it, wherever and whenever it runs | differential: B' result equals A result on the same view (bit-equal facts) |
| PF-2 S0-only views, structure included | the plugin thread has no access to fused entries at all (only frozen S0 rows are reachable), so the rule is enforced structurally, not by convention | A7 (patches matching each other's selectors) under B' |
| PF-3 plugin facts never enter S0 | admission stays on the writer through `gorca-fact-api`; the actor cannot write any register | existing |
| PF-4 trigger rule | a late `Done` is a message handled by admission and enqueues no job; only stamp changes, TTL, attach and epoch do (§2.4.1) | A12 (self-trigger) with deliberately delayed replies: still 1 call per real change |
| PF-5 taint-clean derivation | taint bits are computed by the host while serving imports on the actor (from the provenance on the frozen rows) and travel in `Done`; the guest cannot set them; admission re-checks tier against taint on the writer | T3 taint test with deferred admission |
| PF-6 total S1 rank | CR-2 ranks by (tier, specificity, id, anchor key), never by arrival order, and `FUSE` is a pure function of the admitted set, so a late arrival cannot change the winner, only when it appears | permutation test: all arrival orders of 3 plugins' replies give the same fused entry |
| PF-7 token fixes the basis | exactly the case it was written for (§2.6.1): `t` is the snapshot turn, `g` the `BindGen` at the snapshot; stale iff some `n ∈ R, c ∈ M` has `stamp > t` or the `BindGen` changed; `slot_epoch` additionally rejects replies of a dead incarnation | MQ-2 replay corpus with randomized delivery delay: 0 false accepts |
| PF-8 interactive budget | the writer's added delay, measured **from the start of the writer turn** (snapshot, send, wait and fuse included), is at most `D` (0.3 ms) by the bounded wait with timer slack 1 ns, and one wait per keypress path; per-plugin grants are the serial-equivalent progressive filling (§2.4.2) until MQ-4 | PX-4 A1-A6 on B', stall max reported |

Determinism (§2.4.5) is unchanged: once primary inputs are quiescent, every reply is admitted or
rejected by the token, the re-derivations caused by rejections run on the final S0, and the final
model equals `Full(S0)`. Only the *first utterance* depends on the race between the plugin and the
0.3 ms deadline, which is what the "facts at first utterance" gate measures.

### 2.5 Performance and safety

#### 2.5.1 Budgets (provisional; frozen only after WQ-1..WQ-3 in M18a-0)

Budgets are stated in **wall time**; the fuel grant for a call is `time × κ`, where κ is a calibrated
rate in units/µs. Spike reference (one ALU loop): κ_spike ≈ 21,739 units/µs, so 0.2 ms ≈ 4.35 M
units, 0.3 ms ≈ 6.5 M units, 1 ms ≈ 21.7 M units. Normative calibration:

- κ_int (interactive lane) = the **p01 rate over WQ-2's shapes**, including a pointer chase over the
  instance's maximum memory and string-heavy code, on the slowest core class of the host (P/E), so
  ρ ≈ 1 (math §4.5 option 1).
- κ_bg (background and R lanes) = the median rate; wall accounting catches the rest.
- Per plugin, adaptive: `κ_p = min(κ_lane, p10 of the plugin's measured units/µs over its last 64
  calls)`; a plugin whose code shape runs slowly gets proportionally less fuel (review H-3a).

| Id | Path | Per call | Aggregate | On overrun |
|---|---|---|---|---|
| B-1 | `derive` background, `attach`, `removed` | 1 ms | writer background share (ADR-0024 LB-2), batch ≤ 64 | call aborted, effects dropped, strike, batch requeued once halved, then the nodes are skipped for 10 s |
| B-2 | `derive` interactive (focus chain) | 0.2 ms cap | **0.3 ms per focus change, progressive filling (§2.4.2)** | fail-open, refine via LB-4; strike rule of §2.4.2 |
| B-3 | `present.edit` (unstable) | 0.1 ms | **0.3 ms per utterance** (replaces ADR-0019 I-1's 2/3 ms), same progressive filling | fail-open, strike |
| B-4 | `commands.run` | 20 ms | one in flight per plugin | effects dropped, strike, "add-on did not respond" cue |
| B-5 | `reactor.*`, `route-changed`, `settings-changed` | 5 ms | per plugin queue ≤ 64, coalesced per (kind, id) | effects dropped, strike |
| B-6 | effects per call | facts ≤ 256 (4 × batch), other effects ≤ 64, effect text ≤ 32 KiB total | — | **whole call rejected (atomic, ADR-0019 I-3) + malformed-effect fault**; never partial (review M-2) |
| B-7 | memory per instance | 16 MiB default; manifest may ask up to 64 MiB (T2 only) | 512 MiB all plugins | `memory_growing` denies → guest traps → fault |
| B-8 | host imports | each import **pre-charges** fuel for its estimated work before doing it (e.g. `subtree` 1 unit per node + per byte/16) and traps without doing the work when the grant cannot cover it | — | runs out like guest code; δ = 0 in the bound |
| B-9 | argument and result sizes | caps of §2.5.3, checked **before** any copy or UTF-8 validation | — | effect dropped / call rejected, malformed-effect fault |

Budget fit: keypress → speech p99 < 15 ms gorca-side; under B' the writer adds at most the wait
`D` = 0.3 ms (plus B-3 on the Reader); under arm A at most 0.6 ms for plugins whose rate is at least
κ_int; typically < 10 µs either way. Under memory-bound adversarial code the
bound is ρ-dependent (math §4.5: ρ = 3 → ≈ 0.86 ms) until the wall-clock rules demote the plugin;
this is exactly what the C-3 bake-off measures (§5). 10k events/s: with the prefilter only matched
nodes reach a plugin; even if all 10k nodes/s match, batches of 64 make ≈ 160 calls/s × ~2–8 µs of
marshalling ≈ 1 ms/s, plus guest work bounded by B-1.

#### 2.5.2 Fuel plus wall-clock accounting (decision C-2, revised by review H-3)

- **Fuel only on the engine (no epoch).** Each call gets `set_fuel(grant)`; `OutOfFuel` traps
  deterministically with ≈ 0 overshoot. Epoch interruption is not enabled in the plugin engine: fuel +
  epoch costs 34–36 % on compute loops against 2–6 % for fuel alone, its granularity (≥ 1 ms tick) is
  coarser than every interactive budget, and it is checked at the same loop headers as fuel, so it
  adds nothing against page faults, cache misses or lifting (review §3).
- **Wall clock around every guest call** (two `Instant::now()`, ≈ 40 ns). A **wall overrun** is a call
  whose wall time exceeds `max(2 × grant_time, grant_time + 50 µs)`, `grant_time = grant / κ_p`. On a
  wall overrun: the call's effects are still admitted if it returned normally (it is late, not wrong),
  the plugin's κ_p is updated from the measurement, it is demoted to background-only in that scope for
  60 s, and an overrun is recorded. **3 wall overruns within 60 s = one fault** in the slot machine
  (§2.5.4), so a slow-but-not-faulting plugin goes to `Backoff` and then `Disabled` (closes prior F-1).
  OS preemption is not filtered out on purpose: one preempted call never reaches 3 in 60 s; a plugin
  that repeatedly is the call during which time is lost is demoted either way, which is the safe side.
- **Host watchdog inside imports**: every 64th import checks the wall clock and traps the call when it
  exceeds 4 × its grant time (catches long import chains early; does not tighten the bound for pure
  compute loops, math §4.2).
- Under B' (the design target) the writer's wait is bounded by wall clock (the shared deadline `D`,
  §2.4.6) regardless of all the above; the per-plugin accounting is the same, run on the plugin thread.

#### 2.5.3 Size caps before copy (review H-3d)

Host imports are implemented with lazy parameters (`WasmStr`, `WasmList<T>`; exported results lifted
the same way) and check the length against the cap before copying or validating; over cap = the
import returns its empty value or drops the effect, and counts one malformed-effect fault per call.
Whether wasmtime's bindgen can emit these parameter types or hand-written `func_wrap` is needed is
checked in M18a-0.

| Item | Cap |
|---|---|
| `diag.log` message | 256 B |
| `facts` text value, `output` literal, `settings` words | 1 KiB each; effect text per call ≤ 32 KiB (B-6) |
| `attr` key, `settings` key, `cue` name | 64 B |
| `output.say` parts | ≤ 16 |
| `present.edit` result | ≤ 32 edits, text ≤ 512 B each |
| `requests` text length | ≤ 64 KiB |
| list arguments (`removed`, facts per call) | as B-6 / batch size |
| component file / data segments / package (manifest validator) | ≤ 8 MiB / ≤ 4 MiB (T3) / ≤ 16 MiB |

#### 2.5.4 Fault model and slot state machine (review L-1)

Faults: trap (`unreachable`, panic, OOB), `OutOfFuel` per §2.4.2/B-1 rules, watchdog, limiter denial
trap, malformed effects (forged or foreign handle beyond the tolerance of §2.3.3, unregistered code
beyond the registry range, cap exceeded, B-6 overflow), 3 wall overruns in 60 s. Not faults: calls to
imports the plugin has no permission for (reads return `none`, effects are dropped and counted, one
diagnostic per kind). Admission rejections of facts are ADR-0024 `Violation`s.

```rust
struct Slot {                            // per (plugin, scope, lane)
    state: SlotState,
    level: u8,                           // 0..=2 backoff escalations this session (kept across scope close)
    strikes: Window<3, 60_s>,            // faults
    overruns: Window<3, 60_s>,           // wall overruns; full window = one fault
    interactive_ok_after: Option<Instant>, // demotion to background-only
}
enum SlotState {
    Unloaded,                            // no InstancePre yet (pre-load running)
    Cold,                                // InstancePre ready, no instance
    Ready { inst: Instance },
    Backoff { until: Instant },          // facts masked (SourceState::Paused)
    Disabled,                            // this scope, this session (SourceState::Quarantined)
}
```

"Fault" below means: record a strike; if the strike window is not full → (drop instance) `Cold`; if
it is full and `level < 2` → `Backoff{until: now + D(level+1)}`, `level += 1`; if full and
`level = 2` → `Disabled`. D(1) = 30 s, D(2) = 5 min. So a crashing plugin is disabled after at most
9 faults per session.

| State \ Event | pre-load done | pre-load failed | call needed | call ok | fault | wall overrun (window not full) | backoff expires | user re-enable | package swapped | scope closes |
|---|---|---|---|---|---|---|---|---|---|---|
| `Unloaded` | → `Cold` | → `Disabled` + notice | skip (fail-open) | — (impossible) | — (impossible) | — (impossible) | — | stay | restart pre-load | slot dropped, `level` kept |
| `Cold` | stay | — | instantiate (3–9 µs) → `Ready` and call; instantiate failure = fault | — | fault (from instantiate) | — | — | stay | → `Unloaded` | slot dropped, `level` kept |
| `Ready` | stay | — | call (interactive only if not demoted) | stay; unmask facts if masked | discard effects, fault | record, demote 60 s, stay | — | stay | → `Unloaded` | slot dropped, `level` kept |
| `Backoff` | stay | — | skip (fail-open) | — | — | — | → `Cold`, strikes and overruns cleared, `level` kept; facts unmasked after the first good call | → `Cold`, `level` 0 | → `Unloaded`, `level` kept | slot dropped, `level` kept |
| `Disabled` | stay | — | skip | — | — | — | — | → `Cold` (or `Unloaded`), `level` 0, windows cleared | stay | slot dropped, stays disabled for the session |

- The W and R slots of a scope share `strikes`, `overruns` and `level`. The plugin's fact source has
  the matching ADR-0024 `SourceState` keyed by `StableIdentity { name: id, peer:
  PeerProof::Extension(id@sha256) }`: `Backoff` = `Paused` (masked, synchronous re-fuse of focus chain
  and visible window, FS-7), `Disabled` = `Quarantined` for that scope. ADR-0019 §2.5 escalation
  stays: disabled in 3 scopes within 10 min → package `Quarantined` (all scopes), rollback to the
  previous version if within 24 h of an update. In dev mode (T3-dev) a package swap resets `level`.
- Reporting: one spoken notice per plugin per session ("Signal add-on stopped; gorca continues
  without it"), counters per (plugin, scope) (calls, fuel p99, wall p99, faults by kind, overruns,
  dropped effects, admission rejections), the last fault in `explain` and the Control API
  diagnostics (M14).
- A fault never touches core state: effects are applied only after a successful return (ADR-0019 I-3);
  facts admitted earlier stay unless the source is masked.

### 2.6 Facts, layers and conflict rules

Plugins are ADR-0024 fact sources of class **Patch** (S1) or **Inferred** (S4); never **Secondary**
(PF-3: a plugin's facts never enter S0, otherwise the feedback loop closes through the observed
view). Scope is the instance's scope (FS-11); seq and `SourceId` are assigned per instance by the host.

| Producer | Stratum | Who may | Speech |
|---|---|---|---|
| manifest `[[patch]]` (declarative), T2 or user-trusted T3 | S1 | enabled plugin | plain |
| manifest `[[patch]]`, untrusted T3 | **S4** (Inferred, conf class capped at `Probable`) | enabled plugin | hedged |
| `facts.assert(layer = patch)` (computed patch), T2 or user-trusted T3 | S1 | `contribute-facts` with `patch` | plain; T3-trusted values tainted (PF-5) |
| `facts.assert(layer = patch)`, untrusted T3 | **S4**, as `inferred` | `contribute-facts` | hedged |
| `facts.assert(layer = inferred)` | S4 (below derived) | `contribute-facts` | hedged by class; T3 capped at `Probable` |
| `hide` (manifest or computed), T2 or user-trusted T3 | S1 existence register | `contribute-facts` with `patch` | — |
| `hide`, untrusted T3 | **advisory only**: recorded at S4 as an evidence flag shown in `explain`; existence unchanged | — | — |
| evidence groups (bit 15) | side table, read by S3/S4 | `contribute-facts` | never directly |
| `[[profile]]` rows | settings plane (L3 third-party off by default, L4 first-party on) | ADR-0016 | — |

No plugin fact of any tier may target a node in a system scope unless the plugin is T2 (§2.7, SS).

#### 2.6.1 Basis and staleness: the revision token (normative; math MQ-2, PF-7)

r1's mapping of the token to `(sem_rev, vis_rev, BindGen)` is **withdrawn**: `sem_rev` bumps when the
published entry changes, so every fact invalidated itself on admission (1288/1288 false rejects);
`vis_rev` misses role, state, text and attribute changes (291 false accepts in 1712 stale cases).

- Writer clock `T` (u64), strictly increasing per turn. Per node `stamp[n][c]` for six read classes
  c ∈ {`struct`, `core`, `names`, `text`, `attrs`, `geom`} = the turn in which `π_c(S0(n))` last
  changed (set from the S0 recompute's changed-group mask; O(1) per changed class; 24 B per node as
  u32 turn offsets, or sparse for nodes in some live read set).
- Every W-lane call has a host-side token `(t, R, M, g)`: t = the turn, R = the batch plus every node
  any import returned during the call (recorded while serving; capped at 256, beyond which the
  scope-wide stamp is used: more false rejects, never a false accept), M = the read classes the
  plugin's permissions and the imports it used allow, g = `BindGen(target, t)` per target anchor.
- **Stale ⇔ ∃ n ∈ R, c ∈ M: `stamp[n][c] > t`, or `BindGen(target, now) ≠ g`.** Zero false accepts
  (Theorem S); false rejects only on ABA. The writer stamps `g` from the token, never the
  admission-time generation (PF-7), which keeps F18 when admission is deferred (arm B', future async
  producers).
- Lifetime (FS-5) uses the same reverse index `node → live tokens` as the trigger rule: a stamp bump
  marks dependent tokens stale and dirties their targets; `FUSE` reads one stale bit per register.

#### 2.6.2 Normative restrictions (math PF-1..PF-8)

- **PF-1 View-functional S1.** A computed S1 value is a function of the call's view (S0 of R), static
  manifest data and the plugin's settings only; private plugin state may influence S4 facts only.
  Enforced by differential testing in `ext-test` (re-derive in a fresh instance on the same view,
  values must be equal); T2 S1 plugins must pass it to ship; a T3 plugin failing it loses S1 (trust
  flag ignored) with a diagnostic.
- **PF-2 S0-only views, structure included.** §2.3.2.
- **PF-3 Plugin facts never enter S0** (never a Secondary source).
- **PF-4 Trigger rule.** §2.4.1.
- **PF-5 Taint-clean derivation.** A T3 computed or declarative patch value (trusted T3 at S1) is
  tainted. S3 rules (name from content, relation text, positions) read each child's **best untainted
  value** (its value below tainted winners), using tainted values only when no untainted part exists
  (then the result is DerivedTainted). One "winner tainted" bit per (node, S3-read group); no new
  register. With it, F12 holds in the restated form below (math: 118 → 0 violations in 4000 models).
- **PF-6 Total S1 rank.** CR-2 below.
- **PF-7 Token, not admission, fixes the basis.** §2.6.1.
- **PF-8 Interactive budget.** §2.4.2 and §2.5.1.

**F12 for plugin facts** (amends ADR-0024 §7 when this ADR is accepted): "adding an Inferred,
Secondary or Annotation fact, or a T3 Patch fact, never changes a group whose winner is User, Patch,
untainted Derived or Observed-primary with a non-empty value, except, for a T3 Patch fact, the groups
it targets." T2 patches are first-party author intent, exempt like declarative rows.

#### 2.6.3 Conflict rules

- **CR-1 Across layers**: user > patch > observed > derived > inferred > default. A plugin can
  override an engine name only at S1; built-in S-1 quirks and S3 derived rules always yield to S1 and
  always beat S4.
- **CR-2 Inside S1** (PF-6), per (node, group), rank by `(tier: T2 before T3-trusted, specificity desc,
  plugin id, anchor key)`. For computed patches the anchor key is the register identity
  `(instance, anchor, group)`, so two registers never tie; a node handle or `Unique` anchor has maximal
  specificity, a `selector` target its selector's specificity. Equal (tier, specificity) with different
  values from different plugins gives a `patch-tie` diagnostic (both in `explain`); the winner is the
  rank, deterministic.
- **CR-3 Inside S4**: ADR-0024 order `(ConfClass after T3 cap, kind order, source id, ...)`.
- **CR-4 Existence**: `hide` is an S1 effect on the existence register (T2 and trusted T3 only); a
  plugin's `unhide` only retracts its own `hide`; the user's "show hidden" override (S2) wins.
- **CR-5 Categories**: membership is a set union across plugins (no conflict); built-in `Cat` stays a
  pure function of `Semantics` (SM-1). Plugin categories are never visible to selectors (PF-2).
- **CR-6 Keys**: §2.7 KB-1..KB-4.
- **CR-7 Present edits** (unstable): fixed phase/before/after order (ADR-0019), then PR-1.
- **CR-8 No feedback.** W-lane reads and all selectors see S0 only (§2.3.2), plugin facts never enter
  S0 (PF-3), and admission never triggers derivation (PF-4). `FUSE` remains a pure function of
  registers; the published model converges to `Full(S0)` (§2.4.5).

### 2.7 Capabilities, trust and distribution

**Trust tiers** (review M-6; QP-3, QP-4):

| Tier | What | Notes |
|---|---|---|
| **T2 first-party** | built from this repository, files under `/usr/lib/gorca/addons/`, each pinned by an sha256 list compiled into the daemon | hash mismatch = **refused** with a spoken notice and a diagnostic (never demoted to T3); its ids are **reserved** |
| **T3-trusted** | a T3 plugin the user explicitly marked trusted (spoken warning; persisted) | S1 patches and `hide` plain but tainted (PF-5); all T3 key, scope and output rules still apply |
| **T3** third-party | sideloaded `.gorca-addon` (owner Q-A: allowed, spoken warning), off until enabled | patches and `hide` hedged/advisory (§2.6) |
| **T3-dev** | `--ext-dev DIR` | always T3 rules; the trust mark lasts for the session only; reserved ids refused (a first-party plugin under development loads as `dev.<id>` and the installed T2 copy is disabled for the session); package swap resets `level`; verbose diagnostics |

Reserved ids: every id in the compiled first-party list and the `gorca.` prefix. A T3 package using one
is refused at install and at load. Store and signatures are ADR-0020 (Phase 4).

**Application identity and the threat model (r5; normative for activation).**

*Threat model.* A malicious process running as the same user can already present any accessibility
tree it likes. If it impersonates Firefox, a plugin activates on the attacker's own content, and the
attacker could have had that content spoken without any plugin. Identity verification therefore
exists to stop **accidental misattribution and cheap spoofing**; it is **not a defence against a
determined same-user adversary**, and nothing in the design relies on it being one. What a
plugin must never gain from a spoofed identity is access to content of *another* application, which
the scope rules (AC-2, one scope = one application or one document) already guarantee, and a higher
trust tier than the package earned, which the pin list and the tier rules guarantee.

*Evidence.* `app-info.name` and the matcher key are never the application's self-reported name. The
connector establishes the **verified identity** of the bus peer (`AppInfo::exe`):
1. the peer's pid from `org.freedesktop.DBus.GetConnectionCredentials`, re-checked against the process
   start time before and after the reads (pids are reused);
2. **native**: `/proc/<pid>/exe`, canonicalised, a live file (not `(deleted)`) under `/usr` or `/opt`
   whose every path component is owned by root and not writable by anyone else (group-writable only
   for group 0), the same file (device and inode) as the canonical path in gorca's own mount
   namespace, and the process in gorca's mount namespace. The identity is the file name
   (`firefox-esr`, `chromium`); it is trustworthy because only root could have put the file there. A
   copy under `$HOME` or `/tmp`, a deleted binary and a process in a foreign mount namespace are
   unverified whatever they are called;
3. **snap**: only the AppArmor attribute `/proc/<pid>/attr/apparmor/current`, label
   `snap.<name>.<app> (enforce)` (complain mode confines nothing), and an executable under
   `/snap/<name>/` passing the inode check: identity `snap:<name>`;
4. **flatpak**: `.flatpak-info` read through `/proc/<pid>/root` agrees with the systemd scope
   (`app-flatpak-<id>-<n>.scope`), identity `flatpak:<app id>`. This evidence can be forged with a
   user namespace, so a flatpak application is **sandboxed-unverified** (below). *TODO(portal):*
   read the application id from the portal (`GetAppInfo`/`GetHostAppInfo`) to back it.

Comparison with a manifest `[match] apps` entry is **case-sensitive**.

*Consequence.* A scope whose identity is unverified (or sandboxed-unverified) is served **first-party
plugins that are not URL-scoped only**; no third-party plugin and no URL-scoped plugin (a URL match
reads page content) is activated for it. The decision is the plugin service's, taken when the scope
opens; the core only carries `ScopeSpec.verified` (`plugin_port::scope_identity`).

*URL patterns* (`[match] urls`) are matched structurally (`gorca-plugin-host::urlmatch`), never as
flat strings: scheme (http and https only, exact), host (lower-cased, IDN as punycode; a wildcard is
a leading `*.` that matches whole labels and never the bare suffix; `*` anywhere else in a host is a
manifest error), port (the scheme default on both sides), path (`*` never crosses `?` or `#`); a
pattern with no query or fragment ignores the URL's. Userinfo in a pattern is refused and a URL with
userinfo never matches. `https://web.telegram.org*` and `https://web.telegram.org.evil.com/` are
the cases this exists for.

*Logging.* No text a plugin produced and no text the host hands a plugin reaches any log: facts log
their kind and length, a plugin's `diag.log` line logs only its length (tests pin it).

**System scopes (normative; review H-1).** Untrusted and trusted T3 plugins can never match, read,
target, bind keys in or speak over these scopes; T2 plugins may (P-4 needs the Shell):

- **SS-1 Authentication**: the polkit agent dialog, keyring and GCR prompts, network secret prompts,
  the unlock and login screens (Shell: `refs/gnome-shell/js/ui/components/polkitAgent.js`,
  `keyring.js`, `networkAgent.js`, `js/ui/unlockDialog.js`; out-of-Shell prompters by verified app id).
- **SS-2 Shell system dialogs**: any Shell modal dialog (`js/ui/modalDialog.js` subclasses: end
  session, close/not-responding, mount operation, ...).
- **SS-3 Secret nodes**: password and other secret nodes and their subtrees (AC-1), and while focus is
  on one, no T3 code runs on the focus slice of that scope.
- **SS-4 gorca's own UI**: settings, chooser, dialogs and notices of the gorca process.

The classification is data in a first-party K0 package `system-scopes` (verified identity + window
role/modality matchers, backend-neutral; the only place app names are allowed besides plugins). It is
always on, loaded in safe mode, cannot be disabled by the user, and **fails closed**: if it fails to
load, no T3 plugin runs.

**Keys (normative; review H-2).**

- **KB-1** Reserved chords (ADR-0019 I-7) are unbindable by anyone.
- **KB-2** T3 (all T3 tiers) may bind **only chords that include the gorca modifier**. Unmodified keys,
  Shift/Ctrl/Alt-only chords, printable keys, Enter, Space, Tab and arrows are unbindable for T3 in
  every mode. Violations are load errors (the binding is dropped, spoken once in input help).
- **KB-3** While focus is in an editable or secret node, **no T3 binding is active** (the key goes to
  the core or the application as if the plugin did not exist). While focus is on a secret node no
  plugin binding of any tier is active. T2 bindings in a non-secret editable must include the gorca
  modifier (Signal composer: "read last message" stays possible).
- **KB-4** Within a scope, first-party binds before third-party, then plugin id order; a shadowed
  binding is a load diagnostic, spoken once in input help. `commands.run` receives the command id
  only, never key text.

**Output by T3 (normative; review M-1).** Output a T3 plugin produces outside a command it owns
(`reactor.tick`, deliveries of its own requests not issued by a command, `settings-changed`) is
attributed: prefixed by the add-on's name or played after a distinct add-on cue (setting, prefix by
default), rate-limited to 1 utterance per 2 s per plugin (excess dropped and counted), never
`interrupt`, and suppressed while focus is in a system scope. `cue(name)` accepts core cue names and
cue files declared in the plugin's manifest theme only.

**Permissions** (manifest, each with a reason msg id; deny by default; spoken on install/enable):

| Permission | Grants | T2 | T3 |
|---|---|---|---|
| (none) | role, states, counts, structure of nodes reached from the batch; manifest selectors | yes | yes |
| `read-names` | names/descriptions in reads | declared | declared, shown |
| `read-text` | own text, `text`, text requests (message bodies are content) | declared | declared, spoken warning |
| `read-attributes` | `attr` | declared | declared |
| `contribute-facts { groups, layers }` | `facts.*` for listed groups | declared | `patch` effective only when trusted (§2.6) |
| `speak` | `output.*` | declared | declared; attribution rules above |
| `navigate` | `move-cursor`, `request-mode` | declared | declared |
| `act` | `activate`, `focus` | declared | per-plugin user consent |
| `pointer` | `pointer-click` | declared | **never in 0.10** (QP-5) |
| `storage` | state dir quota 1 MiB | declared | declared |
| `net:*`, `clipboard-*`, `observe-typing`, `observe-all-apps` | — | **not in 0.10** | **not in 0.10** |

- **AC-1 Secrets** (ADR-0019 I-5, ADR-0002 X-1..X-11): secret nodes and their subtrees are absent from
  every read; facts targeting them are rejected (FS-8); see SS-3 and KB-3.
- **AC-2 Scope**: a handle of another scope or instance reads `none` (§2.3.3); a K2 plugin sees one
  document only.
- **AC-3 Acting needs a user**: `actions.*` and `settings.set` are applied only while a command of this
  plugin is in flight, or in a `reactor.deliver` of a request issued by that command in the same
  epoch. `derive`, timers and other deliveries can never act or change settings. Every action
  re-validates the target at apply time and the ADR-0024 §2.3.3 "act on what was spoken" check.
- **AC-4 Pointer**: `pointer-click` = gorca's ADR-0025 click on the node's centre through the helper.
  It requires all of: permission `pointer` (T2 only), the helper's allow-list `pointer` cap for gorca
  (AL-1), the user's pointer setting on, AC-3, ≤ 2 clicks/s. Plugins never talk to the helper.
- **Safe mode** (M11; QP-2): no plugin code runs. First-party K0 **profile rows** stay on (consistent
  with ADR-0016 M-5, so VS Code never gets a buffer by accident) and `system-scopes` stays loaded;
  declarative patches, categories, keymaps and all third-party data are off.
- **Hot reload**: a changed package is staged (validated, precompiled off-lane, pre-loaded) and
  swapped at a switch point (no call in flight for that plugin; ADR-0019 §2.5); its slots go to
  `Unloaded`; its source epoch bumps, so its old facts are masked and re-derived (background, focus
  chain first). Dev mode watches the directory and reloads on change.

### 2.8 Migration

Rule: a plugin replaces a core row or switch only after a **shadow run** (plugin facts admitted but
masked, `explain` diff against the old path; 0 diffs on the fixture suite), then the core row is deleted
and an xtask lint forbids application names in `crates/gorca-core` and `crates/gorca-config/product`
(allow-list empty after M18a-7; `system-scopes` is a first-party add-on package, not core).

| # | Plugin | Kind | Moves | First version |
|---|---|---|---|---|
| P-0 | `system-scopes` | K0 (T2, always on) | — (new) | SS-1..SS-4 matchers |
| P-1 | `vscode` | K0 | `web-apps.toml` `vscode` row (verbatim, ADR-0024 PS-3) | profile row `browse.enabled = false`; later: status-bar line/col command (NVDA `code.py:101` `getStatusBarText`) |
| P-2 | `signal-desktop` | K1 | the planned Signal row (decisions 2026-10-19, item 3) | row `browse.mode_default = focus`; `[[watch]]` message rows → computed S1 name "author, text, time" only where generic S3 is wrong (uncached text via W-lane `text` requests, §2.4.4); category `message`; commands next/previous message, jump to composer. Generic fixes (S3 recursion, grid pass-through) stay in core |
| P-3 | `telegram-web` | K2 (one package, two site profiles `/a/*`, `/k/*`; QP-1) | the 98cb234 demo (re-based on facts), the Telegram A study | chat row S1 name "Alice, 3 unread, last message"; categories `chat`, `unread chat`; `live` patch on the message list; commands next/previous unread; `route-changed` resets state |
| P-4 | `gnome-shell` (M18a-7) | K1 | `builtin_ext.rs` `AppProfile` Shell switches → manifest event-policy data (`speak_selection`, `watch_showing`); policy → facts (`valuetext` for the OSD, `live` for dialogs) + `present.edit` for silence | after `present-edit` stabilises |

### 2.9 Milestones (M18a slices)

Gate before M18a-0: **M22b1 merged** (presenter reads only fused entries) and the registries frozen.

| Slice | Content | Acceptance | Benchmarks |
|---|---|---|---|
| **M18a-0** | **C-3 bake-off**: measures A against B' to **confirm B'** (§5, confirmation rule); WQ-1..WQ-3 and WQ-6 measured; H-3 probes (PX-4 adversarial set); κ_int/κ_bg table per core class; lazy-parameter bindgen check (§2.5.3) | reviewer verdict on the measured numbers; budgets B-1..B-9 frozen in the exit report; decision row | PX-4, PX-6 (on the bake-off fixture), WQ-1..3 |
| M18a-1 | WIT 0.10 freeze from `wit/gorca-ext-0.10/` into `gorca-ext-api` (beside 0.3), generated registries, golden empty components | host + guest bindgen compile; registry equality test (L-1); a component built for 0.10.0 links against 0.10.1 with an added import | — |
| M18a-2 | Runtime: pre-load, pool, `.cwasm` cache (keyed by `precompile_compatibility_hash`), W/R slots, plugin actor + `WLaneRunner` (B', §2.4.6; inline runner as fallback), `Slot`/`SlotState` table, fuel grants + import pre-charging + watchdog, wall accounting + overrun strikes, size caps, handle MAC, limiter, `SourceState` link | exhaustive `SlotState` tests (every cell of §2.5.4, plus the stale-reply and busy cells of §2.4.6); PX-4 full adversarial set A1..A13 passes its gates | PX-1..PX-5 |
| M18a-3 | K0: manifest v2, add-on profile rows (L3/L4), selectors + declarative patches (tier → S1/S4), categories, watch prefilter, KB-1..KB-4 load checks; **P-0 system-scopes**, **P-1 vscode**; `web-apps.toml` deleted | vscode parity e2e; safe-mode row test; T3 patch hedged test; SS denial tests (a T3 matcher on the polkit and Shell dialog fixtures matches nothing) | XP-8 (≤ 100 ns/node), B-FUSE with 100 patches unchanged |
| M18a-4 | W lane: `rules`, `facts`, revision token and stamps (§2.6.1), trigger rule, W-lane requests (§2.4.4), PF-5 taint bit, progressive filling, admission via `gorca-fact-api`, masking on fault; **P-2 signal-desktop** (shadow → switch) | Signal private-session e2e: message rows read author/text/time at first focus; F-test with two plugins whose patches would match each other's selectors (H-4): no oscillation; self-trigger test: 1 call per real change; PF-1 differential test | PX-6, PX-7 |
| M18a-5 | R lane: `commands`, `output` (content utterances through render, T3 attribution), `actions` (AC-3/AC-4), `settings`, `timers`, `requests`/`reactor`, `site` world + `route-changed`; **P-3 telegram-web** A and K. **Gate: M22b1 + M22b2 + M22b3 merged** (browse, quick nav and review on `Semantics`; review M-5) | M8-TG/M8-TGL re-based on mock A and K pages; capability denial tests per permission and per KB rule (a T3 keymap binding `a` or `Enter` is refused) | PX-8 |
| M18a-6 | `present-edit` (stable once `PresIr` frozen), spoken permission review, sideload path, manifest/package fuzzing | fuzz 0 host crashes (XP-7); e2e: enabling a T3 plugin speaks every requested permission with its reason and requires explicit confirmation; trust marking speaks the warning; declining leaves it off | XP-2 |
| M18a-7 | P-4 gnome-shell port, `builtin_ext.rs` deleted, app-name lint | M4b Shell e2e unchanged | XP-1 |

**Status of the slices (r5, 2026-10-25).** M18a-1 (runtime foundation) is merged. What this revision
implements of M18a-2..M18a-5, for the document actor (K2) and the app actor (K1) alike:
`[plugins] enabled` (default off), package discovery (first-party directory with the compiled-in
pin list, `$XDG_DATA_HOME/gorca/plugins`), a per-scope plugin actor set with writer-owned slots,
`[match]`/`[[watch]]` manifest tables, native watch prefilter, the revision token and the trigger
rule (`gorca-core::plugin_port`), computed S1 patches in `FUSE` (`Ivm::set_computed`) and S4 facts
through the side table, taint and stratum by tier, the deterministic WASI shim, the keypress-path
wait governor, progressive filling, the first-party `telegram-web` plugin. Not implemented: the R
lane (`commands`, `reactor`, `output`, `actions`, `timers`, `settings`), `present-edit`, `attach`
and `removed` calls, `requests`, `matches`/`select`/`prop`/`anchor-of`/`prov` (stubs),
`hide`/categories, MQ-3 progressive filling across plugins by EWMA of the p95 (a mean is used),
spoken permission review and the trust UI, `route-changed`.

**Gaps found while implementing (r5).**
1. ADR-0024 lists a *Patch* source class, but the frozen `SourceClass` registry has none and the side
   table consumes Secondary and Inferred only. Computed S1 patches live beside the side table
   (`Ivm::set_computed`, ranked `(taint, plugin id, group)`, cleared per source on mask); they have no
   anchors, TTL or epoch state of their own: validity is the writer's token, masking is
   `clear_computed_source`.
2. Fact values are literals (`prop-value::text`); there is no `msg-ref` for a fact, so a plugin cannot
   localise what it asserts (the Telegram plugin says "unread" in English). Needs `prop-value::msg`.
3. The ADR names no manifest schema for activation. Chosen: `[match] apps/urls` and `[[watch]]
   {id, tag, class, aria-role}`, all native.
4. Neutral attributes `class` and `aria-role` were not in `SrcRecord`; added (document source only).
   The rows plugins read are built from the normalised record (after the normaliser, before S-1
   quirks), an approximation of "S0 after quirks".
5. The revision token collapses the six read classes into one stamp per node (more false rejects,
   never a false accept).
6. The wall-overrun rule and the host watchdog are defined against the grant. Under progressive
   filling a crowded share (23 us of fuel for ten plugins) made every healthy 90 us call an overrun and
   demoted the plugin for 60 s. `FuelGrant::wall` now carries the basis (the per-call cap for an
   interactive share). Out of fuel on a crowded interactive share is not a strike and goes to the
   background slice (section 2.4.2 step 4), as written, but the slot table had no cell for it.
7. A busy actor must not be skipped (bake-off finding, `busy_skips = 72 594`): the writer sends the
   newest job anyway, it displaces the queued one, and the displaced job's nodes are re-queued.
   Section 2.5.4 "Call needed while busy = coalesce" is realised that way, not as a skip.
8. One wait per keypress path is realised as a time window (2 ms) on a process-wide governor plus the
   writer's own once-per-20-ms limit; the input layer has no keypress identity to key it on.
9. The document URL was missing in the core. The document source reads it from
   `Document.GetAttributes` (`DocURL` in Gecko, `URI` in Blink) and reports it as a `RouteChanged`
   hint after every `Reset`; a route change inside a document does not re-scope yet.
10. A role patch reached only the entry. The translator now derives the article category flag from the
    fused role (and patches it when a later fact changes the role), and the state-change overlay no
    longer takes back a name whose provenance is Patch or User (it spoke the raw name again).
11. PF-1 is enforced at run time by determinism of the environment (fixed per-call random stream, clocks
    read 0, no `now-ms`) and the read-only snapshot; the differential re-run for user-trusted third
    parties is not built because no trust marking exists yet (`TrustedThirdParty` cannot occur).
12. First-party status needs a pin list compiled into the daemon: `plugins/pins.txt` is regenerated by
    `tools/build-plugins.sh` and must be committed with the plugin; the daemon is rebuilt after it.

### 2.10 Metrics to validate

| Id | Metric | Gate |
|---|---|---|
| PX-1 | pre-load of a precompiled plugin (deserialize from buffer + `InstancePre`) | p99 ≤ 1 ms, never on a writer turn or the Reader |
| PX-2 | trap → rebuilt instance | p99 ≤ 50 µs (spike 9–22 µs) |
| PX-3 | memory per idle instance pair | ≤ 64 KiB + guest state; 50 plugins × 2 lanes ≤ 120 MB RSS worst case |
| PX-4 | fault injection, adversarial set (below), arm chosen in M18a-0 | compute-bound runaway: writer turn delayed ≤ grant + 50 µs p99. Memory/lifting adversaries (A1–A5): every overrun recorded; plugin demoted after the first overrun and `Backoff` after 3 in 60 s; writer stall max per occurrence reported and ≤ the M18a-0 frozen bound. A6: Σ ≤ B + n·c_max at ρ ≈ 1. A7–A13: the stated rule holds |
| PX-5 | fuel-only vs fuel + epoch on the plugin corpus | fuel-only ≥ 20 % faster on compute-bound hooks, no escape in fault injection that epoch would have caught |
| PX-6 | keypress → speech p99 with P-2 + 9 dummy matching plugins (Signal fixture) | Δp99 ≤ 0.3 ms vs no plugins; facts at first utterance ≥ 99 % of focus moves **overall and ≥ 95 % for every plugin** (review M-4); under B' the benign fixture on a quiet host requires ≥ 99 % overall and ≥ 97 % per plugin (r4) |
| PX-7 | 10k ev/s storm (50k for L3-50k), 3 plugins watching 50 % of nodes | ingress lag unchanged; per-event fusion+plugin p99 ≤ 10 µs; drops counted, writer never blocked |
| PX-8 | quick nav by plugin category on 3000-chat Telegram mock | ≤ 5 ms key → speech (W1), no guest call per key |
| PX-9 | revision-token bookkeeping | stamp update ≤ 20 ns per changed class; admission check ≤ 1 µs for \|R\| = 256; 0 false accepts on the MQ-2 replay corpus |
| XP-1, XP-2, XP-5, XP-7, XP-8 | as ADR-0019 §5 (XP-2 cap now 0.3 ms) | — |

PX-4 adversarial set (`ext-test`; review §4, H-1..H-5, M-1, M-3, math C-3):
A1 fresh-page 4 KiB stride over the memory cap; A2 pointer chase over 16 MiB; A3 a 16 MiB
data-segment string passed to `diag.log`, `facts.assert` text, `settings.set` words and `attr` key;
A4 a 100k-element `present.edit` result; A5 cold `.cwasm` (evicted with `posix_fadvise(DONTNEED)` on
the cache file in a private session; **never** a global `drop_caches` on the live host) to prove no
file-backed fault reaches a writer turn; A6 N = 10 plugins each spending its full interactive grant;
A7 two plugins whose patches would match each other's selectors; A8 a W-lane `refresh` on every
`derive`; A9 a T3 keymap binding letters, Enter and arrows; A10 forged and foreign handles and
anchors; A11 a T3 timer saying "Session locked, enter your password"; A12 a plugin that asserts on
its own target every call (self-trigger); A13 a loop of `subtree(max)` imports.

## 3. Alternatives considered

| Alternative | Why not |
|---|---|
| A. Keep ADR-0019's `mirror` + `present`/`policy` verdicts as the main path | builds on raw mirror data and legacy speech; fixes one surface at a time; owner rule forbids it |
| B. Fact rules as async `FactPort` producers via `spawn_blocking` | a hop costs 5–14 µs p50 / 0.19 ms p99 and the first utterance misses the fact; superseded as a bake-off arm by **B'** (dedicated plugin actor, bounded wait, §2.4.6), which r3 makes the **design target** (owner, 2026-10-24) |
| C. One instance per plugin, shared by all actors (behind a mutex) | contention between app actors and the Reader; one slow call stalls every app; violates "one actor per application" isolation |
| D. W-lane reads or selectors on fused entries | feedback loops between plugins, order-dependent results, oscillation (math C-2); breaks ADR-0022 A4 |
| E. Fuel + epoch (current `gorca-ext-host`) | 34–36 % compute penalty; epoch granularity coarser than our budgets; it checks at the same points as fuel and does not catch faults, misses or lifting; wall accounting does the useful part (C-2) |
| F. Third-party patches or `hide` at S1 by default | a sideloaded plugin could rename "Cancel" to "OK" plainly or hide a control (UI redress, review H-1); hedged S4 and advisory `hide` unless trusted |
| G. Priorities declared by plugins | arms race, nondeterminism by convention; static rank (tier, specificity, id, anchor key) instead |
| H. Native shared-object plugins | no isolation; first-party native stays only for the legacy 0.3 fallback until ported |
| I. Fixed plugin-id order or equal split for the interactive budget (r1) | fixed order starves later ids (28 % of cheap plugins at n = 16); equal split wastes cheap plugins' shares (69.4 % completion vs 87.7 %); progressive filling (§2.4.2) |
| J. Deficit round robin / budget carry-over across focus changes | breaks the per-focus latency bound; the per-turn floor already gives every plugin its fair share (math §4.4) |
| K. `(sem_rev, vis_rev, BindGen)` basis (r1) | `sem_rev` rejects every fresh fact, `vis_rev` accepts stale ones (§2.6.1) |
| L. T3 unmodified keys allowed in browse mode (review H-2 option) | browse/focus switches are automatic, so a mode flip would turn a browse binding into a typed-key tap; single-letter quick nav belongs to the core, and plugin categories are reachable through core quick nav (PX-8) |
| M. `resource` handles instead of MAC handles | a drop protocol across two lanes and one more boundary type; the MAC gives unforgeability at u64 cost |

## 4. Consequences

- The core gains: the selector engine and watch prefilter in the writers, the W/R slot runtime with
  wall accounting, per-(node, class) stamps and the reverse token index, the "winner tainted" bit in
  S3, the plugin fact source wired to `gorca-fact-api`, per-instance handle MACs, registry generation,
  the `system-scopes` first-party package.
- `gorca-ext-host` runs two engines during the transition (0.3 legacy fuel + epoch; 0.10 fuel-only).
- App names disappear from core code and product rows (lint).
- Plugin authors write against a small, typed, backend-neutral API; behaviour is explainable
  (`explain` names plugin, layer, tier, binding, basis, taint).
- Costs: a writer turn waits up to 0.3 ms for plugins at focus (the shared deadline under B'; under the
  fallback arm A it computes for up to that long, ρ-dependent until demotion); the first utterance may
  lack a plugin's facts when the plugin loses the race (accepted, ≥ 97 % gate) and the refine arrives
  by LB-4; a plugin thread pool and per-job `FrozenRows` snapshots exist; the W/R split forbids shared plugin memory; a plugin needing private-state-dependent output
  can only produce S4; T3 plugins have a narrower key space (gorca-modifier chords only) and attributed
  unprompted speech.
- ADR-0019 §2.2, the `mirror`/`effects` sketch, I-1 and C-1 are superseded here (ADR-0019 points to
  this ADR). ADR-0024 §7 F12 gets the plugin clause of §2.6.2 and ADR-0024 §2.1's "Secondary for
  reviewed add-ons" row excludes WASM plugins (PF-3), both applied when this ADR is accepted.

## 5. Contested choices

- **C-2 fuel-only engine + wall accounting vs fuel + epoch.** Decided fuel-only on the engine plus wall
  accounting (§2.5.2), medium-high confidence; validated by PX-4/PX-5. No bake-off unless PX-4 fails.
- **C-3 plugin placement: dedicated plugin actor with a bounded wait (B') vs synchronous `derive` in the
  owner actor (A). Decided by the owner on 2026-10-24: B' is the design target; A is the fallback.**
  The bake-off (slice **M18a-0**, the gate for every later slice) still runs both arms, but its job is
  now to **confirm that B' meets the budgets**, not to choose between equals.
  - **Arm B' (target)**: §2.4.6. The writer hands the interactive batch to the plugin actors with a
    frozen view and waits on the shared deadline `D` = 0.3 ms wall; on timeout it publishes without the
    facts and admits the late facts under the PF-7 token check (refine via LB-4). Cost: one cross-thread
    wake (about 5-20 µs, to be measured) and the snapshot.
  - **Arm A (fallback, kept as a measured reference)**: `derive` inside the writer turn, with all of
    §2.5 (progressive filling, κ_int, wall accounting, size caps, pre-load).
  - Fixture: Signal private session, P-2 + 9 benign dummies, then the PX-4 adversarial set A1..A6 and
    A13 added one at a time and all together. Host conditions per `bench/b1/quiet-host.sh`, labelled.
  - Criteria (reviewer judges on the measured numbers): (1) writer-turn stall per focus change, p99
    and max, benign and adversarial; (2) facts at first utterance, overall and per plugin; (3) Δp99
    keypress → speech vs no plugins; (4) PX-7 background throughput; (5) writer-integration code size
    and `unsafe` count; (6) **B' only**: reply delay distribution (queued, exec), `pa_busy` rate,
    pool-size sensitivity (T = 2, 4, cores - 2), and the arrival-order test of PF-6.
  - **Confirmation rule (r4).** B' is **confirmed** iff: stall, **measured from the start of the writer
    turn**, max ≤ 0.35 ms (quiet-host gate; loaded-host max is reported separately, it is dominated by
    preemption) and p99 ≤ 0.31 ms per focus change (benign and adversarial, with demotion active, timer
    slack 1 ns), at most one wait per keypress path, benign Δp99 ≤ 0.3 ms, facts at first utterance on
    the **benign** fixture (P-2 + dummies, quiet host) ≥ 99 % overall and ≥ 97 % for every plugin (on the
    adversarial mix ≥ 97 % overall and ≥ 90 % per plugin), PX-7 ingress lag unchanged, and 0 false
    accepts in the randomized-delay MQ-2 replay. A's numbers are reported for the record and **do not by
    themselves overturn B'** (A being faster on a gate B' also passes is not a reason to switch).
  - **If B' is not confirmed** the reviewer reports which gate failed and why; the fallback needs a new
    owner decision row. Candidates, in order: (i) tune B' (pool size, spin time, grant rule MQ-4) and
    rerun the failed gate; (ii) arm A if it passes its former rule (adversarial stall p99 ≤ 0.45 ms and
    max ≤ 1 ms with demotion, benign Δp99 ≤ 0.3 ms, facts at first utterance ≥ 99 %); (iii) arm C: A
    for T2 (first-party) and B' for T3.
  - Budget numbers (B-1..B-9, κ table) are frozen in the M18a-0 exit report, after WQ-1..WQ-3.

## 6. Open questions

Owner decisions, **CONFIRMED by the owner on 2026-10-24 with the defaults unchanged**
(`docs/decisions.md`, row of that date):
- **QP-1 (CONFIRMED)** One `telegram-web` plugin with two site profiles (A, K): yes.
- **QP-2 (CONFIRMED)** Safe mode keeps first-party data rows (profile rows) but runs no plugin code: yes.
- **QP-3 (CONFIRMED)** Third-party facts, declarative patches and `hide` are hedged/advisory until the
  user marks the plugin trusted (spoken warning), which allows S1: yes.
- **QP-4 (CONFIRMED)** First-party plugins are files under `/usr/lib/gorca/addons/` pinned by an sha256
  list compiled into the daemon: yes.
- **QP-5 (CONFIRMED)** `pointer-click` first-party only: yes.

Math: MQ-1..MQ-3 answered (`docs/reports/plugin-fusion-math.md`), adopted as §2.4.1, §2.4.2, §2.6.1,
§2.6.2. New in r3: **MQ-4**, the tightest provable grant rule when the n interactive jobs run in
parallel on T pool threads under one shared deadline (serial-equivalent progressive filling is the
safe default until it is proved; §2.4.6). Remaining: ρ across shapes and cores is unmeasured (WQ-2); the 6-class partition must match
the final `read-*` permissions; relations and positions are assumed to behave like the S3 name edge.

WASM engineer (M18a-0 unless noted):
- **WQ-1** `derive` with a 64-node `view-node` batch (names ≤ 64 B): call cost p50/p99.
- **WQ-2** Fuel calibration across guest shapes (ALU, string-heavy, call-heavy, pointer chase over the
  maximum memory, fresh-page stride) and P/E cores: the κ_int/κ_bg table.
- **WQ-3** Cost of import pre-charging (`get_fuel`/`set_fuel` per call), the 64th-import watchdog,
  `Instant::now` pairs and handle MAC verification.
- **WQ-4** (M18a-2) `memory_growing` denial: clean trap path and rebuild cost with 16 MiB touched.
- **WQ-5** (M18a-2) RSS of a W/R instance pair of one component (shared code); resident code after
  buffer deserialize under memory pressure; whether `mlock` is needed.
- **WQ-6** (M18a-0, r3/r4) B' mechanics: wake latency p50/p99/max of the writer → plugin actor → reply
  round trip (futex vs `timerfd`/`AsyncFd`, spin vs park, pool size T, P/E cores, loaded host); late
  wakeups with `PR_SET_TIMERSLACK` 1 ns versus the default; cost of building `FrozenRows` per focus
  change (eager only; the lazy variant is rejected in r4); the parallel grant rule per MQ-4.

## 7. Invariants (summary)

- I-P1 No W-lane read, selector or admission sees any S1..S4 value, hide, flatten or plugin category.
- I-P2 Plugin facts never enter S0; admission never triggers `derive`.
- I-P3 Every guest call has a fuel grant and a wall measurement; no file read, deserialize or compile
  runs on a writer turn.
- I-P8 (r3, r4) The only blocking call a writer makes for a plugin is the interactive wait (atomic
  countdown + futex, timer slack 1 ns, never a tokio blocking receive), bounded by the shared deadline
  `D` = B-2 = 0.3 ms from the start of the writer turn, at most once per keypress path; plugin results are messages; a reply is admitted only after the
  token check and only if its `slot_epoch` is current; a plugin actor never touches writer state.
- I-P4 No host import copies a guest string or list before checking its cap.
- I-P5 No T3 output is plain author intent (S1) or unattributed unprompted speech unless trusted /
  inside its command; no T3 code touches a system scope; no T3 binding exists without the gorca
  modifier or is active in an editable or secret node.
- I-P6 Handles and anchors are host-minted and verified; the basis is host-side.
- I-P7 Slot state transitions are exactly §2.5.4; every cell is tested.

## 8. Changelog

**r4 (2026-10-24)** — applies the r3 review (`docs/reports/adr-0026-review.md`, section r3). No change to
the fact model or the placement decision.

| Finding | Change |
|---|---|
| R3-1 timer slack | `prctl(PR_SET_TIMERSLACK, 1)` on writer and plugin-pool threads; late wakeups measured in M18a-0 (WQ-6); gate p99 <= 0.31 ms stated with the slack setting |
| R3-2 deadline basis | `D = t0 + 0.3 ms` from the start of the writer turn; snapshot, send and fuse are inside it; PF-8 row and confirmation rule say "from the start of the writer turn"; lazy snapshot rejected |
| R3-3 wait primitive | atomic countdown + futex (or `timerfd` via `AsyncFd`, measured); never `blocking_recv` or `tokio::time`; replies still via the mailbox; I-P8 updated |
| R3-4 cascade | one bounded wait per keypress path (K1 and K2 share the first writer's deadline); flapping-focus cap of one interactive wait per N ms |
| R3-5 gates | benign fixture: facts at first utterance >= 99 % overall and >= 97 % per plugin (PX-6 and the confirmation rule); 97/90 only for the adversarial mix; interactive jobs pre-empt background jobs; max is a quiet-host gate, loaded max reported |

**r5 (2026-10-25)** — implementation notes of the first wiring of the runtime into the fusion writers
(section 2.9 status and gaps, section 2.7 identity and threat model, URL matching, logging rule). No
change to the fact model, the lanes or the call model; B' is as owner-decided.

**r3 (2026-10-24)** — owner decisions of 2026-10-24 (`docs/decisions.md`). No change to the fact model;
one runtime-placement change and one confirmation.

| Source | Change |
|---|---|
| Owner: canonical actor model (C-3) | plugins are actors with mailboxes; **B' is the design target**, synchronous `derive` in the owner actor (A) is the fallback. New §2.4.6 (messages, ownership, plugin thread pool, shared deadline `D` = 0.3 ms, backpressure, cancel, actor-lost fault, slot machine delta with `slot_epoch`, `WLaneRunner` trait). Also the §2.4 diagram note and Instances bullet, §2.5.1 budget fit, §2.5.2, §3 alt. B, §4 costs, §2.9 M18a-0/M18a-2 rows, invariant I-P8 |
| C-3 / §5 | bake-off kept, goal changed to **confirm B'**: confirmation rule with numeric gates, A reported for the record, fallback order (tune B', A, arm C) needing a new owner row; criterion (6), WQ-6 and MQ-4 added |
| PF-1..PF-8 re-check | all eight kept verbatim and verified under asynchronous delivery (table in §2.4.6); the MQ-2 token already fixes the basis at call time, so late replies need no new staleness mechanism; PF-8 uses the serial-equivalent grants until MQ-4; a test per rule |
| QP-1..QP-5 | **CONFIRMED by the owner, defaults unchanged** (§6) |
| WIT | **no change to any interface or type** (`gorca:ext@0.10.0` stays). The guest cannot tell whether `derive` runs inline or on an actor; the frozen view is host-side. Only the lane comment in `plugin.wit` was reworded |
| `.cwasm` cache | key is now `Engine::precompile_compatibility_hash()` (covers triple, compiler flags, ISA flags, features, tunables, module version; read in `refs/wasmtime/crates/wasmtime/src/compile/code_builder.rs`); see ADR-0027 |

**r2 (2026-10-23)** — folds in `docs/reports/adr-0026-review.md` and `docs/reports/plugin-fusion-math.md`.

| Source | Change |
|---|---|
| H-1 | T3 declarative and computed patches → S4 hedged, T3 `hide` advisory, unless user-trusted; system scopes SS-1..SS-4 with fail-closed `system-scopes` package (§2.6, §2.7) |
| H-2 | KB-1..KB-4: T3 gorca-modifier chords only; no T3 binding in editable/secret focus; no binding of any tier on secret focus (§2.7) |
| H-3 | wall accounting + overrun strikes, adaptive κ_p, per-call fuel = min(cap, remaining), import pre-charging, size caps before copy, `attach`/`removed` budgets and eager attach, `.cwasm` pre-load from a buffer, PX-4 adversarial set A1..A13 (§2.4, §2.5, §2.10) |
| H-4, PF-1, PF-2 | §2.3.2 lane read rules: selectors, structure and existence on S0 everywhere; plugins never see other plugins' patches |
| H-5, PF-4 | trigger rule (§2.4.1); W-lane requests as observation fillers, one per (node, kind, stamp); replies become facts only via the observed view (§2.4.4) |
| PF-3, PF-5..PF-8, MQ-2, MQ-3 | §2.6.1 revision token `(t, R, M, g)` replaces `sem_rev`/`vis_rev`; §2.6.2 normative PF list and F12 clause; CR-2 total rank; §2.4.2 progressive filling with rotating ties |
| fuel unit | 21,739 units/µs (not 22) everywhere; budgets stated in wall time, fuel derived from calibration |
| M-1 | T3 unprompted output attributed, 1 per 2 s, never interrupt, cue restrictions |
| M-2 | facts ≤ 256 per call (≥ batch), over-cap call rejected atomically; WIT comments aligned |
| M-3 | MAC'd per-instance handles, host-minted anchors, no guest basis (§2.3.3) |
| M-4 | per-plugin first-utterance gate in PX-6 |
| M-5 | M18a-5 gated on M22b1 + M22b2 + M22b3; acceptance for spoken permission review in M18a-6; M18a-2 names the probes |
| M-6 | reserved ids, T3-dev tier, hash mismatch = refuse (§2.7) |
| C-3 (review §3) | not decided; bake-off A vs B' as slice M18a-0 with a decision rule (§5) |
| L-1 | `Slot` record + complete `SlotState` table |
| L-2 | settings change bumps source epoch; no clock in the W lane |
| L-3 | `aria-role` replaces `xml-roles`; neutral cause codes; neutral `rect`; `app-info.name` defined; one scope per application across connectors |
| L-4 | §2.3.2 now exists; accessor cost corrected; alt. B replaced by B' |
| QP-1..QP-5 | orchestrator's recommended defaults recorded, pending owner confirmation |
| WIT | `wit/gorca-ext-0.9/` → `wit/gorca-ext-0.10/` (`gorca:ext@0.10.0`): `snapshot-token` and all `basis`/`token` parameters removed, `anchor-ref` + `model.anchor-of`, `fact-target` = node/anchor/selector, `diag.now-ms -> option<u64>`, caps and lane rules in comments, neutral cause codes |

Not adopted from the inputs, with reasons: the review's "unmodified keys in browse mode only" option
for T3 (alt. L); deficit round robin (alt. J); the H-3 "strike on 2× wall" as a single-event strike
(one overrun demotes, three make a fault, so OS preemption alone does not disable a plugin); the
review's "forbid `requests` in the W lane" option (the observation-filler form keeps Signal's
uncached text possible).

**r1 (2026-10-23)** — initial proposal.
