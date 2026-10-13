# Bake-off B-1: native mirror fill strategy

Status: **spec only**. No harness and no arms yet (step 1 of `.claude/skills/bakeoff/SKILL.md`).
Decision under test: ADR-0001 §15, B-1 ("Native-app mirror fill strategy"), Phase 2 milestone M2
(`docs/plan/phase-2.md` §M2).
Planned harness: `crates/gorca-bench-b1`, `docs/bakeoff/b1-mirror-fill/harness/`, the fake app
`tests/harness/b1/` (section 11).
Blocking inputs: shared pre-arm work PW-1…PW-12 (section 4.7) merged on `main`; harness H-1…H-12
(section 11) merged on `main`. **P-9 answered 2026-10-13** (`docs/reports/2026-10-13-p9-cache-p2p.md`):
eager `GetItems` is complete only for gnome-shell, so B-1 runs **two arms, A and C**; pure-eager
arm B is dropped (orchestrator decision, `docs/decisions.md` 2026-10-13; section 2.2).

## 1. Problem

Phase 1 refetches the focused node on every focus change. The mirror (`gorca-mirror`) exists, but
nothing invalidates it: `PropertiesChanged`, `ChildrenChanged` and friends only set
`resync_needed` in the app actor (`crates/gorca-core/src/app_actor.rs`, `route()`), and a mirror
hit is presented as is (`crates/gorca-core/src/present/flow.rs`, `BeginPresent`). So M2 needs two
things:

1. an **invalidation contract**: which event makes which cached field untrustworthy, what happens
   when the toolkit sends no event, and how a removed or restarted object is never spoken; this
   is the same for every arm and is normative (section 5);
2. a **fill strategy**: when to fetch what, so that the next focus, state change or "where am I"
   is answered from memory (L1h ≤ 2 ms) without flooding the bus or the application's main loop.

B-1 picks (2). The arms differ only in their fill policy and in how the connector executes the
fetches the policy asks for. Everything that decides correctness (invalidation, revalidation,
tombstones, bounds, the store) is shared code that exists before the arms start.

| Arm | Idea | Expected strength | Expected weakness |
|---|---|---|---|
| **A lazy** | fetch on demand; events invalidate; nothing prefetched | fewest calls and bytes, no head-of-line blocking in the app | misses on first visits; ancestry for "where am I" costs round trips |
| ~~**B eager**~~ | *dropped after P-9:* `GetItems` is incomplete on GTK4 (realised objects only, no `RemoveAccessible`), GTK3 (partial), Firefox and Qt (empty) | — | — |
| **C hybrid** | lazy everywhere; eager `GetItems` only for apps whose dump is verified complete (gnome-shell); on focus/window activation prefetch ancestry to the window and the focus's siblings | hit rate of an eager mirror on Shell and of a focus-window prefetch elsewhere, at a cost close to A | Shell main-loop stall during the dump; prefetch misses jumps (search results, new panels); more policy state |

Out of scope: web documents (ADR-0003 doc actor; `GetItems` does not return document nodes,
ADR-0001 §5.4), text content and caret (M3, `TextQuery`), review/object navigation commands (M4),
the store implementation (MS-3 decided: `scc::HashIndex`,
`docs/reports/2026-10-13-mb1-mirror-store.md`), the drain and ingress (ADR-0004).

## 2. Platform facts and rules that bind the design

### 2.1 Facts (verified in `refs/` unless marked)

| # | Fact | Source | Consequence |
|---|---|---|---|
| F-6 | `GetItems() -> a((so)(so)(so)iiassusau)`: ref, app, parent, index, child count, interfaces, name, role, description, states. 168-334 items in 4.3-5.2 ms on this host (Python GIO). No value, relations, attributes or bounds | `docs/platform/org.a11y.atspi.Cache.md`; `refs/gtk/gtk/a11y/gtkatspicache.c:48-49` | A GetItems node is **partial**: it satisfies `PropMask::PRESENT` only when the node has no `Value` interface and a non-empty name (else value / labelled-by need a fetch) |
| G-1 | GTK4 emits `accessible-name` only when the `LABEL` property changes; names computed from content or `labelled-by` change silently | `refs/gtk/gtk/a11y/gtkatspicontext.c:1231-1236` | Event invalidation alone cannot keep GTK4 names fresh → revalidation RV-1 (section 5.3) |
| G-2 | GTK4 skips `AddAccessible` and `RemoveAccessible` for contexts in the hidden state | `gtkatspicache.c:222-229, 251-259` | An eager cache can keep ghosts and miss additions (P-9 H1) |
| G-3 | GTK4 suppresses `AddAccessible` while it serves `GetItems` | `gtkatspicache.c:439-443` | Objects added during a dump may be neither in the reply nor signalled (P-9 H2) |
| G-4 | GTK4 `GetApplicationBusAddress` returns the a11y **bus** address, not a peer socket | `gtkatspiroot.c:191-196`, root created with `get_bus_address(display)` at `gtkatspicontext.c:1589` | For Settings and Files, "P2P" (D-7) is a second bus connection: replies still cross the broker. The P2P pool must detect it and reuse the call connection instead of opening a duplicate. **Superseded live by P-9:** GTK4 4.18 answers `UnknownMethod` (method missing from the interface info), so GTK4 apps are mode `None`; `SameBus` stays as a defensive mode |
| G-5 | GTK4 paths are `<base>/<uuid>` | `gtkatspicontext.c:1610-1611` | Paths are never reused → `LocalId = FNV-1a(path)` (ADR-0004) satisfies I-1 for GTK4 |
| G-6 | GTK4 list and grid views (`GtkListView`, `GtkColumnView`, `GtkGridView`, used by Files) recycle row widgets while scrolling; a row keeps its path and shows another item (unverified on the wire: P-9 H4) | GTK design; to confirm in P-9 | Same key, different logical item, name possibly changed silently (G-1): a cached row name is suspect after any scroll |
| A-1 | atk-bridge (GTK3, St/Shell via Cally) numbers objects with a monotonic counter `/org/a11y/atspi/accessible/<n>` | `refs/at-spi2-core/atk-adaptor/accessible-register.c:155-162, 180-182` | Paths are never reused (until 2^32 wrap) → I-1 holds |
| A-2 | atk-bridge's cache is lazy (idle-batched `AddAccessible`), excludes `TRANSIENT` objects and children of `MANAGES_DESCENDANTS` containers, and leases event-only objects for 15 s, after which their path answers `UnknownObject` | `accessible-cache.c:303-308`; `docs/platform/org.a11y.atspi.Cache.md` | A cached key may die without any event; `UnknownObject` is authoritative (I-1, Firefox note in ADR-0001 §5.6) |
| A-3 | atk-bridge serves a real peer socket from `GetApplicationBusAddress`, but returns `""` when libatspi is initialised in the same process | `application-adaptor.c:100-120` | GNOME Shell calls `Atspi.init()` when the magnifier's focus/caret tracking is on (`refs/gnome-shell/js/ui/focusCaretTracker.js:48`): then Shell has no P2P and must use the bus call connection |
| C-S | Cache signals are delivered only with an explicit `interface='org.a11y.atspi.Cache'` match rule; they are always on in the app (not gated by `RegisterEvent`) | `docs/platform/org.a11y.atspi.Cache.md`; P-2 in ADR-0001 §16.1 | Subscribing to them is an arm decision with a msgs/s cost (section 6) |
| S-1 | St (Shell) names derived from a `label_actor` change without an event on the labelled widget (to confirm with the census H-1) | GNOME Shell St widgets | Same treatment as G-1 |
| S-2 | Shell's Alt+Tab switcher announces only `state-changed:selected` and is rebuilt per invocation (new paths) | `docs/decisions.md` 2026-10-13 (switcher fix) | Subtree-replace pattern; the fill must not prefetch dead switchers |
| D-1 | A toolkit main loop serves D-Bus requests one at a time | GLib/GDBus dispatch | A 50 ms `GetItems` delays every request queued behind it, including the Interactive focus fetch (head-of-line). The fake app must model this (H-2) |

### 2.2 P-9 results and what they change (answered 2026-10-13)

Source: `docs/reports/2026-10-13-p9-cache-p2p.md` (LIVE-real read-only probes on this desktop,
LIVE-private scripted GTK4 4.18.6 / GTK3 3.24.49 apps under a private headless Shell, SRC reading).
"Complete" = the set reachable by a `GetChildren` walk equals the `GetItems` set.

| Toolkit | `GetItems` complete? | Cache signals | P2P (D-7) |
|---|---|---|---|
| gnome-shell (St, atk-bridge) | **yes**: 1 765 items in 17.0 ms (~10 µs/item), walk found exactly 1 765; 0 idle diffs | `ChildrenChanged` add/remove on every actor add/remove | yes (`GetChildren` p50/p99 0.108/0.337 ms vs bus 0.140/1.088 ms) |
| GTK4 (Settings, Nautilus) | **no**: only realised contexts; a fresh app or Nautilus returns **2 items**; grows only when a client walks (Nautilus 2 → 307, 13.8 ms, ~45 µs/item) | `AddAccessible` per realised node only (not its children), none for hidden widgets; **`RemoveAccessible` never observed** (removal gives `ChildrenChanged:remove` + `defunct` only) | **no** (`UnknownMethod`) |
| GTK3 (atk-bridge) | **partial**: gnome-terminal 170 of 173; gedit 181 of 488 (no descent into `MANAGES_DESCENDANTS`, menus, popovers) | Add correct for normal containers; Remove only on finalisation | yes |
| Firefox | **no**: 2 items vs 400+ reachable | — | yes |
| Qt | **no**: always empty | never emitted | no |

