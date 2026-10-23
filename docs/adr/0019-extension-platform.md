# ADR-0019: Extension platform (add-on worlds, contract, versioning, lifecycle)

Status: **Proposed** (draft, 2026-10-17). Owner: architect. Full analysis, NVDA capability matrix and
store groundwork: `docs/reports/addon-platform-architecture.md`.
Amends: ADR-0001 §10 (extension model), ADR-0008 §6.1 and amendment 3 (web hooks become the `site`
world). Covers the scope reserved for ADR-0015 ("extension API stability") in `docs/plan/phase-3.md`.
Milestone: M18 (M18a platform core, M18b SDK). Follow-up: ADR-0020 (packaging, signing, store; Phase 4).
**Partly superseded by [ADR-0026](0026-wasm-plugin-model.md)** (WASM plugin model, Proposed r2): §2.2
(world table, `mirror`/`effects` sketch), I-1 (inline budget) and C-1 (node representation) are
replaced there; the rest of this ADR stays the umbrella for lifecycle, versioning and packaging.

## 1. Context

The owner wants add-ons to extend any application and to build arbitrary features, with an add-on
store later. The current contract (`crates/gorca-ext-api/wit/gorca-ext.wit`, `gorca:ext@0.3.0`; 0.3.0 adds `term-config.word-level` (`[editing] word`), `term-config.boundary` (`[editing] boundary`) and `term-action.cue` (the boundary cue); pre-1.0 versions carry no ABI promise, so components are rebuilt against the new package) was
built for first-party policy: one world, a closed `role` enum, nine event kinds, `command(id)` with no
context and speech as its only effect, `fetch`/`text` declared but `not-supported`, a string KV, and a
manifest with a name but no id, version, API range or permission rationale. The experimental
`web` interface (0.3.0, branch `worktree-agent-aa50a60757c1de84c`) proved two things: declarative
profiles evaluated natively by the core cost nothing per node (describe p99 8.5 µs on 3000 chats,
zero difference without the add-on), and the SPA route-change and category-count limits need a
general mechanism.

The isolation layer is already strong (`gorca-ext-host`: one Store per extension × app, fuel and a
CPU-time epoch backstop, 32 MiB, reset from `InstancePre`, three strikes in 60 s disable). NVDA has no
sandbox, but its add-ons can do nearly anything, and they break every year because the whole Python
namespace is the API (`refs/nvda/source/addonAPIVersion.py:17-37`: `BACK_COMPAT_TO` bumped in 2019.3,
2021.1, 2022.1, 2023.1, 2024.1, 2025.1, 2026.1, 2027.1).

Forces: the 15 ms keypress → speech budget; never-block (R3); crash isolation; Component Model types
are closed (a new record field or variant case is a new type), while instances may provide extra
functions and wasmtime resolves semver-compatible import names
(`refs/wasmtime/crates/wasmtime/src/runtime/component/linker.rs:26-60`).

## 2. Decision

### 2.1 Principles

- **PA-1 Data first.** Everything declarable is manifest data evaluated by the core: keymaps and
  prefix layers, selectors, node patches (rename, re-role, label-by, hide, flatten, live, category,
  sleep, claim), categories, vendor profiles, dictionaries, symbols, cue themes, settings schema,
  catalogs, settings migrations. Data-only add-ons have no component.
- **PA-2 Pure hooks, transactional effects.** Hooks take facts and return verdicts. Effects are
  imported functions that append to a per-call bounded buffer; the host applies it after the hook
  returns and drops it on trap or timeout.
- **PA-3 No blocking host call.** IPC-backed reads are `request(query, budget) -> req-id` answered by
  `reactor.deliver(req-id, reply)` under a host deadline.
- **PA-4 Lane placement.** Only `present`, `policy`, `describe` (and a future speech filter) run
  inline on the Reader, with a per-presentation aggregate cap.
- **PA-5 The core decides.** Section 2.7.

### 2.2 Worlds

> **Superseded by ADR-0026 §2.2–§2.4** (plugin kinds K0–K4, WIT `gorca:ext@0.10.0`, W/R lanes,
> facts instead of `mirror`/`effects` verdicts). Kept for history.

