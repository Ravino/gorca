# ADR-0026: WASM plugin model on the fused semantic model (M18a)

Status: **Proposed** (r1, 2026-10-23). Owner: architect.
Refines: ADR-0019 (extension platform: worlds, effects buffer, lifecycle, versioning V-1..V-8,
permissions). ADR-0019 stays the umbrella for packaging, lifecycle states and versioning; this ADR
replaces its §2.2 world table, its `mirror`/`effects` interface sketch, its I-1 budget and its
C-1 question with a contract built on the fused model. Builds on: ADR-0022 r2/r3 (registers, `FUSE`,
SM-8, `PresIr`, §2.9 extension text paths, §2.10 order), ADR-0024 r3 (fact sources, admission,
`SourceState`, grants, taint, LB-1..LB-5, FS-1..FS-12), ADR-0016 r3.1 (profile layers L3/L4, PF-1..PF-5),
ADR-0001 (actors, R3: results as messages), ADR-0025 (helper `pointer` capability).
Draft WIT (paper only, parsed with wit-parser 0.261, not built): `wit/gorca-ext-0.9/`.
Numbers: `docs/reports/wasm-plugin-cost-spike.md` (wasmtime 49.0.3, loaded host),
`docs/reports/wasm-hook-overhead.md`, `docs/reports/ext-telegram-web-demo.md` (branch 98cb234).
Milestone: M18a (slices §2.10). Order fixed by the owner: **M22b1 → M18a runtime → first plugins**.

## 1. Context

The owner's rule: the core stays generic; everything app-specific lives in plugins (WASM components),
so core special cases go away. Today they live in three places:

| Where | What | Status |
|---|---|---|
| `crates/gorca-config/product/web-apps.toml` | `vscode` row: `browse.enabled = false` (NVDA does the same, `refs/nvda/source/appModules/code.py:23-30` `VSCodeDocument` blocks the tree interceptor) | L4 product row, compiled in, applies in safe mode (ADR-0016 M-5) |
| `crates/gorca-core/src/builtin_ext.rs` | `AppProfile::for_app`: Shell-only switches `speak_selection`, `watch_showing`, policy routing | "stand-in for per-app extension hooks until the WASM host is wired" (file header) |
| decisions 2026-10-19 (Signal plan item 3), 2026-10-17 (Telegram demo), 2026-10-23 (Telegram A study) | a `signal-desktop` row (mode default focus), the `ext-telegram-web` demo (WIT 0.3 `web`, unmerged), Telegram A/K polish | planned or on a branch |

The existing contract (`crates/gorca-ext-api/wit/gorca-ext.wit`, 0.3.0) is built on the legacy model:
a closed `role` enum, a `node` record of AT-SPI-shaped `states: u64`, `present -> option<list<segment>>`
and `policy -> say(...)` that bypass presentation, `host.speak`, and a blocking-shaped `fetch`. The
owner forbids building the plugin API on legacy speech paths. ADR-0022 §2.9/§2.10 and ADR-0024 §2.10
already fixed the direction (plugins are fact sources; text paths go through the pipeline; WIT 0.9
reads semantics through accessors); this ADR turns it into a contract.

### 1.1 Prior art and what to avoid

- **NVDA app modules** are Python modules bound by executable name
  (`refs/nvda/source/appModuleHandler.py:96-117` `_getPossibleAppModuleNamesForExecutable`,
  `EXECUTABLE_NAMES_TO_APP_MODS`), one instance per process (`getAppModuleFromProcessID`, :241),
  with `sleepMode` (:494) and overlay classes (`chooseNVDAObjectOverlayClasses`, :646), plus
  `GlobalPlugin` singletons (`refs/nvda/source/globalPluginHandler.py:62-81`). Strengths: binding by
  app, sleep mode, per-object behaviour. Failures: no isolation (an add-on hang freezes NVDA), the
  whole Python namespace is the API so add-ons break every year (`addonAPIVersion.py` `BACK_COMPAT_TO`
  bumped 8 times since 2019, ADR-0019 §1), and app modules mutate objects in place, so "why was this
  said" has no answer. `code.py` itself is mostly data (no buffer) plus one feature (status-bar
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
  `total_core_instances`, :4549 `max_memory_size`), `call_async` (`component/func/typed.rs:167`).

### 1.2 Measured costs (spike, wasmtime 49.0.3, loadavg 9–14, tails pessimistic)