Answers to the hypotheses: **H1 yes**, worse than assumed (GTK4 never sends `RemoveAccessible`
for widget removal and skips hidden contexts). **H2** source-only (`in_get_items` race), unobserved:
the profile keeps it on. **H3, H4** not exercised by P-9: the profile keeps the pessimistic
answer (reorder may re-use paths with `remove`+`add`; recycled rows rename silently). **H5**
GTK4 dump cost ~45 µs/item when populated; the 10 000-entry directory is still unmeasured
(section 16).

Further P-9 facts that bind the harness and every arm:

* **P9-1** GTK4 emits **no `Object` event at all** while no AT listener is registered
  (`gtk_at_spi_root_has_event_listeners`). The shared base registers listeners before the first
  fill request; the fake GTK4 profile reproduces the gate, and IC-21 checks that nothing in the
  mirror is trusted from before registration.
* **P9-2** GTK4 sends a spurious `ChildrenChanged:add` on the parent for most child property or
  state updates. `ChildrenChanged` stays "children list dirty", never a delta (5.1 already).
* **P9-3** GTK4 names derived from content (`Button.set_label`, expander content) or from
  `labelled-by` change without an event on the named node; only the inner label fires
  `accessible-name`. Shared invalidation rule added to 5.1: `PropertiesChanged{n, NAME}` also
  invalidates NAME on the known ancestors of `n` up to the first role that does not take its name
  from content (at most 8 levels). RV-1 is unchanged and still covers the silent case.
* **P9-4** GTK4 `sensitive=false` leaves `ENABLED` set; a presenter must not infer "dimmed" from
  `ENABLED` alone (PW-12).
* **P9-5** atk-bridge leases event-only objects for 15 s, then answers `UnknownObject`: normal, not
  an error (IC-7 already).
* **P9-6** `GetItems` order is a hash order (children may precede parents); `Cache.version` is not
  readable on any toolkit. The decoder (PW-9) builds the tree in two passes by parent ref and
  detects the layout by reply signature.
* **P9-7** Calling `GetItems` or `GetApplicationBusAddress` registers gorca as an AT client in the
  app's bridge (it starts event emission to us); it is not side-effect-free (relevant to shadow
  mode, 8.4).

**Shared rules decided after P-9 (identical for both arms, harness-owned):**

* **Revalidate the focused node on every focus.** RV-1 (5.3) is unchanged and already does this for
  every presentation served from the mirror; P-9 confirms it is mandatory, not an optimisation.
* **TTL safety net (30–60 s).** A node whose last confirmation is older than its TTL reads as
  `aged`: it may still be presented (tagged `may_refine`), but any read that speaks it, including
  ancestors and context spoken by where-am-I, is followed by a revalidation `Fetch` (Interactive for
  the presented node, Prefetch for context). TTL by verified toolkit class: GTK4 30 s; GTK3 and
  Shell 60 s; browser chrome and unknown 30 s. It is evaluated on read: **no timer, no periodic
  refetch**, so B1-MSG.idle is unaffected. Implemented in `view()` (PW-1) and the Reader (PW-4).