| World | Topology | Exports |
|---|---|---|
| data-only | none | — |
| `app-module` | Store per (add-on, app), lazy, idle-evicted | `app-hooks` (app-focused/blurred, on-event, present, policy), `commands`, `reactor` |
| `site` | Store per (add-on, browser app), state keyed by `doc-id` | `site` (open, describe, route-changed), `commands`, `reactor` |
| `terminal-policy` | per (add-on, terminal) | `terminal` (as 0.2) |
| `global` | one Store per add-on, `ext-global` lane | `global-events.on-batch`, `commands`, `reactor` (speech filter later) |

All worlds import `mirror` (read-only, in-memory) and `effects` (say, cue, braille-flash,
move-cursor, act, arm-timer, set-setting, request, cancel, log; open-chooser in P1). Interface sketch:
report §4.3.

### 2.3 Invariants

- **I-1** *(Superseded by ADR-0026 §2.4.2 and §2.5.1: B-2 0.3 ms per focus change and B-3 0.3 ms per
  utterance, split by progressive filling.)* Extension code on the keypress → speech path is limited to inline hooks: 2 ms CPU per call
  and **3 ms aggregate per presentation**; on overrun the remaining add-ons are skipped and the core
  presentation is used (fail-open), and the overrunning add-on takes a strike.
- **I-2** No host import blocks or performs IPC; every IPC is a request with a deadline
  (interactive 150 ms, prefetch 500 ms, background 2 s, http 10 s); at most 8 in flight per
  instance, the ninth fails `busy` at once.
- **I-3** Effects are applied only after a successful return, in order; a fault applies none.
- **I-4** Every effect and request is tagged with the epoch at call time; add-on speech of a stale
  epoch is cancelled; `interrupt` priority only inside a command the add-on owns.
- **I-5** Secret nodes and text typed into them are never visible to any add-on (fail closed).
- **I-6** Producers never wait on add-ons: global mailboxes are bounded (256), coalesce by
  (kind, node), drop oldest, and report `dropped`.
- **I-7** Reserved chords (stop speech, toggle speech, disable all add-ons, input help) cannot be
  bound or claimed.
- **I-8** An add-on writes settings only under `ext.<id>.*`, validated by its schema; it cannot
  activate profiles.

### 2.4 Backpressure and budgets

As report §4.5: effects ≤ 32 per call and ≤ 8 KiB text; timers ≥ 50 ms, ≤ 16 armed; global batches
≤ 64 events, 5 ms CPU; add-on speech outside commands ≤ 10 utterances/s, excess coalesced to the
latest; memory 32 MiB per instance (exists).

### 2.5 Lifecycle

> r3 pointer (ADR-0027): the precompiled `.cwasm` cache is keyed by package hash + `Engine::precompile_compatibility_hash()` (includes host ISA flags); packages ship `.wasm` only.

Package states `Discovered → Installed{enabled} → Active`, with `Rejected`, `Staged{next, prev}`,
`Quarantined{reason}` and `PendingRemoval`; transition table in report §4.6. Key rules:
updates are staged (validated and precompiled while the old version runs) and swapped at a switch
point (no slot mid-call); a version quarantined within 24 h of its swap rolls back automatically with
one notice; an add-on disabled in three apps within 10 minutes is quarantined globally; safe mode
(M11) quarantines every non-core add-on. Slot states: `Cold → Instantiating → Active ↔ Resetting →
DisabledForApp`, `Active → Cold` after 10 minutes idle. Precompiled code is cached keyed by package
hash, wasmtime version and engine configuration.

### 2.6 Versioning (replaces the planned ADR-0015)

- **V-1** `gorca:ext@MAJOR.MINOR.PATCH`; 0.9 is the preview, 1.0 freezes.
- **V-2** Within a major: add interfaces, add functions to imported interfaces, add optional exported
  interfaces (host probes). Never change an existing type or signature.
- **V-3** Growing vocabularies (role, state, event kind, segment kind, prop bits, cue names) are open
  numeric codes with append-only registries; unknown means unknown, never a trap.
- **V-4** Inputs that grow are read through accessor imports; records are frozen snapshots.
- **V-5** The host links the current and the previous major for at least 24 months via host-side
  adapters; golden components of every released minor run in CI forever.
- **V-6** Experiments live behind `@unstable(feature = ...)`, enabled only with `--experimental` or
  the dev channel.
- **V-7** Compatibility is computed from the component's imports/exports, not declared; the manifest
  keeps `min-gorca` for behaviour only.