| Quantity | Value |
|---|---|
| compile (Cranelift) | 8 ms warm, 31 ms first in process |
| `.cwasm` deserialize | 0.1–0.2 ms |
| instantiate from `InstancePre` (pooled) | 3–6 µs p50/p99; rebuild after trap 9–22 µs |
| call, trivial | 46–50 ns p50, 51–57 ns p99 |
| call, record of 10 strings (200 B) | 460 / 570 ns |
| call, list of 20 records | 1.83 / 2.13 µs |
| guest → host import | 26 ns |
| `call_async` vs sync | 120–150 ns vs 46 ns |
| `spawn_blocking` hop per call | 5–14 µs p50, 0.14–0.19 ms p99 |
| memory per instance | 14–17.5 KiB + touched state (1 MB state ≈ 1 MiB) |
| core instances per Rust component | ≥ 3 |
| infinite loop → trap | epoch 1 tick (1 ms): 1.1 ms; fuel 3M units: 138 µs (≈ 22 units/µs) |
| hot-loop overhead | fuel 2–6 %, epoch 5–10 %, **fuel + epoch 34–36 %** |
| trap / panic | returned as `Err` in 13–35 µs, host unaffected |

Conclusion of the spike, adopted here: the boundary is negligible for our budgets; **guest compute is
the thing to budget**.

## 2. Decision

### 2.1 Principles

- **PM-1 Plugins contribute facts, not speech.** App knowledge enters the model as facts (ADR-0024),
  through the single `FUSE` in the node's writer (SM-8). Every surface (focus, browse, review, quick
  nav, elements list, braille) then benefits from one fix. A plugin never replaces an utterance.
- **PM-2 Data before code** (ADR-0019 PA-1). Profile rows, selectors, patches, categories, keymaps and
  settings schemas are manifest data evaluated natively. Code runs only for nodes a manifest
  `[[watch]]` selector matched (the core prefilters).
- **PM-3 Stratified reads.** Code that produces facts reads only the **observed view** (S0 after
  quirks); code that presents reads only **published fused entries**. A plugin's facts therefore never
  depend on any plugin's facts (§2.6, no feedback loop).
- **PM-4 Synchronous, bounded, in place.** Fact rules run inside the node writer as a plain
  synchronous call under a fuel budget (no message hop, no `spawn_blocking`), so the facts are in the
  first publish of a focused node. Everything that may need IPC is a request answered by message (R3).
- **PM-5 Backend-agnostic.** No connector type crosses the boundary (FS-1): node handles, registry codes
  and anchors only. The same plugin runs over AT-SPI and Newton/AccessKit.
- **PM-6 Fail open, fail alone.** A fault, overrun or violation costs that plugin's contribution for
  that call; the core presentation goes on. Faults are counted per (plugin, scope).

### 2.2 Plugin kinds, worlds and what each may do

A **plugin** is an add-on package (ADR-0019 §2.8) with a manifest and zero or one component.

| Kind | Component world | Binding | Contributes | Example |
|---|---|---|---|---|
| **K0 data-only** | none | manifest matchers | `[[profile]]` rows (L3/L4), `[[selector]]`, `[[patch]]` (S1), `[[category]]`, `[[keymap]]` to core commands, `[settings]` schema, catalogs | `vscode` |
| **K1 app module** | `app-module` | app matcher (`AppMatcher`, ADR-0016) | K0 + `rules` (facts, S1 computed patches or S4), `commands`, `reactor`, `present` (unstable) | `signal-desktop` |
| **K2 site module** | `site` (= `app-module` + `site-hooks`) | browser app matcher + URL glob (`DocMeta.url`) | as K1, scoped to one document; `route-changed` | `telegram-web` |
| K3 terminal policy | `terminal-policy` (WIT 0.3 `terminal`, unchanged) | terminal apps | ADR-0018; ported later | `ext-vte` |
| K4 global | `global` (ADR-0019) | none | not in M18a (needs a privacy design) | clipboard history |

**Presentation hooks** exist only in the IR-edit form of ADR-0022 §2.9 (`present.edit`: drop, replace,
insert-after, move-before on `PresIr` fields, then PR-1 dedupe and render). They are behind
`@unstable(feature = present-edit)` until M22b1 freezes `PresIr`, and none of the first three plugins
needs them. The 0.3 `present -> segments`, `policy -> say`, `on-event -> rewrite` and `host.speak`
forms do **not** exist in 0.9; the 0.3 world is served by the legacy host until its last user
(`ext-gnome-shell`, `ext-vte`) is ported (§2.9).

**Activation.** A plugin is *enabled* (user/first-party state, ADR-0019 §2.5) and *matched* per scope:
app matcher on `AppInfo` (verified identity first, as `profiles.toml`), for K2 also the document URL
read only when a loaded manifest has a URL glob (demo rule, privacy). Profiles never activate plugins
and plugins never activate profiles (ADR-0019 I-8, ADR-0016 PF-3); a profile may only carry the
plugin's own `ext.<id>.*` settings. NVDA's `sleepMode` maps to manifest `sleep = true` patches /
`browse.enabled = false` rows (data).