* **Eager eligibility is decided by shared code, never by an arm or by self-reported names.** The
  connector computes `AppInfo.dump_complete: bool` and passes it to the policy as
  `FillCtx.dump_complete`. It is `true` only when (a) the app's identity is **verified**: for
  gnome-shell, the a11y-bus sender's PID (`GetConnectionCredentials`) equals the PID of the
  session-bus owner of `org.gnome.Shell`; `Application.ToolkitName`, the accessible name and
  `/proc/<pid>/comm` are self-reported and never sufficient; and (b) the toolkit is on the
  P-9-verified list, which today is gnome-shell only. A dump that comes back implausible (≤ 2
  items, or the root's child count disagrees with the dump) clears the flag for that app's
  lifetime. The harness's fake Shell owns `org.gnome.Shell` on the harness session bus with its own
  PID, so the same code path runs in the harness and on the desktop (no harness detection).

### 2.3 ADR rules

| Rule | Consequence for B-1 |
|---|---|
| I-1 NodeKey never denotes two objects; generation bump on reuse | Holds for AT-SPI GNOME toolkits because paths are never reused (G-5, A-1). Path reuse (Qt) is out of scope and counted (`paths_revived_unexpectedly`) |
| I-3 resolving `Gone` never panics or blocks | Unchanged |
| §4.4 freshness table, `may_refine` (§8.1, 150 ms audio rule), M-6 < 1 % | Section 5; the representation changes (PW-2) but not the semantics |
| MS-1/MS-2 lock-free reads, per-node consistency only | The Reader reads the store directly; no arm may add a read-side lock or a read-side message |
| C-1…C-6 connector contract, C-2 reply by deadline + 5 ms, C-3 in-flight limit | Every fill request goes through the shared driver with a class and a deadline |
| D-3/D-7 replies never share a socket with the flood; P2P lazy, `Startup` deadline, fallback to the bus call connection | Shared P2P pool (PW-6); G-4 and A-3 modes are first-class |
| D-5 minimal subscription | An arm's extra subscriptions are counted in msgs/s |
| §6.2 app actor → connector: 32 in flight per app, coalesce identical `Fetch`, reject `Background` first | Section 7 bounds; Interactive keeps reserved slots |
| §6.4 budget classes Interactive 120 ms, Prefetch 300 ms, Background 2 s, Startup 5 s; `AppHealth` | A policy gets `HealthView` and must stop Prefetch/Background in `Slow`/`Hung` (I-4) |
| PC-2 no global bus, R3 results come back as messages | The policy is a pure step function; replies come back as `FillInput::Reply` |
| §11 rule 1 / I-2 core holds no connector type | `gorca-mirror` and the policies depend only on `gorca-model`, `gorca-rt`; `cargo xtask deps` enforces it. A Newton connector must be able to use the same driver with a push-only policy |
| §11 rule 6 no `unwrap`/`panic`; `unsafe` only in `gorca-dbus`/`gorca-rt` | Same for every arm |
| ADR-0003 C-5a, §9 | The fill never descends into `Role::Document*` subtrees; those belong to the doc actor (IC-20) |

### 2.4 Prior art (what to copy, what to avoid)

* **Orca** keeps many small caches with a forced clear on events and a 60 s periodic clear
  (`refs/orca/src/orca/ax_cache_manager.py:53, 266-326`), and marks objects dead or hung explicitly
  (`ax_object.py:50, 271-280`, hung expiry 1 s). Lesson: event invalidation is not trusted alone;
  a time-based clear hides silent mutations at the cost of refetches. Our answer is RV-1 for the
  presented node instead of a global TTL.
* **libatspi** caches name/role/states/children after `GetItems` and maintains them from cache
  signals (`refs/at-spi2-core/atspi/atspi-misc.c:951-955, 1058-1062, 1115-1116`); its calls are
  synchronous with an 800 ms timeout. Lesson: cache coherence by cache signals is the toolkit's
  intended contract, and G-2/G-3 show where GTK4 breaks it.
* **Odilia** calls `GetItems` per app and then resolves every item with further async lookups
  (`refs/odilia/cache/src/lib.rs:295-311`), and handles `AddAccessible`/`RemoveAccessible`
  (`cache/src/event_handlers.rs:375-393`). Lesson: a bulk dump followed by N per-item calls loses the
  point of the dump; an eager fill (arm C on Shell) must build nodes from the dump itself.
* **NVDA** (IAccessible2) queries live and caches only in virtual buffers; **AccessKit/Newton** push
  full tree updates (push model). Lesson: the driver must support a push source (`push_snapshot`)
  with a trivial policy, so B-1 does not bake AT-SPI into the core.

## 3. What is measured, in one paragraph

A scripted walk (section 8) drives three fake apps modelled on GNOME Shell, Settings and Files
(real tree sizes, event patterns and toolkit quirks, calibrated by a census of the live desktop)
on the isolated harness, with `gorca-daemon` built from each arm as the reader. The harness
knows the truth (every mutation the fake apps make, with timestamps), sees every presentation
(a structured presentation log plus the mock speech server) and counts every D-Bus message on the
app side. It reports hit rate, latency, first-focus latency, D-Bus traffic, RSS, refine
corrections and stale presentations, gates on correctness and bounds first, then ranks.

## 4. Shared API

All signatures below are **frozen** once PW-1…PW-12 merge. An arm may add private items in its own
files (section 10); it may not change a shared signature, a default, or a shared file.

### 4.1 Model changes (`gorca-model`, PW-2)

```rust
pub struct NodeSnapshot {
    // ... existing fields (ADR-0001 §4.2) ...
    /// Fields this snapshot actually carries. A GetItems node has NAME|DESCRIPTION|ROLE|STATES|
    /// CHILDREN(count)|PARENT; a full fetch has what was asked for.
    pub have: PropMask,
    /// Mirror write sequence at which the app confirmed this content (the `issued_at` of the
    /// request whose reply produced it). Used by the late-reply and re-dirty rules (5.4).
    pub confirmed_seq: u64,
    pub fresh: Freshness,
}

/// ADR-0001 §4.4, representation change only: `Fetching` moves out of the published snapshot
/// into the driver's in-flight table (a snapshot is immutable once published).
pub enum Freshness {
    Fresh { at: Instant },
    /// `mask` = the fields an event said are no longer trustworthy.
    Stale { since: Instant, mask: PropMask },
    Defunct,
}

/// Result of a mirror read with the app-wide stale epoch applied (5.2).
pub struct NodeView { pub snap: Arc<NodeSnapshot>, pub stale: PropMask }
```

`PropMask` gains `PARENT` (bit 10). `PropMask::PRESENT` is unchanged.

### 4.2 Store (`gorca-mirror`, PW-1, harness-owned)

The store is `scc::HashIndex<NodeKey, Entry>` per app (MS-3). Readers are any thread.

```rust
pub trait MirrorRead: Send + Sync {
    fn get(&self, k: NodeKey) -> Lookup<Arc<NodeSnapshot>>;   // Hit | Miss | Gone (unchanged)
    /// get + the app-wide stale epoch folded into `stale` (5.2). Never blocks, no allocation on hit.
    fn view(&self, k: NodeKey) -> Lookup<NodeView>;
    fn revision(&self) -> u64;
    fn stats(&self) -> MirrorStats;                            // counters, exact (MA-1)
}

pub struct MirrorWriter { /* !Clone, owned by the app actor */ }
impl MirrorWriter {
    /// Monotonic write sequence; every request records `issued_at = seq()` (5.4).
    pub fn seq(&self) -> u64;
    /// Apply a reply or a push. Per node: dropped if tombstoned at a seq > `origin.issued_at`;
    /// kept Stale if invalidated at a seq > `origin.issued_at`; otherwise Fresh. Never revives a
    /// key whose tombstone is newer than the request (MA-4).
    pub fn apply(&mut self, batch: SnapshotBatch, origin: ApplyOrigin) -> ApplyOutcome;
    pub fn invalidate(&mut self, k: NodeKey, mask: PropMask, at: Instant) -> bool;
    /// O(1): every node confirmed before now is Stale(ALL) (flood resync, dirty-all).
    pub fn invalidate_all(&mut self, at: Instant);
    pub fn tombstone(&mut self, k: NodeKey, at: Instant, why: RemovalCause);
    /// Forget a live node (bounds). The key reads `Miss`, never `Gone`.
    pub fn evict(&mut self, k: NodeKey) -> bool;
    /// Bounded work per call (at most `SWEEP_BUDGET` entries): tombstone TTL, size caps.
    pub fn sweep(&mut self, now: Instant) -> SweepStats;
    /// App gone: drop everything; readers get `Gone` for every key of the app afterwards.
    pub fn retire_all(&mut self);
    /// Counters equal real sizes (MA-1). `debug_assert` after every mutation in debug builds.
    pub fn check_invariants(&self) -> Result<(), MirrorInvariantError>;
}
pub enum ApplyOrigin { Reply { issued_at: u64 }, Push, CacheDump { issued_at: u64 } }
pub enum RemovalCause { Event, Defunct, UnknownObject, CacheSignal, WindowDestroyed }
pub struct MirrorStats { pub live: u64, pub tombstones: u64, pub stale: u64, pub bytes: u64,
                         pub partial: u64, pub evictions: u64, pub revivals: u64 }
```

### 4.3 Connector API additions (`gorca-connector-api`, PW-5)

```rust
pub enum BackendOp {
    // existing: Fetch, Children, Subtree, Text, Action, HitTest, Collection, Probe
    /// Children of `node` in `range`, each as a snapshot with at least `want`.
    ChildSnapshots { node: NodeKey, range: Range<u32>, want: PropMask },
    /// `node`, its parent, its parent's parent ... up to `max_depth` (≤ 64) or the app root;
    /// stops at a cycle, a foreign reference or a `Document*` role.
    Ancestry { node: NodeKey, max_depth: u8, want: PropMask },
    /// Whole-app bulk dump (AT-SPI: `Cache.GetItems`). `NotSupported` when the app has none.
    CacheDump { max_nodes: u32 },
}
// Replies: ChildSnapshots/Ancestry/CacheDump -> BackendReply::Subtree(SnapshotBatch) (partial
// nodes carry their `have`), with `truncated: bool` added to `SnapshotBatch`.
pub enum BackendError { /* existing */ Unresolved }   // key has no known path (5.5): never `Gone`
```

### 4.4 Fill policy (`gorca-mirror::fill`, PW-3): **this is what each arm implements**

A policy is a pure, synchronous state machine. It never performs I/O, never blocks, never awaits.
The shared driver (4.5) feeds it inputs and executes its effects within the bounds of section 7.

```rust
pub trait FillPolicy: Send + 'static {
    fn name(&self) -> &'static str;
    /// One step. Must be O(input + effects) and bounded by MA-6 (≤ 250 µs at the caps).
    fn step(&mut self, cx: &FillCtx<'_>, input: FillInput<'_>, out: &mut FillEffects);
    /// Exact bytes of policy-owned memory (MA-1). Bounded by FILL_STATE_MAX_BYTES.
    fn memory_bytes(&self) -> usize;
}

pub struct FillCtx<'a> {
    pub now: Instant,
    pub app: AppId,
    pub app_class: AppClass,            // Shell | Toolkit(GenericToolkit) | Browser | Terminal | Unknown,
                                        // from AppInfo; budget tuning only, never per-app behaviour
    pub health: HealthView,             // Healthy | Slow | Hung | Flooded | Gone
    /// Shared eligibility (2.2): the connector verified that `CacheDump` returns the whole tree
    /// for this app. Today true only for a verified gnome-shell.
    pub dump_complete: bool,
    pub mirror: &'a dyn MirrorRead,
    pub focus: Option<NodeKey>,         // the Reader's settled focus if it is in this app
    pub slots: SlotView,                // free in-flight slots per class, rate-limit tokens left
    pub limits: &'a FillLimits,         // section 7 values
}

pub enum FillInput<'a> {
    Appeared { root: NodeKey },
    Ready,                              // AppLifecycle::Ready
    /// A drained ingress batch, **after** the shared invalidation of 5.1 was applied.
    Events(&'a [NodeEvent]),
    Resync { dirty: &'a DirtySet },     // flood resync (already invalidated)
    /// The Reader read the mirror for a reason; `outcome` says what it found.
    Demand { node: NodeKey, why: DemandWhy, outcome: DemandOutcome },
    FocusSettled { node: NodeKey },     // FocusSM reached Presented
    WindowActivated { window: NodeKey },
    /// A fill request finished; the driver has already applied it to the mirror.
    Reply { id: FillReqId, op: &'a FillOp, result: FillResult<'a> },
    Tick,                               // at most every 50 ms while the app has work, else on WakeAt
}
pub enum DemandWhy { Focus, StateChange, WhereAmI, NavStep, Revalidate }
pub enum DemandOutcome { Hit, Partial { missing: PropMask }, Stale { mask: PropMask }, Miss, Gone }

pub enum FillEffect {
    Request { op: FillOp, class: BudgetClass },   // driver assigns id + deadline, applies the reply
    Cancel(FillReqId),
    Evict(NodeKey),
    WakeAt(Instant),
}
pub enum FillOp {
    Fetch { node: NodeKey, want: PropMask },
    ChildSnapshots { node: NodeKey, range: Range<u32>, want: PropMask },
    Ancestry { node: NodeKey, max_depth: u8, want: PropMask },
    Subtree { root: NodeKey, max_nodes: u32 },
    CacheDump { max_nodes: u32 },
}
pub type FillEffects = SmallVec<[FillEffect; 8]>;
```

Arm constraints on effects: arm A may use `Fetch`, `ChildSnapshots` and `Ancestry` only on demand
(in reaction to `Demand`), never on `Events`/`FocusSettled`/`WindowActivated`/`Tick` except to
refetch the focused node; arm C may use `CacheDump` **only** when `cx.dump_complete` is true (the
driver refuses it otherwise, `fill_refused{dump_ineligible}`). Everything else is free.

### 4.5 The shared driver (`gorca-mirror::driver` + app-actor wiring in `gorca-core`, PW-3/PW-4)

Owned by the harness, identical for every arm. Per app, inside the app actor's pump:

1. drains the ingress batch, applies the **invalidation table** (5.1) to the mirror;
2. routes events to the Reader as today, plus S1 (5.6);
3. calls `policy.step(Events | Resync | ...)`;
4. turns `Request` effects into `BackendRequest`s: assigns `FillReqId`, `issued_at = writer.seq()`,
   deadline from the class, deduplicates identical in-flight ops, enforces section 7 (in-flight per
   class, rate limit, node and byte caps); a refused effect is counted (`fill_refused{reason}`) and
   reported back as `FillResult::Refused(reason)`;
5. applies replies with `ApplyOrigin::Reply { issued_at }`, maps `Gone`/`UnknownObject` to a
   tombstone, `Timeout` to a health penalty, then feeds `FillInput::Reply`;
6. runs RV-1 (5.3) and `sweep` with a bounded budget.

The Reader's own miss path (Interactive `Fetch` on a focus miss, `present/flow.rs`) stays as in
Phase 1 and is not routed through the policy, so the cold path is identical for all arms; the
driver tells the policy about it with `Demand` and applies its reply to the mirror.

### 4.6 Connector side (`gorca-connector-atspi`)

* `fetch/` (shared skeleton, PW-5/PW-7): request service, deadlines, `MAX_IN_FLIGHT`, path
  resolution, the `GetItems` decoder `fetch/items.rs` (untrusted input, fuzzed, bounded; section 7).
* `fetch/strategy.rs` (**arm-owned**): how each `BackendOp` above is executed: which AT-SPI calls,
  pipelining over the app's connection, `Properties.GetAll` versus per-property calls, the order of
  sub-calls, splitting large ops. Fixed signatures:

```rust
pub(crate) async fn fetch(cx: &CallCx, node: &Resolved, want: PropMask, dl: Deadline) -> Result<RawNode, BackendError>;
pub(crate) async fn child_snapshots(cx: &CallCx, node: &Resolved, range: Range<u32>, want: PropMask, dl: Deadline) -> Result<RawBatch, BackendError>;
pub(crate) async fn ancestry(cx: &CallCx, node: &Resolved, max_depth: u8, want: PropMask, dl: Deadline) -> Result<RawBatch, BackendError>;
pub(crate) async fn subtree(cx: &CallCx, root: &Resolved, max_nodes: u32, dl: Deadline) -> Result<RawBatch, BackendError>;
pub(crate) async fn cache_dump(cx: &CallCx, max_nodes: u32, dl: Deadline) -> Result<RawBatch, BackendError>;
```
  `CallCx` carries the app's call connection (P2P, same-bus or bus), the per-app in-flight
  semaphore and the path table. A strategy may issue several calls concurrently but never more
  than its op's share of `MAX_IN_FLIGHT` (section 7), and every call goes through
  `gorca_dbus::call_method` with the op's deadline (D-4).
* `events/subscription.rs` (**arm-owned**): the arm's extra subscription, from a closed whitelist:

```rust
pub(crate) fn extra_subscription() -> Subscription;   // default: Subscription::NONE
pub struct Subscription { pub cache_signals: bool }    // adds interface='org.a11y.atspi.Cache'
```
  The base set (`REGISTERED_EVENTS`, `default_match_rules()`) is shared and unchanged.

### 4.7 Pre-arm shared work (must be on `main` before arms start)

| # | Work | Owner | Notes |
|---|---|---|---|
| PW-1 | `gorca-mirror` store on `scc::HashIndex` with the 4.2 API, tombstone TTL, eviction, stale epoch, `check_invariants` | rust-engineer | port of MB-1's `scc-index` cell; keeps the existing tests |
| PW-2 | `NodeSnapshot.have`, `confirmed_seq`, `Freshness` representation change, `PropMask::PARENT`, `NodeView` | rust-engineer | ADR-0001 §4.2/§4.4 changelog entry (architect) |
| PW-3 | `FillPolicy` trait, driver, `NullPolicy` (R0, section 9) | rust-engineer | pure, `step()` tests per PR-4 |
| PW-4 | app actor and Reader wiring: invalidation table, RV-1, S1 routing, Gone-is-not-final focus rule (5.5), cache-served presentations tagged `may_refine` | rust-engineer | `gorca-core/src/app_actor.rs`, `present/` |
| PW-5 | `BackendOp::{ChildSnapshots, Ancestry, CacheDump}`, `BackendError::Unresolved`, `SnapshotBatch.truncated`; connector skeleton with a naive reference `strategy.rs` | rust-engineer | the naive strategy is R0's |
| PW-6 | P2P pool in `gorca-dbus`: lazy connect with `Startup` deadline, modes `Peer` / `SameBus` (G-4: address equals the a11y bus address → reuse the call connection) / `None` (A-3: empty) / fallback on EOF | linux-platform-engineer | D-7 |
| PW-7 | Path table for mirrored keys: per-owner quota ≥ `MAX_MIRROR_NODES`, global byte cap 64 MiB, paths recorded from replies (refs in children, ancestry, dumps), eviction yields `Unresolved` | rust-engineer | today's quota is 1 024 per owner, focus paths only |
| PW-8 | Drain mapping of cache signals (ADR-0004 mapping-table amendment): `Cache.RemoveAccessible (so)` → `NodeRemoved{hash(path)}`; `Cache.AddAccessible` → `ChildrenChanged{hash(parent path)}` (only the first three refs are read); **a foreign bus name in any ref read (Remove's ref; Add's object or parent ref) is malformed: no event, counted in `DrainStats::malformed`** (decided 2026-10-13; ADR-0004 D-7; IC-16 asserts the count); match rule added only when `Subscription.cache_signals` | rust-engineer | conformance C-D tests extended; B-2 model updated |
| PW-9 | `GetItems` decoder `fetch/items.rs`: streaming over the reply, caps of section 7, foreign refs dropped, fuzz target | rust-engineer + test-engineer | MA-7 |
| PW-10 | Presentation log (`GORCA_PRESENT_LOG=<path>`): one JSON line per utterance and per refine (section 6.1) | rust-engineer | passive; compiled in all builds; no behaviour depends on it |
| PW-11 | Shadow mode (`GORCA_SHADOW=1`): read-only call allowlist (section 8.4) | rust-engineer + linux-platform-engineer | for the optional live run |
| PW-12 | S1 presenter: state/value change of the focused node (checked, expanded/collapsed, selected, pressed, sensitive, value) | rust-engineer | M2 scope, arm-independent |

## 5. Invalidation contract (normative, shared, identical for every arm)

### 5.1 Event → mirror effect (applied in the same pump, before the Reader sees the batch)

| `NodeEvent` (after ADR-0004 mapping) | Mirror effect |
|---|---|
| `PropertiesChanged{n, mask}` | `invalidate(n, mask ∩ (NAME∣DESCRIPTION∣VALUE∣ROLE∣STATES∣ATTRS))`; `BOUNDS` ignored (M2 scope) |
| `PropertiesChanged{n, mask ∋ NAME}` (P9-3) | additionally `invalidate(a, NAME)` for each known ancestor `a` of `n` up to and including the first whose role does not take its name from content, at most 8 levels |
| `ChildrenChanged{n}` | `invalidate(n, CHILDREN)` |
| `NodeRemoved{n}` | `tombstone(n, Event)`; `invalidate(parent(n), CHILDREN)` if the parent is known |
| `TextChanged{n, _}` | `invalidate(n, NAME∣VALUE)` (St/GTK labels derive names from text) |
| `ActiveDescendantChanged{c, _}` | `invalidate(c, STATES)` |
| `WindowEvent{n, Destroy}` | `tombstone(n, WindowDestroyed)` |
| `WindowEvent{n, Activate∣Deactivate∣Create}` | `invalidate(n, STATES∣NAME)` |
| `FocusChanged`, `CaretMoved`, `SelectionChanged`, `DocumentEvent`, `Announcement` | none |
| Flooded batch with `DirtySet` of nodes | `invalidate(k, ALL)` for each |
| Flooded batch with dirty-all | `invalidate_all()` (O(1)) |
| reply `Gone` / `UnknownObject` / `defunct` state in a reply | `tombstone(k, UnknownObject∣Defunct)` |
| `AppLifecycle::Gone` | `retire_all()`; the policy gets no further input except `Gone` |

### 5.2 Reading

`view(k)` returns `stale = node.stale_mask ∪ (ALL if node.confirmed_seq < app_stale_epoch)`.
A read is a **hit** for a purpose when `have ⊇ need` and `stale ∩ need = ∅`, where
`need = PRESENT` for focus (minus `VALUE` if the node has no `Value` interface; plus `RELATIONS` if
the name is empty). A partial or stale read may be presented (tagged `may_refine`) or treated as a
miss: that is the Reader's shared rule, not the arm's: **stale or partial → present what is known
tagged `may_refine` only if the missing/stale fields are not NAME or ROLE; otherwise fetch
(Interactive) and present the reply.**

### 5.3 RV-1 revalidation of what was presented

Every presentation served from the mirror (focus, S1, where-am-I) is followed by exactly one
`Fetch{node, PRESENT}` with class Interactive, unless the presented snapshot was confirmed by a
request issued after the triggering event. If the reply differs in a presented field, the Reader
refines (ADR-0001 §8.1: replace if the epoch is unchanged and < 150 ms of audio was played),
otherwise speaks a short follow-up with the changed field. Both count as **corrections** (M-6).

Why it is mandatory: G-1, G-6, S-1 change names silently, and replies (P2P) and events (bus) travel
on different connections with no mutual order, so even an evented mutation can be presented
before its event is drained. RV-1 makes `stale = 0` reachable for every arm; the arms compete on
how rarely it has to correct (M-6) and on what it costs (msgs/s).

### 5.4 Ordering rules (the late-reply and re-dirty rules)

* **LR-1** A reply to a request issued at seq `s` never resurrects a key tombstoned at seq `t > s`.
* **LR-2** A reply to a request issued at seq `s` for a node invalidated at seq `t > s` is stored
  but stays `Stale` (ADR-0001 §4.4 "Fresh, or Stale if re-dirtied").
* **LR-3** A reply to a request issued **after** the tombstone (`s > t`) that succeeds is proof of
  life: the key is revived (`MirrorStats.revivals`). This is I-1-safe because GNOME toolkits never
  reuse paths (G-5, A-1); it is what makes reorders (`remove` + `add` of the same path) work.
* **LR-4** `CacheDump` applies per node with the same rules; a node absent from a dump is not
  removed by the dump (G-3: the dump may be incomplete), only by an event, a signal or a failed fetch.

### 5.5 Focus on a removed key is not final

`FocusChanged` for a key that reads `Gone` triggers one Interactive proof-of-life `Fetch`
(LR-3) instead of dropping the focus (today's `present/flow.rs` drops it). `UnknownObject` keeps
it dead and the FocusSM handles the removal. `Unresolved` (path evicted, PW-7) is treated as a
miss, never as `Gone`.

### 5.6 S1: state change of the focused node

`PropertiesChanged{n, STATES∣VALUE}` with `n == focus` goes to the focus lane; the Reader issues a
Fetch(STATES∣VALUE∣NAME) Interactive and speaks the change. Gate S1 ≤ 5 ms p99 gorca-side.

### 5.7 Invalidation conformance suite (`gorca-testkit`, test-engineer, H-8)

Each case runs against the fake app (section 11) with R0 and each arm; each presentation gets a
verdict (6.2). Required verdicts: **no `STALE` anywhere**; for evented mutations settled ≥ 50 ms
before the focus, `CORRECT` or `REFINED`; for silent mutations any non-`STALE` verdict. Each case
also asserts that it exercised what it claims (the stale value was in the mirror before the
mutation, read through a test-only `MirrorRead` probe), so a case cannot pass vacuously.

| Case | Script | Asserts |
|---|---|---|
| IC-1 rename, evented | rename N (not focused) with `accessible-name`; focus N | new name |
| IC-2 rename, silent | GTK4 computed name (G-1) and St `label_actor` (S-1): rename without event; focus N; also rename the focused N | corrected within the RV-1 rules; counted |
| IC-3 state flip | checked, expanded/collapsed, selected, sensitive on focused N (S1 path) and on non-focused N | S1 speech ≤ 5 ms; later focus correct |
| IC-4 value | slider/spin value change on focused N | S1 speech with the new value |
| IC-5 subtree replace | Settings panel switch: remove old panel root (events), add new subtree; variant without remove events (G-2) | no presentation of old nodes; parent CHILDREN stale; new nodes correct |
| IC-6 app restart | kill Settings mid-walk (bus name vanishes), restart with the same paths and a new bus name | no presentation from the old AppId; old mirror, paths and policy state released (MA-2) |
| IC-7 defunct | `state-changed:defunct` on N and on the focused node; `UnknownObject` reply (A-2 lease expiry) | tombstoned; focus SM handles it; nothing spoken from the dead key |
| IC-8 reorder | (a) `children-changed:remove`+`:add` of the same paths (H3); (b) only `children-changed` on the parent; (c) silent | reordered child still focusable and spoken (LR-3); CHILDREN stale |
| IC-9 recycled rows | Files scroll: rows keep paths, names change (silent or evented per H4) | focus on a recycled row never `STALE` |
| IC-10 late reply | reply delayed past a removal (LR-1); reply issued after removal (LR-3) | no ghost; revival only with proof of life |
| IC-11 reply/event race | name changed while a Fetch is in flight: event-before-reply, reply-before-event, pre-mutation reply delayed on P2P | never `Fresh` with the old value (LR-2) |
| IC-12 hidden ghost | GTK4 node hidden then destroyed without `RemoveAccessible` (G-2/H1) | never presented; an eager mirror must not keep it as live forever (tombstone on failed fetch; aged by TTL) |
| IC-13 add during dump | node added while `GetItems` runs, no `AddAccessible` (G-3/H2) | focus on it works (miss path) |
| IC-14 Shell rebuilds | Alt+Tab switcher and a popup menu rebuilt per open (S-2) | selections spoken correctly; dead switchers not prefetched after `WindowEvent::Destroy` |
| IC-15 window destroy | window destroyed, focus moves | no presentation of descendants |
| IC-16 foreign refs | GetItems/relations/children referencing another bus name; self-parent; cycles | dropped, counted; no cross-app write (MA-3) |
| IC-17 flood | background app at 50k ev/s → `Flooded` + dirty set | dirty nodes Stale, not dropped; refetch rate-limited (section 7); focused app's L1h unaffected |
| IC-18 hung app | app stops answering mid-walk and mid-`GetItems` | from-cache presentations tagged `may_refine`; `Hung` after 3 timeouts; only probes afterwards (I-4) |
| IC-19 P2P modes | `Peer`, `SameBus` (defensive, G-4), `None` (GTK4 per P-9, Qt, A-3), P2P EOF mid-request, P2P connect hang | all work; fallback within `Startup` deadline; no duplicate connection in `SameBus` |
| IC-20 document boundary | Firefox-chrome model with a `DocumentWeb` child | no fill request below the document node |
| IC-21 listener gate | GTK4 model emits no `Object` event before `RegisterEvent` (P9-1); mutate, register, focus | nothing presented from pre-registration state without RV-1 |
| IC-22 dump eligibility | (a) a model that claims `ToolkitName = "gnome-shell"` and name "GNOME Shell" without owning `org.gnome.Shell`; (b) the verified Shell returning a 2-item dump | (a) `dump_complete = false`, no `CacheDump` sent; (b) flag cleared, lazy fill, no `STALE` |
| IC-23 TTL | node confirmed, no event for TTL + 1 s, silent rename, focus it and speak it as where-am-I context | presented `may_refine` and revalidated; idle run shows no TTL-driven calls |

## 6. Measurement definitions

### 6.1 Truth, presentations, messages

* **Truth log**: every fake app writes `{t_ns, app, path, field, old, new, evented}` for each
  mutation, and `{t_ns, focus, path}` for each focus change, `CLOCK_MONOTONIC`.
* **Presentation log** (PW-10): `{t_ns, utt_id, epoch, app_bus, path, source: hit|partial|stale|miss_reply|refine|followup, role, states, name, value, may_refine, refines: utt_id?}`.
  The path comes from the connector's path table through a test-only debug accessor; the core
  still holds no paths. Mock speech server timestamps cross-check `t_ns` (must agree within 1 ms).
* **Messages**: the fake apps count every method call they serve, per member and per connection
  (bus, same-bus, peer); signals delivered to gorca are `DrainStats.messages` cross-checked against
  a monitor on the private bus counting signals that match gorca's declared match rules. A
  disagreement > 1 % invalidates the run. `msgs = 2 × calls + signals_received`.

### 6.2 Verdict per presentation

For the node presented, let `V` be the set of truth versions valid at some instant in
`[t_stimulus, t_speak]` (the stimulus is the focus or state event):

| Verdict | Condition |
|---|---|
| `CORRECT` | first utterance matches a version in `V` on name, role, presented states, value |
| `REFINED` | it does not, and a `refine` replaces it under the §8.1 rule |
| `LATE` | it does not, and a `followup` with the right value comes within 500 ms |
| `STALE` | none of the above |

`corrections = REFINED + LATE`. M-6 rate = corrections / focus presentations.

## 7. Hard resource bounds (hostile input)

Every bound is a hard requirement for every arm; overflow degrades to "less cached" (Miss), never
to unbounded growth, a wrong presentation, or starvation of the Interactive class. Constants live in
`crates/gorca-mirror/src/limits.rs` (shared); tests in `crates/gorca-mirror/tests/bounds.rs` and
`crates/gorca-bench-b1/tests/`.

| Bound | Value | Beyond it | Test |
|---|---|---|---|
| Live nodes per app mirror (`MAX_MIRROR_NODES`) | 65 536 | Background/Prefetch fills refused; Interactive replies evict a non-focus, non-ancestry node (policy's `Evict` hints first, else oldest `confirmed_seq`) | MB-B1 |
| Mirror bytes per app | 64 MiB (names, descriptions clipped at `MAX_TEXT_BYTES` 2 048) | same as above | MB-B2 |
| Mirror bytes, all apps (`GLOBAL_MIRROR_BYTES`) | 256 MiB | evict from the largest non-focused, non-Shell app first (preempt the largest holder; ADR-0004 style); the focused app and Shell keep ≥ 4 096 nodes each | MB-B3 |
| Tombstones per app | ≤ 2 × live + 4 096, TTL 10 s | oldest swept first | MB-B4 |
| In-flight requests per app | `MAX_IN_FLIGHT` 8; Interactive has 2 reserved; Background (incl. `CacheDump`) ≤ 2 | `Busy`/`Refused`; Interactive never waits for a Background slot | MB-B5 |
| Pending policy requests per app | 256 queued effects | newest Prefetch/Background refused, counted | MB-B6 |
| Fill rate per app (Prefetch + Background calls) | 500 calls/s non-focused app, 2 000 calls/s focused app; 5 000 calls/s all apps | token bucket; refused effects reported to the policy | MB-B7, IC-17 |
| `CacheDump` | ≤ 1 per app per 30 s, `max_nodes` ≤ 65 536, reply ≤ 16 MiB, decoded in a `cpu` job | larger reply rejected whole (`Protocol`), counted; arm falls back to lazy | MB-B8, MA-7 |
| `Subtree` / `ChildSnapshots` | ≤ 4 096 nodes per op; ranges ≤ 1 024 children | truncated (`SnapshotBatch.truncated`) | MB-B9 |
| `Ancestry` depth | ≤ 64; cycle or foreign ref stops it | truncated | MB-B9, IC-16 |
| Child count trusted from the app | `min(reported, 65 536)`; negative → 0 | clipped | MB-B9 |
| Path table | per owner ≥ `MAX_MIRROR_NODES`; global 64 MiB; path ≤ 256 B | owner's own entries evicted; key becomes `Unresolved` (not `Gone`) | PW-7 tests |
| Policy state (`FILL_STATE_MAX_BYTES`) | 4 MiB per app | policy must shed its own state; checked by `memory_bytes()` vs a real allocation count in tests | MB-B10 |
| Policy step | ≤ 250 µs worst case at the caps (65 536 nodes, dirty-all, 4 096-event batch) | — | MA-6 |
| Counters vs structures | exact | — | MA-1 |

## 8. Workloads

### 8.1 App models (fake apps, H-2), calibrated by the census (H-1) and P-9

Live numbers from P-9 (`docs/reports/2026-10-13-p9-cache-p2p.md`) replace the provisional
`GetItems` and P2P columns; the census (H-1) still replaces node counts, depths and event patterns,
and the table is then regenerated from `tests/harness/b1/models/*.json`.

**P-9 calibration (live, this host):**

| App | Reachable by walk | `GetItems` | Dump time | P2P | Call service time |
|---|---|---|---|---|---|
| gnome-shell | 1 765 | 1 765 (complete) | 17.0 ms (~10 µs/item) | `Peer` | `GetChildren` p50/p99 0.108/0.337 ms P2P, 0.140/1.088 ms bus |
| gnome-terminal (GTK3) | 173 | 170 | — | `Peer` | `Properties.Get` p50 0.12–0.15 ms, p99 0.6–1.5 ms (all apps) |
| gedit (GTK3) | 488 | 181 (63 % missing: list boxes, tree views, popovers, menus) | — | `Peer` | as above |
| Nautilus (GTK4) | ≥ 307 (bounded walk) | 2 when fresh; 307 after a client walk realised them | 13.8 ms for 307 (~45 µs/item) | `None` | as above |

| Model | Toolkit profile | Nodes (idle / peak) | Depth | Event pattern | Service time per call (p50 / p99) | GetItems |
|---|---|---|---|---|---|---|
| `shell` | atk-bridge (A-1…A-3, S-1, S-2), P2P `Peer` (or `None` variant, A-3); owns `org.gnome.Shell` on the harness session bus (2.2 eligibility) | **1 765** / 3 000 (overview with app grid and search results) | 12-20 | clock rename every 60 s (evented); overview open/close: 1-2k `children-changed` + `state-changed:showing` within 250 ms; switcher rebuild per Alt+Tab; notifications | 0.11 / 0.34 ms P2P; 0.14 / 1.1 ms bus | **complete**, 10 µs/item (17 ms for 1 765), served on the app's serial loop |
| `settings` | GTK4 (G-1…G-6, P9-1…P9-4), P2P **`None`** | 800 / 2 000 per panel; sidebar ~45 rows | 15-25 | panel switch = subtree replace (300-1 500 nodes); toggles, sliders, combo rows; computed-name changes silent; `RemoveAccessible` never sent; spurious parent `ChildrenChanged:add` on child updates | 0.13 / 1.0 ms | realised contexts only (2 when fresh), 45 µs/item; hidden contexts kept as `SHOWING` ghosts |
| `files` | GTK4 (Nautilus), P2P **`None`** | 2 when fresh → ≥ 307 realised; ~60 realised rows of 6 nodes; `files-big` variant: 10 000-entry directory, rows recycled | 15-25 | scroll = row recycling (G-6); rename (evented on the label, silent on the row); folder open = subtree replace | 0.13 / 1.0 ms | as `settings` |
| `gtk3` | atk-bridge, calibrated on gedit: 488 reachable, 181 in `GetItems`; `MANAGES_DESCENDANTS` list/tree, menus with child count −1; P2P `Peer` | 488 / 800 | 10-18 | `RowInserted/RowDeleted` instead of `ChildrenChanged` in lists; tooltip changes silent | 0.13 / 1.0 ms | partial (eligibility must stay false; used in `ff` and IC-22) |
| `storm` | raw | — | — | terminal-style text flood 10k / 50k ev/s (L3) | — | — |
| `hung` | any | — | — | stops answering on command | ∞ | — |
| `ff-chrome` | Gecko chrome with one `DocumentWeb` child (IC-20 only); P2P `Peer` | 300 | — | — | 0.15 / 1.0 ms | 2 items (Firefox live: root + frame) |

Every app model is served **serially** (D-1): one request at a time, in arrival order, across all
its connections, with the service-time distribution above plus GetItems' per-item cost.

### 8.2 The scripted walk W-1 (deterministic, seeded)

Human pacing: 150-400 ms between steps (uniform), 30 Hz bursts for held arrows (3 s), 2-5 s pauses
every ~20 steps. One **loop** (about 6 minutes):

1. Shell: Super (overview opens: storm of children/showing events), type "set" (search results
   replaced 3 times), Enter → `settings` appears; first focus 50 ms after `Ready` (variant 0 ms).
2. Settings: Tab through the header and sidebar, arrow down 20 sidebar rows, Enter on 5 panels in
   turn (subtree replace each), Tab through each panel's controls, toggle 3 switches (S1), move a
   slider 5 steps (S1), rename-type changes (silent G-1 on 10 % of visited controls between visits),
   "where am I" on 5 controls (ancestry).
3. Alt+Tab (switcher rebuild, `selected` toggles) → `files` appears (first focus).
4. Files: Tab to the list, arrow through 200 entries (recycling), PageDown ×5, rename one entry (F2),
   open a folder (subtree replace), Back, Ctrl+L, Escape.
5. Alt+Tab to Settings: **revisits** of 30 controls seen in step 2, half of them mutated meanwhile
   (evented 70 %, silent 30 %).
6. Every third loop: kill and restart `settings` (IC-6 path) before step 5.

Every loop also runs: 1 of 3 loops under L3-50k (a `storm` app floods in the background), and in one
loop the `hung` model takes 2 s to answer anything and then recovers (L4 isolation).

The driver reports per run: focus presentations, revisits, evented and silent mutations at focus,
S1 stimuli, ancestry demands, app starts. A run with fewer than the scripted counts is invalid
(guards against a policy or harness bug making steps vanish).

### 8.3 Runs

| Run | Content | Duration | Repeats |
|---|---|---|---|
| `walk` | W-1 × 3 loops (one of them under L3-50k, one with `hung`) | ~20 min | 5, interleaved across arms (round-robin, rotated each round) |
| `ff` | first focus only: each model (`shell`, `settings`, `files`, `files-big`, `gtk3`) appears 50 times with first-focus delays {0, 50, 300} ms | ~5 min | 5, interleaved |
| `idle` | after one loop, 120 s with no input; clock rename only | 2 min | 3 |
| `big` | three 20 000-node apps (`settings` scaled) + walk inside them, for RSS | 10 min | 3 |
| `soak` | W-1 repeated for 1 h | 60 min | 1 per arm (reviewer) |
| `conf` | IC-1…IC-23, MA-1…MA-10, bounds | ~10 min | debug and release, 3× for flaky detection |
| `step` | in-process policy-step microbench at the caps (MA-6) | 1 min | 7 rounds, median |

### 8.4 Optional live read-only run (information only, never gating)

On the user's real GNOME 48 desktop, the two arm daemons run **simultaneously in shadow mode**
(PW-11) beside the user's normal screen reader while the user follows the W-1 checklist by hand
(`docs/bakeoff/b1-mirror-fill/harness/live-checklist.md`, ~15 min). One manual walk gives paired
data for all arms.

Shadow mode: no `org.gnome.Orca.KeyboardMonitor`, no `org.a11y.Status` writes, no
`RegisterEvent`/`DeregisterEvent`, no `Action`/`EditableText`/`Selection`/`Component.GrabFocus`,
speech to a null sink; allowed calls: `Accessible.*` getters, `Properties.Get/GetAll`,
`Cache.GetItems`, `Application.GetApplicationBusAddress`, `Text.GetText` of the focus,
`AddMatch`. The allowlist is enforced in the connector and audited from a `dbus-monitor` capture
of the a11y bus; any call outside it aborts the run.

Reported per arm: hit rate, first-focus latency, own calls/s, RSS, RV-1 disagreements (which
measure the real rate of silent mutations: the best calibration input for G-1/S-1), P2P mode per
app. The two daemons load the apps twice; latency numbers are relative only.

## 9. Metrics and gates

All latencies are gorca-side (bus arrival at the tap → SPEAK at the mock), `CLOCK_MONOTONIC`.
A metric passes when the **median over repeats** meets the threshold; min and max are reported.
Thresholds live in `crates/gorca-bench-b1/src/limits.rs`; this table is generated from it.
**R0** = `NullPolicy` + the shared base (every focus miss fetches; nothing prefetched; the cache
serves revisits). It validates the harness and is the baseline for "unchanged" gates.

| Id | What | Method | Pass |
|---|---|---|---|
| **B1-STALE@{walk,conf,soak}** | uncorrected wrong presentations | verdicts (6.2) | **0** |
| **B1-LOST@walk** | focus stimuli with no SPEAK | e2e correlation | **0** |
| **B1-CONF** | IC-1…IC-23, MA-1…MA-10, bounds | `cargo test -p gorca-testkit -p gorca-mirror -p gorca-bench-b1` debug + release, 3× | all pass |
| **B1-L1h.p99** | focus → SPEAK when served from the mirror (`source=hit`) | walk | **≤ 2 ms** (target 1 ms); p50 reported |
| B1-HIT.all | `source ∈ {hit}` / focus presentations | walk | rank (higher) |
| **B1-HIT.revisit** | hit rate on revisits of nodes not mutated since the last visit | walk | **≥ 0.95** (guards against a policy that never caches) |
| B1-HIT.ancestry | where-am-I demands answered without IPC | walk | rank |
| **B1-L1c.p99** | focus → SPEAK on a miss | walk | ≤ 15 ms and ≤ 1.2 × R0's median (cold path unchanged) |
| **B1-FF.p99@{shell,settings,files,files-big,gtk3}** | first focus of a newly appeared app → SPEAK | `ff` | **≤ 15 ms** each; rank on the worst p50 and p99 |
| **B1-S1.p99** | state change on focused node → SPEAK | walk | **≤ 5 ms** |
| **B1-M6** | corrections / focus presentations | walk | **< 1 %**; split evented / silent reported |
| **B1-MSG.idle** | gorca-originated calls/s, 60 s after settle | `idle` | **≤ 0.5 calls/s per app** (no polling); signals/s reported |
| B1-MSG.walk | msgs/s and calls per focus presentation | walk | rank (lower) |
| **B1-MSG.l3** | calls/s to the `storm` app during L3-50k | walk | ≤ 500 calls/s (fill rate bound); L3 lost = 0 |
| B1-MSG.startup | calls and bytes in the first 2 s after an app appears | `ff` | rank (info) |
| **B1-RSS.soak** | daemon RSS growth from minute 10 to minute 60 | `soak` | **≤ 8 MiB** |
| **B1-RSS.big** | RSS peak minus RSS before the apps appeared | `big` | **≤ 128 MiB** (M-8 share) ; rank |
| **B1-L4** | `hung` model: L1 of the other apps; "not responding" spoken | walk | L1 p99 ≤ 15 ms elsewhere; announce ≤ 500 ms (M-4) |
| **B1-STEP.max_us** | worst policy `step()` at the caps | `step` | **≤ 250 µs** (MA-6) |
| B1-CPU.focus_us | daemon CPU per focus presentation (all threads, schedstat) | walk | rank (info) |

**Gate.** An arm is rankable only if it passes every bold row. **Ranking** among passing arms:
B1-HIT.all and B1-FF (worst app) first, then B1-MSG.walk, B1-RSS.big, B1-M6, B1-CPU, then code
size, then the reviewer's judgment of correctness risks the harness cannot see.

### 9.1 Audit requirements (binding on every arm; tests written before arms start, `#[ignore]` where R0 cannot exercise them)

| # | Severity | Requirement | Test |
|---|---|---|---|
| MA-1 | High | `MirrorStats` and `memory_bytes()` equal the real sizes after every apply, invalidate, tombstone, evict, sweep, retire | `mirror_counters_exact`, `policy_memory_accounting` |
| MA-2 | High | App `Gone` releases mirror, tombstones, paths and policy state; RSS of 100 app start/stop cycles of a 5 000-node app returns within 4 MiB | `audit_app_churn_releases_everything` |
| MA-3 | High | No reply, dump item or relation ever writes a key of another app; foreign refs are dropped and counted | `audit_foreign_refs_rejected` |
| MA-4 | High | LR-1: no ghost revival; LR-3 revival only by a request issued after the tombstone | `audit_late_reply_never_revives`, `audit_proof_of_life_revives` |
| MA-5 | High | LR-2: a node invalidated during its fetch never becomes `Fresh` with pre-invalidation content | `audit_redirty_during_fetch` |
| MA-6 | Medium | `step()`, `apply()` of a 4 096-node batch on the app actor, and `sweep()` each ≤ 250 µs worst case at the caps; dump decoding runs on `cpu` | `audit_step_bounded`, `audit_apply_bounded` |
| MA-7 | High | The GetItems decoder never panics and respects every cap: 10 min `cargo fuzz` (or proptest) on arbitrary bytes and on mutated real dumps | `fuzz_get_items`, `audit_dump_caps` |
| MA-8 | High | Interactive requests never wait behind Background/Prefetch in gorca (reserved slots); measured: Interactive fetch issued while 2 dumps of 65 536 items are in flight on other apps completes within its service time + 1 ms | `audit_interactive_not_starved` |
| MA-9 | Medium | Policies stop Prefetch/Background within one step of `HealthView::Slow`/`Hung`, and send nothing to a `Hung` app (I-4) | `audit_policy_respects_health` |
| MA-10 | Medium | Shadow mode issues no call outside the allowlist, whatever the arm does | `audit_shadow_allowlist` |

## 10. Constraints, ownership and budgets

**Files an arm may change** (everything else is harness-owned; a needed change goes to the
orchestrator):

* `crates/gorca-mirror/src/fill/candidate.rs` and `crates/gorca-mirror/src/fill/candidate/**`
  (exported as `gorca_mirror::fill::Candidate`, selected by the daemon at build time);
* `crates/gorca-connector-atspi/src/fetch/strategy.rs` and `fetch/strategy/**`;
* `crates/gorca-connector-atspi/src/events/subscription.rs`;
* new tests: `crates/gorca-mirror/tests/candidate_*.rs`,
  `crates/gorca-connector-atspi/tests/strategy_*.rs`;
* `docs/bakeoff/b1-mirror-fill/candidate-<x>/` (design note, `RESULTS.md`).

**Harness-owned:** `crates/gorca-mirror/src/{lib,store,driver,limits}.rs` and `fill/mod.rs`,
`gorca-model`, `gorca-connector-api`, `gorca-core/**`, `gorca-dbus/**`, the rest of
`gorca-connector-atspi` (including `fetch/items.rs`, `events/router.rs`, `events/paths.rs`),
`crates/gorca-bench-b1/**`, `crates/gorca-testkit/**`, `tests/harness/**`,
`docs/bakeoff/b1-mirror-fill/{SPEC.md,harness/**}`.

**Rules.**
* No blocking, no await in the policy; no I/O outside `strategy.rs`; every call with the op's deadline.
* No read-side lock or message on the Reader's path (MS-1).
* No per-app or per-toolkit special case keyed on app name; `AppClass` may tune budgets only.
* No gaming: no detection of the harness (bus address, `/gorca-h.*` paths, `node-<N>` names,
  timing signatures), no special casing of the fake apps.
* `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
  `./tools/check-deps.sh`, `cargo xtask deps` (no transport crate under `gorca-mirror`) stay green.
* Dependencies: crates already in `Cargo.lock`; anything new needs the orchestrator; at most 1.

| Budget | Limit |
|---|---|
| Wall time per arm | 6 h from branch to `RESULTS.md`; one implementation pass + at most two fix passes after the first harness run |
| Code size | ≤ 1 200 added non-test lines in the arm-owned paths (`git diff --numstat`, `#[cfg(test)]` excluded) |
| Deliverables | the branch; `RESULTS.md` with raw `run-all.sh <label>` output; result files in `bench/results/b1-mirror-fill/<label>/`; a 1-page design note (what is fetched when, what bounds what, which P-9 answers it relies on) |
| Runs | `run-all.sh` without `soak` ~2 h for two arms interleaved; `soak` separately |

## 11. Harness work (before any arm starts)

| # | Work | Owner | Output |
|---|---|---|---|
| H-1 | **Census** of the live desktop, read-only: per app (gnome-shell, gnome-control-center on 6 panels, nautilus on 3 directories): GetItems count/time/bytes, full walk node count (bounded), depth and fan-out histograms, role histogram, name-length histogram, `GetApplicationBusAddress` mode, per-call service time p50/p99; plus a 5-min event trace of a manual walk (types, rates, burst shapes, node hashes only; no names or text stored) | linux-platform-engineer (python) | `tools/platform-probes/census.py`; `tests/harness/b1/models/{shell,settings,files}.json`; report `docs/reports/<date>-b1-census.md` |
| H-2 | **Fake app `b1-fakeapp`** (Rust, one binary, many models): loads a model JSON; implements Accessible, Application (P2P `Peer`/`SameBus`/`None`), Cache (GetItems, Add/Remove signals per profile), Component (extents), Value, Text (minimal), Properties (Get/GetAll); **serial service-time model** (D-1) with GetItems per-item cost; quirk flags G-1…G-6, A-1…A-3, S-1, S-2; truth log; control socket (focus, mutate evented/silent, replace subtree, scroll/recycle, hide, kill/restart, hang) | test-engineer | `tests/harness/b1/fakeapp/` (crate in the workspace, `publish = false`) |
| H-3 | **Walk driver** and W-1 scripts (seeded), app spawning, step counters, invalidity checks | test-engineer | `crates/gorca-bench-b1/src/walk.rs`, `tests/harness/b1/walks/w1.toml` |
| H-4 | Presentation-log reader, verdict classifier (6.2), correlation with the mock speech server | perf-engineer | `crates/gorca-bench-b1/src/verdict.rs` |
| H-5 | Message accounting: fake-app counters, drain stats, bus monitor cross-check (verify whether the Debian dbus-daemon exposes `org.freedesktop.DBus.Debug.Stats`; else count with `BecomeMonitor`) | perf-engineer | `crates/gorca-bench-b1/src/msgs.rs` |
| H-6 | Runner: interleaved repeats across arms (`--arm <label>:<path-to-gorca-daemon>`), median gating, `limits.rs`, report rows in the B-2/e2e format with host meta (load average start/end, governor, `cpus_allowed`, git rev per arm), `compare.py`, `run-all.sh` | perf-engineer | `crates/gorca-bench-b1`, `docs/bakeoff/b1-mirror-fill/harness/` |
| H-7 | Policy-step microbench at the caps (MA-6) linking an arm's `Candidate` directly | perf-engineer | `crates/gorca-bench-b1/benches/step.rs` |
| H-8 | Conformance suite IC-1…IC-23, audit tests MA-1…MA-10, bounds tests MB-B1…MB-B10 | test-engineer | `crates/gorca-testkit/src/b1/`, `crates/gorca-mirror/tests/` |
| H-9 | R0 run end to end; baseline numbers in this SPEC (section 12) | perf-engineer | `bench/results/b1-mirror-fill/r0/` |
| H-10 | **Harness self-tests** (the harness must measure the arm, not itself): (a) service-time fidelity: fake replies measured at gorca's socket match the model within ±20 % at p50 and +1 ms at p99, else the run is invalid; (b) a deliberately broken policy that never invalidates (`StaleProbe`) must fail B1-STALE; one that never caches must fail B1-HIT.revisit; one that dumps every 100 ms must fail B1-MSG.idle; (c) the verdict classifier on hand-made logs; (d) msgs cross-check within 1 %; (e) P-9 profile update re-runs (a)-(c) | test-engineer + perf-engineer | `crates/gorca-bench-b1/tests/selftest.rs` |
| H-11 | Live shadow runner and checklist (8.4); `dbus-monitor` allowlist audit | linux-platform-engineer + tech-writer | `docs/bakeoff/b1-mirror-fill/harness/{live.sh,live-checklist.md}` |
| H-12 | Quiet-host recipe for authoritative runs (load < 1, `performance` governor, pinned cores) recorded in meta; runs on a loaded host are informative | perf-engineer | harness README |

Start order: H-1 and PW-1…PW-12 in parallel; H-2 as soon as the model schema is fixed (it may
start with the provisional table and switch to census data); H-3…H-8 after H-2; H-9 and H-10 last.
The arms start when H-9 and H-10 pass on `main`.

## 12. Baseline

To be filled by H-9 (R0 numbers, host, load). Phase 1 reference for the cold path: L1 p99 2.0 ms,
L3-50k 3.1 ms, lost 0 (`docs/decisions.md`, 2026-10-13).

## 13. Arm briefs

### Arm A: lazy on demand + invalidation

* **Fill.** Demand only: focus miss (shared Reader path); `WhereAmI`/`NavStep` demand →
  `Ancestry` or `ChildSnapshots` Interactive. After invalidation of the **focused** node: refetch
  it at once (Interactive) so S1/RV-1 find it fresh. Nothing else.
* **Strategy.** Pipelined property gets for `Fetch` (name, role, states, description, value,
  `labelled-by` concurrently on the app's connection), `Properties.GetAll` where it saves round trips.
* **Should win:** B1-MSG.*, B1-RSS.big, B1-FF.files-big (no dump in the way). **Risk:** B1-HIT.all.

### Arm B: dropped (P-9)

Pure eager `GetItems` per app is not run. P-9 shows the dump is complete only for gnome-shell;
on GTK4 it is empty until something walks the tree and is never pruned (`RemoveAccessible` never
sent), GTK3 misses 63 % of gedit, Firefox and Qt return nothing. An eager-everywhere arm would be
lazy plus a useless dump on every app except Shell, and Shell's case is covered by arm C.

### Arm C: hybrid (lazy + verified-complete eager + focus-window prefetch)

* **Fill, everywhere.** Lazy as arm A, plus on `FocusSettled`: `Ancestry` to the window (Prefetch)
  and `ChildSnapshots` of the focus's parent (± 32 siblings, Prefetch). On `WindowActivated`: the
  window's first two levels. While the focused app is idle for > 500 ms: Background `Subtree` of
  the focus window up to 2 048 nodes, rate-limited.
* **Fill, apps with `cx.dump_complete` (today: verified gnome-shell only).** One `CacheDump`
  (Background) after the app's first focus has been presented, never at `Appeared`; again only
  after a dirty-all or `Flooded` resync older than 30 s, and only when Shell has had no focus
  change for 500 ms (the dump runs on Shell's main loop: ~17 ms of compositor-thread work). Kept
  current by events (`ChildrenChanged` → refetch the parent's child list lazily), not by cache
  signals alone. The arm may subscribe to cache signals; their msgs/s are counted.
* **Must handle:** a dump that races a mutation (IC-13, LR-4: absence from a dump removes
  nothing); the eligibility flag cleared mid-run (IC-22); prefetch of nodes that die immediately
  (switcher, menus, IC-14).
* **Should win:** B1-HIT.all and B1-HIT.ancestry, especially on `shell`; B1-FF on revisited apps.
* **Risks:** B1-L1c/B1-FF on `shell` if a dump is in flight when the user moves (head-of-line on
  Shell's serial loop); B1-MSG.startup; B1-RSS.big; misses on jumps; policy state size. A Shell
  frame drop during the dump is not measurable in the harness and goes to the reviewer and the
  optional live run (8.4).

### Not allowed

* A periodic TTL refetch of the whole mirror (Orca's 60 s clear) as the way to stay correct: it
  fails B1-MSG.idle and hides the question RV-1 answers. The shared on-read TTL of 2.2 is not a
  refetch and is not arm-tunable.
* Choosing eager behaviour by app name, `ToolkitName`, process name or anything else the app
  reports about itself; only `cx.dump_complete` (2.2) enables `CacheDump`.
* Presenting from the mirror without RV-1, changing the invalidation table, or turning a metric
  green by shrinking what it measures.

## 14. How the arms are judged

1. Each arm works in its own worktree from the commit that contains this SPEC, PW-1…PW-12 and
   H-1…H-12, and delivers `RESULTS.md` (raw `run-all.sh <label>` output).
2. The reviewer re-runs everything itself, all arms **interleaved in one session** on a quiet host
   (H-12), with `compare.py`, without trusting `RESULTS.md`; the `soak` run once per arm.
3. Gate first (section 9), then ranking, then risks the harness cannot see: ordering between P2P
   replies and bus events, eviction fairness, LR-3 revival safety, Newton-compatibility of the
   policy (no AT-SPI assumption leaked into `gorca-mirror`), extensibility to M4 review/object
   navigation and to `ext-*` hints.
4. Weights as in B-2: metrics 50, correctness 30, simplicity 10, extensibility 10. On a tie within
   noise (overlapping min..max ranges on every ranking metric), the simpler arm wins (expected: A).
5. Two arms only (A, C; B dropped after P-9, 2.2). C's two mechanisms are judged separately as
   well as together: the reviewer reports B1-HIT.all and B1-FF per model, so the Shell dump's
   gain (`shell`) and the focus-window prefetch's gain (`settings`, `files`, `gtk3`) are visible on
   their own. If C wins only on `shell`, the outcome is "A plus the Shell dump", which must be
   re-measured with this harness before adoption; likewise for any other port between arms.
6. The shared rules decided after P-9 (focus revalidation on every focus, on-read TTL, verified
   eligibility, the ancestor NAME invalidation of P9-3) are not judged: a defect in them is a
   harness bug, fixed on `main` and re-run for both arms.

## 15. Amendments this bake-off needs (for sign-off before PW work starts)

* ADR-0001 §4.4: `Fetching` leaves the published snapshot; `Stale` carries a mask; `have`,
  `confirmed_seq` (PW-2). Semantics unchanged.
* ADR-0001 §4.1 I-1: "removal is not final for a path-unique toolkit: a successful reply to a
  request issued after the tombstone revives the key" (LR-3, 5.5).
* ADR-0001 §8.1: every cache-served presentation is tagged `may_refine`; RV-1 (5.3).
* ADR-0004 mapping table: `Cache.AddAccessible`/`RemoveAccessible` rows (PW-8), opt-in match rule.
* `gorca-connector-api`: `ChildSnapshots`, `Ancestry`, `CacheDump`, `BackendError::Unresolved`.

Applied to ADR-0001 and ADR-0004 on 2026-10-13. Added after P-9 (same date, needs a changelog line
in ADR-0001 §4.4/§5.1 when PW-1/PW-4 land): `AppInfo.dump_complete` and the on-read TTL (2.2).

## 16. Open items

* Section 8.1 node counts, depths and event patterns are provisional until H-1; `GetItems` and P2P
  columns are P-9 live values. G-6, S-1, P-9 H2 (source only), H3 and H4 (not exercised) remain
  hypotheses; their profile flags stay pessimistic.
* Chromium statements in P-9 are source-only (not installed on this host).
* The isolated harness runs dbus-daemon, not dbus-broker (`tests/harness/README.md`); broker
  forwarding cost is covered only by the live run.
* Whether Debian's dbus-daemon has the Stats interface (H-5) is unverified.
* Real GTK4 GetItems cost for 10 000-entry directories is unmeasured (P-9 H5); `files-big` uses
  P-9's 45 µs/item linearly. Irrelevant to the dump (GTK4 is never eligible) but it calibrates the
  per-item cost of `Subtree`/`ChildSnapshots` there.
* The Shell identity check (2.2) assumes gnome-shell owns `org.gnome.Shell` on the session bus and
  is the same process as its a11y-bus connection; to be confirmed live by PW-6/linux-platform-engineer.
* Python is not used for the B-1 fake app because its per-call overhead is of the same order as
  the modelled service time; H-10(a) verifies the Rust fake instead.