- **V-8** Deprecations are announced one minor ahead with a load diagnostic; removal only at a major.

### 2.7 Kept in the core

Focus bookkeeping, the mode machine and cursors (add-ons request moves), speech scheduling and
cancellation, secrets, input grabs and raw key classification (terminal raw keys excepted), reserved
chords, user settings outside the add-on namespace, profile activation, all IPC and connector types.

### 2.8 Permissions and packaging

Deny by default; permissions with a reason each: `speak`, `cue`, `braille`, `read-tree`, `act`,
`observe-all-apps`, `observe-typing`, `storage`, `net:<origin>`, `clipboard-write`, `clipboard-read`,
`capture` (P2). Package `<id>-<version>.gorca-addon` (zip, fixed layout, `CHECKSUMS`, signatures in
Phase 4), installed under `~/.local/share/gorca/addons/<id>/<version>/` with a `current` pointer.
Threat model, signing and store review: reviewer report and ADR-0020.

## 3. Alternatives considered

| Alternative | Why not |
|---|---|
| Keep one `extension` world and grow it | every world-level change rebuilds every component (as 0.2/0.3 did); global and site add-ons get hooks they cannot use; no singleton topology |
| NVDA-style extension points (Action/Filter/Decider) as host callbacks into the guest | callbacks on arbitrary core paths put guest code on hot paths with no placement rule; versioning by closed types makes each point a breaking surface |
| Host-side callbacks with direct effects (no buffer) | partial effects after a trap; no replay determinism; adding a variant case for a new action breaks old guests |
| Synchronous `fetch` with a deadline inside hooks | blocks the calling lane up to the deadline; violates R3 |
| Record-only node model with `role` enum | every new role or property is a major version; NVDA-style breakage by construction |
| Async component model (WASI 0.3 async exports) | not needed for pure hooks; revisit when it stabilises in wasmtime and only for `reactor` |
| Native plugins (shared objects) for third parties | no hang or memory isolation; native stays first-party only (ADR-0008 §3.3) |

## 4. Consequences

- M18 grows from "SDK preview" to platform core + SDK (M18a/M18b); the three first-party add-ons are
  ported to 0.9.
- The core gains a selector/patch engine shared by desktop and web mirrors and a dynamic category
  registry (replaces the four reserved bits of the demo).
- Phase 3 features (M13, M14, M14a, M15) consume add-on data through one manifest model.
- Guests lose some type safety (open codes); the SDK compensates with non-exhaustive enums.
- The host carries adapters for the previous major for 24 months.

## 5. Metrics to validate

| ID | Metric | Target |
|---|---|---|
| XP-1 | keypress → speech p99 with 10 matching app-module add-ons | within 0.5 ms of none |
| XP-2 | aggregate inline extension CPU per presentation p99 | ≤ 1 ms (cap 3 ms) |
| XP-3 | cold instantiate of a precompiled matching add-on | p99 ≤ 2 ms, off the Reader |
| XP-4 | 10k ev/s storm, 3 global add-ons subscribed to all | Reader p99 unchanged, hub never blocked, drops counted |
| XP-5 | golden components of every released minor | link and pass, 100 % |
| XP-6 | crash-looping update | automatic rollback ≤ 1 s, settings intact |
| XP-7 | fuzzed manifests/packages | 0 host crashes, bounded memory |
| XP-8 | 100 patches + 32 categories on a 50k-node page | ≤ 100 ns per upserted node p99 |

## 6. Contested choice and bake-off

> **Resolved by ADR-0026 §2.3.3**: opaque per-instance handles + accessors, one frozen `view-node`
> batch record, no `resource` type. Kept for history.

**C-1 node representation across the boundary**: frozen `node-facts` records + accessors (A) vs a
`node` resource with lazy getters (B). Run a two-day spike measuring `present` p99 on the M4 Shell walk,
`describe` p99 on Telegram 3000 (≤ 20 µs), guest allocations per call, X1 unchanged, and a compat test
(a property added in a minor must not break an old golden component). Escalate to `/bakeoff` only if
p99 differ by more than 2x or (A) cannot pass the compat test.

## 7. Open questions for the owner

- Q-A Sideloading without the store, with spoken permission review: recommended yes.
- Q-B During the 0.9 preview, store add-ons only on beta/dev channels: recommended yes.
- Q-C Reproducible build mandatory for the reviewed tier from day one: recommended yes.