**Versioning.** ADR-0019 V-1..V-8 unchanged: `gorca:ext@0.9.0` is the preview, additive within a
major, open registry codes, `@unstable` for experiments, compatibility computed from imports/exports.

### 2.3 WIT 0.9: host interfaces and plugin exports

Draft: `wit/gorca-ext-0.9/{types,host,plugin}.wit`. Summary:

| Interface | Direction | Lanes | Content |
|---|---|---|---|
| `types` | — | — | `node-ref` (u64, stable per scope, never reused), `snapshot-token`, registry codes (`role-code`, `group-code`, `action-code`, `category-code`, `field-code`, `reason-code`), `prop-value` (shape variant: `absent`, `code`, `bits`, `number`, `text`, `position`, `grid`, `area`, `truth`), `view-node` (frozen batch record), `prov-info` (u8 codes), `app-info` |
| `model` | import | W: observed view; R: published fused entries | `role`, `prop(n, group)`, `name`, `states`, `parent`, `children`, `subtree(root, depth ≤ 8, nodes ≤ 256)`, `text` (cache only), `attr` (raw toolkit/web attribute, clipped), `matches`/`select` (manifest selectors, run natively), `prov`, `focus`, `cursor`, `doc-url`, `app` |
| `facts` | import, buffered | W only | `assert(target, group, value, layer, conf, basis, ttl)`, `retract`, `hide`/`unhide`, `category`; `fact-target` = node / dom-id / structural / region / selector (all cases now, ADR-0024 W-1); `layer` = `patch` (S1) or `inferred` (S4) |
| `output` | import, buffered | R only | `say(parts, urgency)` where a part is localised `text` or `node(n, reason)` = "present this node through the core pipeline"; `cue` |
| `actions` | import, buffered | R, command context only (AC-3) | `activate`, `focus`, `move-cursor`, `request-mode`, `pointer-click`, `pass-key` |
| `settings`, `timers`, `requests`, `diag` | import | `set`, `arm` buffered | own namespace; timers ≥ 50 ms, ≤ 16; async `request(query, budget) -> id` (text, children, refresh), ≤ 8 in flight; log, monotonic clock |
| `rules` | export | W | `attach(ctx)`, `derive(ctx, batch ≤ 64 dirty nodes)`, `removed(nodes)` |
| `commands` | export | R (ext worker) | `run(id, cmd-ctx)` |
| `reactor` | export | R (ext worker) | `deliver(id, reply, token)`, `tick`, `settings-changed` |
| `site-hooks` | export | R | `route-changed(url, token)`: resets plugin state, never speaks |
| `present` | export, `@unstable` | R inline (Reader) | `edit(ctx, fields) -> list<ir-edit>` |

Backend independence: `node-ref` is the host's per-scope handle for a `NodeKey`; roles, groups and
actions are `gorca-model` registry codes; `attr` keys are lowercase neutral names (`tag`, `class`,
`id`, `xml-roles`) that each connector maps (AT-SPI attributes; AccessKit `html_tag`, `class_name`,
`author_id`). A plugin that needs `attr` is web- or toolkit-aware by its own choice, never by the API.

**C-1 (ADR-0019) resolved:** handles + accessors for everything that grows, plus one frozen batch record
(`view-node`) for bulk reads, because a list of 20 records costs 1.8 µs and 20 accessor calls cost
≈ 0.5 µs (26 ns each): both are noise; the batch keeps guest code simple and the accessors keep
growth additive. No `resource` type (no drop protocol across lanes).

### 2.4 Lanes, instances and scheduling

```text
            app actor / doc actor (node writer, SM-8)                Reader / ext worker
 events -> coalescer -> S0 registers -> [watch prefilter] -> W instance.derive -> facts buffer
                                         |                              |
                                         +-- admit (gorca-fact-api) <---+  (same writer turn)
                                         v
                                   FUSE S-1..R -> publish SemEntry ---------> R instance: commands,
                                                                               reactor, present.edit
```

- **Instances.** Per (plugin, scope) at most two instances of the same component: **W** (in the writer:
  the app actor for K1, the document actor for K2) and **R** (ext worker; `present.edit` inline on the
  Reader). They share no memory; R learns what W knows only through the model (facts, categories). A
  `Store` is never shared between threads, so there is no lock on any lane. Both are created lazily
  from a precompiled `InstancePre` (3–9 µs, off the key path: W at scope attach, R at first command
  or first `present` match, prewarmed at app focus).
- **Pool.** Pooling allocator: `total_component_instances = 512`, `total_core_instances = 2048`
  (≥ 4 × instances, spike §3), `max_memory_size` 32 MiB virtual per memory. Idle eviction: R after
  10 min without a call, W when its scope closes. Cold load cost per plugin per process ≈ 0.2 ms
  (`.cwasm` cache keyed by package hash + wasmtime version + engine config, ADR-0019 §2.5).
- **Watch prefilter.** The writer evaluates the plugin's `[[watch]]` selectors natively on dirty nodes
  (the selector engine of M18a, ≤ 100 ns/node, ADR-0019 XP-8) and appends matches to the plugin's
  dirty set, coalesced by node (latest cause wins). No match, no call.
- **Interactive slice.** On a focus change, before publishing the focus chain, the writer calls
  `derive(interactive = true)` for watched nodes of the focus chain and active descendant only
  (≤ 16 nodes), under the interactive budget (B-2); their facts are admitted in the same turn, so the
  first utterance already uses them. Everything else is the **background slice**: batches of ≤ 64
  nodes on the writer's background lane, interleaved with ADR-0024 LB-2 slices.
- **Determinism.** Calls are ordered by (plugin id, batch order); effects applied in that order;
  `FUSE` is order-independent anyway (§2.6).

### 2.5 Performance and safety

#### 2.5.1 Budgets (all CPU, measured by fuel; calibration ≈ 22 units/µs to be fixed per host class in M18a-2)

| Id | Path | Per call | Aggregate | On overrun |
|---|---|---|---|---|
| B-1 | `derive` background | 1 ms | writer background share (ADR-0024 LB-2), batch ≤ 64 | call aborted, effects dropped, strike, batch requeued once halved, then the nodes are skipped for 10 s |
| B-2 | `derive` interactive (focus chain) | 0.2 ms | **0.3 ms per focus change across all plugins** | remaining plugins skipped for this turn (fail-open), refine via LB-4, strike for the overrunner |
| B-3 | `present.edit` (unstable) | 0.1 ms | **0.3 ms per utterance** (replaces ADR-0019 I-1's 2/3 ms) | fail-open, strike |
| B-4 | `commands.run` | 20 ms | one in flight per plugin | effects dropped, strike, "add-on did not respond" cue |
| B-5 | `reactor.*`, `route-changed` | 5 ms | per plugin queue ≤ 64, coalesced per (kind, id) | effects dropped, strike |
| B-6 | effects per call | ≤ 32 effects, ≤ 8 KiB text, facts ≤ 32 | — | excess dropped, counted |
| B-7 | memory per instance | 16 MiB default; manifest may ask up to 64 MiB (first-party) | 512 MiB all plugins | `memory_growing` denies → guest traps → fault |
| B-8 | host imports | each import charges fuel proportional to its work (e.g. `subtree` 1 unit per node + per byte/16) | — | runs out like guest code |

Budget fit: keypress → speech p99 < 15 ms gorca-side; B-2 + B-3 add at most 0.6 ms worst case and
typically < 10 µs (call 0.05–2 µs + small guest work). 10k events/s: with the prefilter only matched
nodes reach a plugin; even if all 10k nodes/s match, batches of 64 make ≈ 160 calls/s × ~2–8 µs of
marshalling ≈ 1 ms/s, plus guest work bounded by B-1: < 1 % of a core per plugin before guest compute.

#### 2.5.2 Fuel only (no epoch) — decision C-2

Each call gets `set_fuel(budget)`; `OutOfFuel` traps deterministically (3M units ≈ 138 µs, overshoot
≈ 0). Epoch interruption is **not** enabled in the plugin engine: fuel + epoch costs 34–36 % on compute
loops against 2–6 % for fuel alone, and epoch granularity (≥ 1 ms tick, trap at 1.1 ms for a 1-tick
deadline) is coarser than every budget above. What epoch would add — catching wall time spent outside
guest instructions — is covered by (a) **B-8 import fuel charging**: every host import is
non-blocking, in-memory and charges fuel for its work, so no guest loop over imports escapes the budget;
(b) the limiter bounding memory growth (page faults ≤ 64 MiB); (c) a **host watchdog**: a wall-clock
check inside host imports (every 64th call) that traps the call when wall time exceeds 4 × its budget
(preemption by the OS is not the plugin's fault and is not struck unless repeated). Determinism is a
bonus: a fuel overrun reproduces in tests. Fallback if the watchdog proves insufficient (M18a-2 gate
PX-4): a second engine with epoch only (5–10 %) for R-lane long calls (B-4), never both on one engine.

#### 2.5.3 Fault model (per (plugin, scope) slot)

Faults: trap (`unreachable`, panic, OOB), `OutOfFuel`, watchdog, limiter denial trap, malformed
effects (bad handle of another scope, unregistered code beyond the registry range, oversized text).
Not faults: calls to imports the plugin has no permission for (reads return `none`, effects are dropped
and counted, one diagnostic per kind). Admission rejections of facts are ADR-0024 `Violation`s.

```rust
enum SlotState {                       // per (plugin, scope, lane)
    Cold,
    Ready { inst: Instance },
    Backoff { until: Instant, level: u8 },   // level 1..=2 entered; durations below
    Disabled,                           // for this scope, this session
}
```

| State \ Event | call needed | call ok | fault (1st/2nd in 60 s) | 3rd fault in 60 s | backoff expires | user re-enable | scope closes |
|---|---|---|---|---|---|---|---|
| `Cold` | instantiate (3–9 µs) → `Ready`; failure counts as a fault | — | — | — | — | — | stay |
| `Ready` | call | stay | drop instance, discard effects, strike; stay `Ready` (rebuild 9–22 µs at next call) | level < 2: → `Backoff{level+1}`; level = 2: → `Disabled`; facts masked (FS-7) | — | — | → `Cold` |
| `Backoff{level}` | skipped (fail-open) | — | — | — | → `Cold`, strikes cleared, level kept, facts unmasked after the first good call | → `Cold`, level 0 | → `Cold`, level kept for the session |
| `Disabled` | skipped | — | — | — | — | → `Cold`, level 0, strikes cleared | stay |

Backoff durations: `Backoff{1}` 30 s, `Backoff{2}` 5 min; a third escalation disables. The level is
kept per (plugin, scope) for the session, so a plugin that keeps crashing ends `Disabled` after at
most 9 faults. `Disabled` gives one spoken notice and a diagnostics entry.

- The W and R slots of a scope share strikes. The plugin's fact source has the matching ADR-0024
  `SourceState` keyed by `StableIdentity { name: id, peer: PeerProof::Extension(id@sha256) }`:
  `Backoff` = `Paused` (masked, synchronous re-fuse of focus chain and visible window, FS-7),
  `Disabled` = `Quarantined` for that scope. ADR-0019 §2.5 escalation stays: disabled in 3 scopes within
  10 min → package `Quarantined` (all scopes), rollback to the previous version if within 24 h of an update.
- Reporting: one spoken notice per plugin per session ("Signal add-on stopped; gorca continues
  without it"), counters per (plugin, scope) (calls, fuel p99, faults by kind, dropped effects,
  admission rejections), the last fault in `explain` and the Control API diagnostics (M14).
- A fault never touches core state: effects are applied only after a successful return (ADR-0019 I-3);
  facts admitted earlier stay unless the source is masked.

### 2.6 Facts, layers and conflict rules

Plugins are ADR-0024 fact sources of class **Patch** (S1) or **Inferred** (S4); scope is the instance's
scope (FS-11); seq and `SourceId` are assigned per instance by the host; `basis` is the call's
`snapshot-token` (W-4), resolved by the writer to the `(sem_rev, vis_rev, BindGen)` of the nodes the
call read, so a fact computed from a view that changed meanwhile is stale on arrival (FS-5).

| Producer | Stratum | Who may | Speech |
|---|---|---|---|
| manifest `[[patch]]` (declarative) | S1 | any enabled plugin | plain |
| `facts.assert(layer = patch)` (computed patch) | S1 | T2 (first-party); T3 only if the user marks the plugin trusted (QP-3) | plain |
| `facts.assert(layer = inferred)` | S4 (below derived) | any with `contribute-facts` | hedged by class; T3 capped at `Probable` |
| evidence groups (bit 15) | side table, read by S3/S4 | any with `contribute-facts` | never directly |
| `[[profile]]` rows | settings plane (L3 third-party off by default, L4 first-party on) | ADR-0016 | — |

Conflict rules (extend ADR-0022 §2.2 S1 and ADR-0024 §2.9; no author-chosen priorities, no arms race):

- **CR-1 Across layers**: unchanged — user > patch > observed > derived > inferred > default. A plugin
  can override an engine name only at S1; built-in S-1 quirks and S3 derived rules always yield to S1
  and always beat S4.
- **CR-2 Inside S1**, per (node, group), rank by `(tier: T2 before T3, specificity desc, plugin id,
  patch id)`. A computed patch targeting a node handle or a `Unique` anchor has maximal specificity; a
  `selector` target has its selector's specificity. Two candidates at equal tier and specificity with
  different values produce a `patch-tie` diagnostic (both in `explain`); the winner is deterministic.
- **CR-3 Inside S4**: ADR-0024 order `(ConfClass after T3 cap, kind order, source id, ...)`.
- **CR-4 Existence**: `hide` is S1 on the existence register; any plugin's `unhide` only retracts its
  own `hide`; the user's "show hidden" override (S2) wins.
- **CR-5 Categories**: membership is a set union across plugins (no conflict); built-in `Cat` stays a
  pure function of `Semantics` (SM-1).
- **CR-6 Keys**: reserved chords (ADR-0019 I-7) are unbindable; within a scope, first-party binds before
  third-party, then plugin id order; a shadowed binding is a load diagnostic, spoken once in input help.
- **CR-7 Present edits** (unstable): fixed phase/before/after order (ADR-0019), then PR-1.
- **CR-8 No feedback.** W-lane reads return S0 only (PM-3), so a plugin's input never contains any S1/S4
  output. FUSE remains a pure function of registers (ADR-0024 §2.2), and the set of plugin facts in one
  writer turn is a function of the S0 snapshot and the plugin's private state. Formal check: MQ-1.

### 2.7 Capabilities, trust and distribution

**Permissions** (manifest, each with a reason msg id; deny by default; spoken on install/enable):

| Permission | Grants | T2 first-party | T3 third-party |
|---|---|---|---|
| (none) | role, states, counts, structure of watched nodes; manifest selectors | yes | yes |
| `read-names` | names/descriptions in reads | declared | declared, shown |
| `read-text` | own text, `text`, text requests (message bodies are content) | declared | declared, spoken warning |
| `read-attributes` | `attr` | declared | declared |
| `contribute-facts { groups, layers }` | `facts.*` for listed groups | declared | `inferred` only unless trusted (QP-3) |
| `speak` | `output.*` | declared | declared |
| `navigate` | `move-cursor`, `request-mode` | declared | declared |
| `act` | `activate`, `focus` | declared | per-plugin user consent |
| `pointer` | `pointer-click` | declared | **not in 0.9** |
| `storage` | state dir quota 1 MiB | declared | declared |
| `net:*`, `clipboard-*`, `observe-typing`, `observe-all-apps` | — | **not in 0.9** | **not in 0.9** |

- **AC-1 Secrets** (ADR-0019 I-5, ADR-0002 X-1..X-11): password/secret nodes and their subtrees are
  absent from every read; facts targeting them are rejected (FS-8).
- **AC-2 Scope**: a handle of another scope is a fault; a K2 plugin sees one document only.
- **AC-3 Acting needs a user**: `actions.*` effects are applied only while a command of this plugin is
  in flight, or in a `reactor.deliver` of a request issued by that command in the same epoch. `derive`,
  timers and other deliveries can never act. Every action re-validates the target at apply time and
  the ADR-0024 §2.3.3 "act on what was spoken" check.
- **AC-4 Pointer**: `pointer-click` = gorca's ADR-0025 click on the node's centre through the helper.
  It requires all of: plugin permission `pointer` (T2 only), the helper's allow-list `pointer` cap for
  gorca (AL-1), the user's pointer setting on, AC-3, ≤ 2 clicks/s. Plugins never talk to the helper.
- **Trust tiers**: T2 = first-party: built from this repository, shipped with gorca under
  `/usr/lib/gorca/addons/`, trusted by an sha256 list compiled into the daemon (no signature
  infrastructure needed in M18a). T3 = everything else: sideloaded `.gorca-addon` (owner Q-A: allowed,
  spoken warning, same sandbox), off until enabled; store and signatures are ADR-0020 (Phase 4).
- **Safe mode** (M11): plugin components never run; first-party K0 **profile rows** stay on
  (consistent with ADR-0016 M-5, so VS Code never gets a buffer by accident); patches and third-party
  rows are off.
- **Hot reload**: a changed package is staged (validated, precompiled off-lane) and swapped at a switch
  point (no call in flight for that plugin; ADR-0019 §2.5); its W/R instances are rebuilt lazily; its
  source epoch bumps, so its old facts are masked and re-derived (background, focus chain first).
  Dev mode (`--ext-dev DIR`) watches the directory and reloads on change.

### 2.8 Migration

Rule: a plugin replaces a core row or switch only after a **shadow run** (plugin facts admitted but
masked, `explain` diff against the old path; 0 diffs on the fixture suite), then the core row is deleted
and an xtask lint forbids application names in `crates/gorca-core` and `crates/gorca-config/product`
(allow-list empty after M18a-7).

| # | Plugin | Kind | Moves | First version |
|---|---|---|---|---|
| P-1 | `vscode` | K0 | `web-apps.toml` `vscode` row (verbatim, ADR-0024 PS-3) | profile row `browse.enabled = false`; later: status-bar line/col command (NVDA `code.py:101` `getStatusBarText`) |
| P-2 | `signal-desktop` | K1 | the planned Signal row (decisions 2026-10-19, item 3) | row `browse.mode_default = focus`; `[[watch]]` message rows → computed S1 name "author, text, time" only where generic S3 is wrong; category `message`; commands next/previous message, jump to composer. Generic fixes (S3 recursion, grid pass-through) stay in core |
| P-3 | `telegram-web` | K2 (one package, two site profiles `/a/*`, `/k/*`) | the 98cb234 demo (re-based on facts), the Telegram A study | chat row S1 name "Alice, 3 unread, last message"; categories `chat`, `unread chat`; `live` patch on the message list; commands next/previous unread; `route-changed` resets state |
| P-4 | `gnome-shell` (M18a-7) | K1 | `builtin_ext.rs` `AppProfile` Shell switches → manifest event-policy data (`speak_selection`, `watch_showing`); policy → facts (`valuetext` for the OSD, `live` for dialogs) + `present.edit` for silence | after `present-edit` stabilises |

### 2.9 Milestones (M18a slices)

Gate before M18a-1: **M22b1 merged** (presenter reads only fused entries) and the registries frozen.

| Slice | Content | Acceptance | Benchmarks |
|---|---|---|---|
| M18a-1 | WIT 0.9 freeze from `wit/gorca-ext-0.9/` into `gorca-ext-api` (beside 0.3), generated registries, golden empty components | host + guest bindgen compile; registry equality test (L-1); a component built for 0.9.0 links against 0.9.1 with an added import | — |
| M18a-2 | Runtime: pool, `.cwasm` cache, W/R slots, `SlotState` table, fuel budgets + import charging + watchdog, limiter, strikes/backoff, `SourceState` link | exhaustive `SlotState` tests; fault injection (`ext-test`: trap, spin, OOM, bad handle, effect flood) never stalls a writer > budget + 50 µs | PX-1..PX-5 |
| M18a-3 | K0: manifest v2, add-on profile rows (L3/L4), selectors + declarative patches into S1, categories, watch prefilter; **P-1 vscode**; `web-apps.toml` deleted | vscode parity e2e; safe-mode row test | XP-8 (≤ 100 ns/node), B-FUSE with 100 patches unchanged |
| M18a-4 | W lane: `rules`, `facts`, admission via `gorca-fact-api`, interactive slice, masking on fault; **P-2 signal-desktop** (shadow → switch) | Signal private-session e2e: message rows read author/text/time at first focus | PX-6, PX-7 |
| M18a-5 | R lane: `commands`, `output` (content utterances through render), `actions` (AC-3/AC-4), `settings`, `timers`, `requests`/`reactor`, `site` world + `route-changed`; **P-3 telegram-web** A and K | M8-TG/M8-TGL re-based on mock A and K pages; capability denial tests | PX-8 |
| M18a-6 | `present-edit` (stable once `PresIr` frozen), spoken permission review, sideload path, manifest/package fuzzing | fuzz 0 host crashes (XP-7) | XP-2 |
| M18a-7 | P-4 gnome-shell port, `builtin_ext.rs` deleted, app-name lint | M4b Shell e2e unchanged | XP-1 |

### 2.10 Metrics to validate

| Id | Metric | Gate |
|---|---|---|
| PX-1 | cold attach of a precompiled plugin (deserialize + instantiate W) | p99 ≤ 1 ms, never on the Reader |
| PX-2 | trap → rebuilt instance | p99 ≤ 50 µs (spike 9–22 µs) |
| PX-3 | memory per idle instance pair | ≤ 64 KiB + guest state; 50 plugins × 2 lanes ≤ 120 MB RSS worst case |
| PX-4 | runaway guest in `derive` interactive | writer turn delayed ≤ budget + 50 µs p99; watchdog catches an import loop within 4 × budget |
| PX-5 | fuel-only vs fuel+epoch on the plugin corpus | fuel-only ≥ 20 % faster on compute-bound hooks, no escape in fault injection |
| PX-6 | keypress → speech p99 with P-2 + 9 dummy matching plugins (Signal fixture) | Δp99 ≤ 0.3 ms vs no plugins; facts present at first utterance ≥ 99 % of focus moves |
| PX-7 | 10k ev/s storm (50k for L3-50k), 3 plugins watching 50 % of nodes | ingress lag unchanged; per-event fusion+plugin p99 ≤ 10 µs; drops counted, writer never blocked |
| PX-8 | quick nav by plugin category on 3000-chat Telegram mock | ≤ 5 ms key → speech (W1), no guest call per key |
| XP-1, XP-2, XP-5, XP-7, XP-8 | as ADR-0019 §5 (XP-2 cap now 0.3 ms) | — |

## 3. Alternatives considered

| Alternative | Why not |
|---|---|
| A. Keep ADR-0019's `mirror` + `present`/`policy` verdicts as the main path | builds on raw mirror data and legacy speech; fixes one surface at a time; owner rule forbids it |
| B. Fact rules as async `FactPort` producers on a worker (ADR-0024 transport for every plugin) | the first utterance of a newly focused node misses the fact (refine tail at best); a hop costs 5–14 µs p50 / 0.19 ms p99 via `spawn_blocking`; the sync call costs 0.05–2 µs. Kept for `reactor`-originated facts later |
| C. One instance per plugin, shared by all actors (behind a mutex) | contention between app actors and the Reader; one slow call stalls every app; violates "one actor per application" isolation |
| D. W-lane reads of fused entries | feedback loops between plugins (A's patch feeds B's rule feeds A's), order-dependent results, oscillation; breaks the cascade rule ADR-0022 A4 |
| E. Fuel + epoch (current `gorca-ext-host`) | 34–36 % compute penalty; epoch granularity coarser than our budgets; import charging + watchdog cover what epoch adds (C-2) |
| F. Third-party computed patches at S1 by default | a sideloaded plugin could rename any control plainly (UI redress); S4 with hedge and taint is the safe default |
| G. Priorities declared by plugins | arms race, nondeterminism by convention; static rank (tier, specificity, id) instead |
| H. Native shared-object plugins | no isolation; first-party native stays only for the legacy 0.3 fallback until ported |

## 4. Consequences

- The core gains: the selector engine and watch prefilter in the writers, the W/R slot runtime, the
  plugin fact source wired to `gorca-fact-api`, handle tables per scope, registry generation.
- `gorca-ext-host` runs two engines during the transition (0.3 legacy fuel+epoch; 0.9 fuel-only).
- App names disappear from core code and product rows (lint).
- Plugin authors write against a small, typed, backend-neutral API; behaviour is explainable
  (`explain` names plugin, layer, tier, binding, basis).
- Costs: a writer turn may spend up to 0.3 ms on plugins at focus; the W/R split forbids shared plugin
  memory (state must be expressed as facts or recomputed).
- ADR-0019 §2.2, the I-1 budget and C-1 are superseded by §2.2–§2.5 here; ADR-0019 status should be
  revisited together with this ADR.

## 5. Contested choices

- **C-2 fuel-only vs fuel + epoch.** Decided fuel-only (§2.5.2), medium-high confidence; validated by
  PX-4/PX-5 in M18a-2 with fault injection. No bake-off unless PX-4 fails.
- **C-3 synchronous W-lane `derive` vs async FactPort (alt. B).** Recommended sync. Measurable; if the
  reviewer contests it, a two-day bake-off on the Signal fixture: arm A sync interactive slice, arm B
  async port + refine tail; criteria: facts at first utterance (≥ 99 % vs ?), writer turn p99 delta,
  K2 Δp99, code size of the writer integration. Pick A unless its writer p99 delta > 0.3 ms.

## 6. Open questions

Owner:
- **QP-1** One `telegram-web` plugin with two site profiles (A, K): recommended yes.
- **QP-2** Safe mode keeps first-party profile rows (vscode) but runs no plugin code: recommended yes.
- **QP-3** Third-party computed facts go to S4 (hedged) unless the user explicitly marks the plugin
  trusted (spoken warning), which lets it write S1: recommended yes.
- **QP-4** First-party plugins as files under `/usr/lib/gorca/addons/` trusted by an embedded sha256
  list (hot-reloadable, inspectable) rather than embedded bytes: recommended files.
- **QP-5** `pointer-click` first-party only in 0.9: recommended yes.

Math researcher:
- **MQ-1** With S1 extended by computed patches whose producers read only the S0 snapshot plus private
  plugin state, ranked statically by (tier, specificity, plugin id, patch id): prove FUSE keeps unique
  results and order independence (Lemma 2, Theorem 1), F12 and F18, and that publishing converges
  (no oscillation) when plugin state is a function of the history of S0 snapshots only.
- **MQ-2** Staleness by `snapshot-token`: given that a `derive` call reads a set R of nodes at token t,
  and the writer admits its facts in the same turn, give the minimal per-node revision set to record so
  that "stale on arrival" is exact (no false negatives) and the bookkeeping is O(|R|).
- **MQ-3** Interactive budget split: N plugins share 0.3 ms per focus change; propose a fair, bounded
  schedule (deficit round robin by observed fuel p99?) and the worst-case focus publish delay bound.

WASM engineer (beyond the spike):
- **WQ-1** `derive` with a 64-node `view-node` batch (names ≤ 64 B): call cost p50/p99.
- **WQ-2** Fuel calibration across guest code shapes (string-heavy, call-heavy) and P/E cores; the
  units/µs table B-1..B-5 must use.
- **WQ-3** Cost of charging fuel inside host imports (`get_fuel`/`set_fuel` per call) and of the
  watchdog check every 64th import.
- **WQ-4** `memory_growing` denial: clean trap path and rebuild cost with 16 MiB touched.
- **WQ-5** RSS of a W/R instance pair of the same component (shared code), and mmap-backed `.cwasm`
  page-in cost.
