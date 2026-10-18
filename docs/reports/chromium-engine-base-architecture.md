# Chromium-engine base: layering, mirror freshness, mode policy (draft ADR-0023)

Date 2026-10-19, architect. Input: `docs/reports/chromium-electron-a11y-research.md` (web-a11y-engineer,
same day), ADR-0003, ADR-0008, ADR-0019, ADR-0021, ADR-0022 r2, code on `main` 176e5773.
Parallel input pending: `docs/reports/chromium-mirror-freshness-math.md` (math-researcher). Decision
points for the reviewer are marked **DP-n**.

Part I is the analysis (what the code does today and why Signal fails). Part II is the draft ADR.

---

## Part I. Analysis

### I.1 The failure chain in the code (verified by reading, not reproduced)

1. Signal's document is created at 4 nodes (`phase=create`, live journal). `DocRegistry::ensure`
   (`crates/gorca-core/src/doc_registry.rs:287`) only creates a mirror for a root it has not seen.
2. Chromium announces growth with one `children-changed::add` on the **parent of the new subtree
   root**, carrying index and child (`refs/chromium/ui/accessibility/platform/ax_platform_node_auralinux.cc:4008-4024`);
   removal likewise (`:4026-4041`). Ignored-flag flips, reparenting and reorders emit no
   children-changed at all (research §1, §2: 35→16 nodes with zero events).
3. The app actor routes an event on an unknown node to the app's document
   (`crates/gorca-core/src/app_actor/doc_route.rs`, `send`, JN-3), but the source maps it with
   `to_atspi` (`crates/gorca-doc-source-atspi/src/source.rs`, `key_of(node)?`) and **silently drops
   any event whose node is not in `live.locals`**. When the growth lands under a node the mirror
   never saw (an un-ignored wrapper, a node created during the silent 35→16 phase), the one event
   that announces 900 nodes vanishes. No counter records it.
4. `accessible-parent` property changes (Signal emitted 4 in 10 s) are dropped by
   `events.rs::from_object_signal` (`_ => return None`). They are the only reparent signal Blink gives.
5. Every later focus misses the join (`web_on_focus`, `crates/gorca-core/src/browse/commands.rs:984`)
   → `WebMode::Loading` (grabs off) → `ensure` → `locate` walks to the top `DOCUMENT_WEB`
   (`factory.rs:114 top_document`) → `by_root` hits → `DocNotice::InDocument` →
   `web_retry_loading` finds no join → nothing. After `LOADING_GRACE` (1 s) a known non-field focus
   goes to browse **at the root of the 4-node mirror** (`commands.rs:2030-2052`); an unknown focus
   stays in `Loading` with grabs off forever. Either way the arrows do nothing useful. Nothing on
   this path ever asks the source to refresh: the miss is a dead end.
6. The mechanism that *would* repair a too-early build exists only for Firefox warm-up
   (`suspect` docs rebuilt on `on_warmed`, `doc_registry.rs:369-383`). `DocRequest::DriftCheck`
   is a no-op (`source.rs:1326`, M7b not done).

Conclusion: the defect is not Electron-specific. Any Blink page whose content attaches under a
node the mirror missed, or whose first build happens during a loading shell, is stuck until a
navigation. Chrome passed the rig because the rig builds after load (research §4).

### I.2 Prior art

| Reader | How it stays fresh / decides membership | How it picks the mode |
|---|---|---|
| NVDA | Membership is asked of the **browser**, not the buffer: `ChromeVBuf.__contains__` calls `accChild(IA2UniqueID)` on the document root (`refs/nvda/source/NVDAObjects/IAccessible/chromium.py:82-96`). The buffer backend invalidates and re-renders a subtree on every `EVENT_OBJECT_REORDER` (`refs/nvda/nvdaHelper/vbufBackends/gecko_ia2/gecko_ia2.cpp:1412-1487`); on Windows Chromium fires reorder for every child-list change, so the Linux sparsity does not arise there | Generic: buffer unless EDITABLE/BUSY (`chromium.py:99-119`); auto pass-through by focused role (`browseMode.py`); no Signal/Slack module; VS Code disables the buffer (`appModules/code.py:25`) |
| Orca | Re-queries live objects (no mirror), so staleness shows as cost, not as wrong structure | Electron = toolkit "chromium" and app name not a known browser (`refs/orca/src/orca/document_presenter.py:175-190`) → **auto sticky focus mode**, setting `auto-sticky-focus-mode` default true (`:850-861`); also for top-level `http` web apps (`:192-208`); per-app session flag "user overrode" (`:223-242`) |
| AccessKit / Newton | Push model: full tree updates with explicit node lists, no sparsity | n/a |

Lessons: (a) the browser is the authority on membership; a mirror miss must become a question to
the browser, never a final answer; (b) Linux Blink needs a pull-side complement to its events;
(c) "Electron ⇒ focus mode" (Orca) and "Electron ⇒ normal browse" (NVDA) are both defensible
defaults; neither is keyed in the engine.

### I.3 Budget facts

- Focus speech does not depend on the join: the focus is presented from the directory snapshot, the
  join only decides mode and cursor. So freshness repair is **off the keypress→speech path**; it
  bounds "time until browse keys work", not p99 speech latency.
- Locate already costs 2-9 ms (live, 2 calls per hop). Chromium fetch rate from the M9 baseline:
  ~0.1-0.3 ms per node, so a 900-node rebuild is ~100-250 ms; a 20k-node Slack/VS Code document is
  seconds (chunked, ids kept, ADR-0003 IL-R5).

---

## Part II. Draft ADR-0023 — Chromium-engine base: mirror freshness, layering and web-app mode policy

**Status:** Proposed (2026-10-19). **Supersedes:** nothing. **Amends:** ADR-0003 (adds §FR repair
states), ADR-0008 (Blink profile caps), ADR-0022 §2.2 (writer handoff on adoption), ADR-0019 (0.9
hook list items, no 0.3 change).

### 1. Context

Part I. Owner goal: one solid base for every Chromium-engine product (Chrome, Chromium, Edge, Brave,
Vivaldi, Opera; Electron: Signal, Slack, VS Code, Discord, Element, WhatsApp-desktop wrappers), with
per-app behaviour in add-ons. Telegram Desktop is **Qt**, not Electron: it goes through the Qt/AT-SPI
path and is out of scope here. Constraints: core never imports connector types; no core app-name
special cases (ADR-0008 §3.1 rule); M22a before M18a (ADR-0022 §2.10); WIT `gorca:ext@0.3.0`
unchanged, new surface in 0.9 only.

### 2. Decision

#### 2.1 Layering

| Layer | Owns | Chromium-engine items |
|---|---|---|
| **L1 connector / doc source (W1, data + `Code` rows)** | how AT-SPI Blink behaves | `EngineProfile::Blink` caps: `silent_reorders` (exists), **`sparse_subtree_events`** (new: children-changed only for subtree roots, ignored/reparent silent), **`young_doc: Option<YoungDocProbe>`** (new), `needs_wake_call` (exists), text-extents frame, `text_leaves_repeat_parent` (exist), `SetTextSelections` as `ActionCaps::SET_SELECTION` (ADR-0021, Chromium only); map `accessible-parent` to a `ParentChanged` hint; orphan accounting (§2.2 FR-3); `locate` returns the ancestor chain, not only the top document; `role=application` with a parent is `ATK_ROLE_EMBEDDED` on Linux (`ax_platform_node_auralinux.cc:2673-2680`) → S-1 quirk row to recover `Application` from `xml-roles` (verify first) |
| **L2 core web engine (W0)** | generic, engine-agnostic behaviour | the freshness state machine (§2.2), join-miss repair, single-flight and rate limits, `WebMode` and the mode-policy *mechanism* (§2.3), structural "web-app" signals (role mix), never app names |
| **L2′ semantic model (ADR-0022, M22a)** | node facts | Blink quirk rows in the S-1 table; `Unknown` vs `Absent` for nodes adopted by repair (props not yet fetched); **writer handoff on adoption** (§2.4); `PI` trust row for `chromium`; nothing about modes (mode is reader state, ADR-0019 §2.7) |
| **L3 product/app add-ons (W2/W3)** | per-product and per-app policy | `ext-chromium` (browser chrome: omnibox popup, tab strip; Electron detection as data: toolkit `Chromium` ∧ app name ∉ browser list), per-app presets (Signal, Slack, Discord, VS Code, Element) as **data-only** add-ons: mode defaults, `buffer = off` (VS Code, like NVDA), lazy-region selectors, pass-through chords, nav units for app roles (message, conversation) via patches/categories |

Rule kept from ADR-0008: a workaround two app add-ons need moves down (engine artefact → L1,
behaviour → L2). Event sparsity and stale mirrors are engine artefacts + core behaviour: **no
add-on may be required for an Electron app to have a fresh mirror.**

#### 2.2 Mirror freshness for event-sparse engines

Per document, a freshness machine in the doc actor (generic; triggers gated by `SourceCaps`):

```rust
pub enum Freshness {
    /// Events are trusted; Revalidate-on-read (AM-7) covers silent reorders.
    Tracking,
    /// Built during a loading shell or while BUSY (young_doc probe armed).
    Young { built_at: Instant, nodes: u32, probe: u8 },
    /// Evidence of missed structure (join miss, orphan events, ParentChanged on unknown node).
    Suspect { since: Instant, evidence: Evidence },
    /// One repair in flight (single flight per document).
    Repairing { gen: RepairGen, kind: RepairKind, deadline: Deadline, waiters: SmallVec<[FocusTicket; 2]> },
    /// Dormant tab: marks only (ADR-0003 §6.6); wake = rebuild (AM-13).
    Dormant,
}
pub enum RepairKind {
    /// Subtree of the lowest mirrored ancestor on the focus chain, capped (max_nodes).
    Attach { at: SrcNodeId, chain: SmallVec<[SrcPath; 16]> },
    /// Whole-document resync, ids kept (existing DocRequest::Rebuild, IL-R5).
    Rebuild,
}
pub enum Evidence { JoinMiss(NodeKey), Orphans(u32), Reparent, YoungGrowth }
```

Transition table (every cell handled; anything not listed is "stay"):

| From \ input | focus join miss in this doc | orphan/ParentChanged on unknown node | probe tick (young) | repair done Ok | repair Err/Timeout | navigation/Reset |
|---|---|---|---|---|---|---|
| Tracking | → Repairing(Attach or Rebuild) | orphan count +1; ≥ `K_ORPHAN` (=1 for `sparse_subtree_events`) in a settle window → Suspect | — | — | — | Tracking (rebuild path exists) |
| Young | → Repairing | → Repairing(Rebuild) | grown or BUSY cleared → Repairing(Rebuild); else next backoff; after `T_YOUNG` (30 s) → Tracking | — | — | Young |
| Suspect | → Repairing | count | — | — | — | Tracking |
| Repairing | add ticket (latest focus wins, cap 2) | count (applied after) | — | → Tracking; re-try join for tickets of the **current** focus seq | backoff; ≤ 3 failures/60 s → Tracking + negative cache | cancel repair (gen bump) |
| Dormant | n/a (no focus) | mark | — | — | — | Dormant |

Rules:
- **FR-1 Join miss is a question, not an answer** (NVDA lesson). A focus that misses the join in an
  app with a mirrored document asks `locate` for the **ancestor chain** (same calls it makes today).
  If the chain ends at a mirrored document root: pick the lowest mirrored ancestor `A`;
  `RepairKind::Attach { at: A }` if `A` is not the root and its last known subtree size ≤ `ATTACH_MAX`
  (2,000 nodes), else `Rebuild`. If the chain ends at an *unmirrored* document → create (today's path).
- **FR-2 Single flight, rate limit, generation.** One repair per document in flight; ≤ 4 starts/s per
  document, ≤ 8/s per app; each repair has a `RepairGen`; results of a superseded gen are applied as
  mirror deltas (they are still true) but never trigger mode or cursor changes. A `FocusTicket`
  carries the focus sequence; on completion the reader re-joins only if the ticket's focus is still
  current (epoch check, ADR-0001 R3).
- **FR-3 Orphans are counted, never silently dropped.** `to_atspi` returning `None` for an unknown
  node becomes `Orphan { path }`; with `sparse_subtree_events` the first orphan in a settle window
  moves the doc to `Suspect`, and the next settle issues `Revalidate` of the orphan's nearest
  mirrored ancestor if `locate`-style parent walk finds one within 8 hops (bounded, background
  budget), else `Rebuild`. Without the cap (Gecko) orphans are counted only (diagnostic).
- **FR-4 Young documents.** A Blink document built with `nodes < YOUNG_NODES` (32) or `BUSY` is
  `Young`: probes at 0.5, 1, 2, 4, 8, 16 s (one `GetChildCount` on root and on the deepest spine node,
  background budget), plus immediate repair on BUSY→false, `load-complete`, or any orphan. Ends at
  30 s or first stable probe with ≥ `YOUNG_NODES`.
- **FR-5 Deadlines.** Chain walk: interactive (150 ms, ADR-0019 I-2 class); Attach: 500 ms;
  Rebuild: background 2 s per chunk, chunked under the existing build budget, publish per chunk.
  Nothing waits on a repair: the reader stays in `Loading{known}` with grabs off (H2), and focus
  speech already went out.
- **FR-6 Negative cache.** A focus node whose chain does not reach a mirrored root (Views UI,
  devtools, `ignored` nodes reached only via focus) is cached `Unreachable` per `NodeKey` for 30 s;
  repeated misses on it cost zero IPC.
- **FR-7 Loading resolution.** When a repair adopts the focused node, the reader resolves `Loading`
  through `web_retry_loading` at the **focused node**, not the root (today's 1 s fallback to root
  stays only for `RepairKind` failures).
- **FR-8 Storm behaviour.** Freshness inputs are focus events (≤ hundreds/s even in storms) and
  per-settle orphan counts (O(1) per event, no IPC per event). Repairs are bounded by FR-2, so a
  10k ev/s storm costs at most 4 repairs/s/doc and Rebuild is already the overflow answer (EF-2).
  Under ingress overflow the doc goes `Suspect` once, not once per dropped event.
- **FR-9 Firefox composition.** The machine is generic; triggers are per profile. Gecko:
  `sparse_subtree_events = false`, `young_doc = None` (it has cache warm-up), and FR-1 runs only if
  the miss persists past the source's settle + 300 ms (Gecko misses normally resolve by events).
  Gate: zero repairs counted on the Firefox M7/M8 suites (§5 M-F5).

**DP-1** Attach (subtree of lowest known ancestor, capped) vs always Rebuild. Attach is cheaper for
big docs and keeps work local; Rebuild is one code path that already exists. Recommendation: ship
Rebuild-only in the minimal fix (Signal is 900 nodes), add Attach in the M7b/W3 slot when a > 5k-node
Electron document misses the M-F2 budget. Math report should bound expected misses vs cost.

**DP-2** Young-doc probe on by default for Blink? It is the only defence when the growth event is
itself orphaned and no focus follows (screen reader started on an idle Signal window). Recommendation:
on, with the bounded schedule above.

#### 2.3 Mode policy (option B)

Mechanism in core (L2), policy as data (L3 + user):

```rust
pub struct DocModePolicy {
    pub buffer: BufferPolicy,        // On | Off (VS Code: focus-only, no browse keys at all)
    pub initial: InitialMode,        // Browse | Focus | FromFocus (today: browse unless the focus passes through)
    pub follow: FocusFollow,         // Auto (NVDA pass-through) | StickyBrowse | StickyFocus (Orca for Electron)
    pub escape_returns: bool,        // Escape from auto focus mode back to browse
}
```

Resolution, highest first, evaluated once per (app, document root URI) at activation and on
navigation: **user override** (per app or per site, M14a profiles) > **add-on preset** (data-only
manifest, M18a; native first-party table before that) > **ext-chromium product default** (Electron
detection as data) > **core default** (`Browse`, `Auto`, `escape_returns`, = NVDA). Structural
signals (no headings/landmarks, ≥ 70 % widgets, grid/tree roots) may only pick between the core
defaults' `FromFocus` branches; they never flip an app wholesale.

- **MP-1** The toggle (gorca+Space) always works and is **sticky per document for the session**
  (NVDA). The last explicit toggle per app is remembered in-session (Orca `user_has_overridden`).
  Persisting it across restarts requires an explicit "save mode as default for this app" command
  (ADR-0005 explicit save), writing the user layer.
- **MP-2** `role=application` keeps `FocusCause::Application` (only the toggle leaves).
- **MP-3** The mode is reader state (ADR-0019 §2.7): add-ons declare policy data or *request* a mode
  change; they never set `WebMode`.

**DP-3 (owner)** Default for Electron apps: (a) NVDA-like browse + auto pass-through (recommended:
the owner wants arrows to read Signal; Signal's focus lands on `button` which does not pass through,
so browse holds), (b) Orca-like sticky focus, (c) per-app presets only, generic Electron = (a).
Recommendation: (a) as core/product default, presets ship for VS Code (`buffer = Off`) only until
users report others.

#### 2.4 Semantic model composition (M22a)

- **SM-8 writer handoff.** A focus-fetched node outside every mirror is written by the app actor.
  When a repair adopts it, writership moves to the doc actor at the adoption publish; the app actor's
  registers for that node are forwarded once as `atspi` facts with their sequence (no fact lost,
  no double writer). Invariant **FR-I1**: at any instant a `NodeKey` has exactly one writer; the
  handoff is a single message carrying the registers and the adoption `RepairGen`.
- Adopted nodes start with `known` = what the repair fetched; groups not fetched are `Unknown` and
  presentation uses `may_refine` (ADR-0022 §2.3), never `Absent`.
- Freshness/generation are **structure metadata of the mirror**, not `Semantics`; provenance
  (`ProvTag`) records `repair:<gen>` as the fetch path, so "why was this said" can show a stale read.

#### 2.5 Add-on surface (WIT 0.9 in M18a; `gorca:ext@0.3.0` unchanged)

Data-only manifest (PA-1), no component needed for the common case:
1. `[[web.mode]] selector = { app = "signal-desktop" | uri-prefix = "file:///opt/Signal/" }` →
   `DocModePolicy` fields (§2.3).
2. `[[web.lazy-region]] selector = "<css-like over Semantics, ADR-0019 selectors>"` → the core treats
   the region as silently changing: Revalidate-on-read (AM-7) and FR-1 attach points scoped there.
3. `[[keys.pass-through]] mode = "browse" chords = [...]` → chords the app owns that browse mode must
   not grab (Signal/Slack single-key shortcuts if any; reserved chords I-7 excluded).
4. Patches/categories for app roles (message, conversation) → quick-nav units (`ExtCats`, ADR-0022 §2.9).

Component hooks (0.9, `app-module`/`site` worlds, additive under V-2/V-3):
5. `doc-policy: func(facts: doc-facts) -> option<doc-mode-policy>` — inline, 2 ms, for policies a
   selector cannot express (URI-dependent). `doc-facts` read through accessors (V-4).
6. `request-mode: func(m: mode-request)` as an **effect** (ADR-0019 effects buffer), honoured only
   inside a command the add-on owns or on `app-focused`.
7. Read-only `freshness: func(doc) -> freshness-info` accessor (state, last repair, orphan count) for
   diagnostics add-ons. No add-on can trigger a rebuild directly (it would bypass FR-2); it may
   `request(revalidate(node))`, which the core rate-limits like FR-2.

Nothing above requires a 0.3.x change; until M18a the presets live in the native first-party
`ext-chromium` data table (ADR-0008 W2 "data files") and migrate verbatim to the manifest.

### 3. Alternatives considered

| Alternative | Why not (now) |
|---|---|
| Periodic full re-sync of Blink documents (timer) | Wastes IPC on idle docs, still misses between ticks; violates "dormant = 0 IPC". Kept only as the bounded young-doc probe |
| Validate every focus (fetch chain + props on every focus, hit or miss) | Adds IPC to every focus in every browser for a miss case; a hit is already proof of membership |
| Sticky focus mode for all Electron apps (Orca) | Hides the defect instead of fixing it; contradicts the owner's goal (browse in Signal) |
| Per-app add-ons for freshness (`ext-signal` rebuilds) | Engine artefact in L3 violates ADR-0008 rule; every Electron app would need one |
| CDP source (`SourceKind::Cdp`, Chrome DevTools accessibility domain) | Full, push-mode tree without sparsity, but needs a debugging port per app (security, Electron disables it); keep as TBD-S1 research, not the base |
| Upstream Chromium fix (emit children-changed for un-ignore/reparent) | Right long-term (track U), but every shipped Electron lags years |

### 4. Consequences

- Good: Signal-class failures become self-healing within one repair; orphans become visible in
  counters; Firefox path unchanged by gating; the mode policy is one mechanism serving NVDA- and
  Orca-style defaults and per-app presets without core special cases.
- Cost: a new per-document state machine and two `DocRequest` uses (`Rebuild` exists; `Attach`
  is `Subtree` with a chain, possibly no new variant). Locate returns a chain (API change inside the
  doc-source API crate, not WIT).
- Risk: a page that mutates structure faster than a rebuild (chat with streaming messages) could keep
  a doc in `Suspect`; FR-2 caps cost, M-F4 measures it.

### 5. Metrics to validate

| Id | Metric | Target |
|---|---|---|
| M-F1 | Signal-like page (§6 T-1): time from first focus after growth to `web_focus hit=true` | p95 ≤ 500 ms (900 nodes), ≤ 2 s (20k) |
| M-F2 | Repairs per growth episode | ≤ 2 (one Rebuild + at most one Attach) |
| M-F3 | Keypress→speech p99 during repair and during x1 storm with focus flapping 50/s on unknown nodes | unchanged vs main (±1 ms; budget 15 ms) |
| M-F4 | Repair IPC under storm (10k ev/s, Blink) | ≤ 4 repairs/s/doc, no unbounded queue growth |
| M-F5 | Firefox M7/M8/M9 suites | 0 repairs, 0 regressions |
| M-F6 | Orphan count on the Chromium M7/M8/M9 suites | recorded baseline; every orphan followed by Suspect→Tracking within 1 s |
| M-F7 | Live Signal (owner, manual) | arrows/H/B/K work within 1 s of focusing the window |

### 6. Test plan

- **T-1 Signal-like page** `tests/web/lazy-shell/` (web-a11y-engineer + test-engineer), query
  parameters: `grow_ms`, `nodes` (900, 5k, 20k), `variant` =
  `single-add` (detached fragment appended once → one add event),
  `under-ignored` (growth under a wrapper that becomes un-ignored silently: `display:contents` →
  block, `aria-hidden` removed — the orphan case),
  `silent-shrink` (35→16 like Signal: `hidden` flips),
  `reparent` (move the app container; `accessible-parent` events),
  `virtual-list` (row reuse, existing B-6 corpora).
  Always keep a titled landmark during the shell phase so the rig's `find_doc` works (research §4 trap).
- **T-2 Rig order**: start gorca first, then the browser/app, and build the mirror during the shell
  phase (reproduces the live order). Wrappers: Chrome 155, Debian Chromium 154,
  `tools/electron-probe/electron-wrapper.sh` (Electron 44, `file://` page in an asar-like path).
- **T-3 Fake-source property tests** (`gorca-core` doc actor + fake `DocumentSource`): random tree
  evolutions with event loss probability p ∈ {0, 0.5, 1}; assert liveness (any focused node present
  in the source joins within ≤ 2 repairs), safety (no superseded-gen mode change; FR-I1 single
  writer), boundedness (repairs ≤ FR-2 rate), id stability (IL-R5).
- **T-4 Storm**: x1 storm suite with a Blink profile plus focus flapping; M-F3/M-F4.
- **T-5 Regression**: M7/M8/M9 Firefox + Chromium, 3 repeats, quiet host; M-F5/M-F6.
- **T-6 Live**: owner-approved reader restart with `RUST_LOG=gorca_doc_source_atspi=debug,gorca_core::doc_actor=debug`
  on Signal; Slack/Discord/VS Code if installed. Read-only for probes.

Gates for merge of the minimal fix: T-1 `single-add`, `under-ignored`, `silent-shrink` pass on all
three Chromium-engine targets; T-3 green; M-F3, M-F5 hold.

### 7. Phasing

| Slot | Content | Depends on |
|---|---|---|
| **Now — Phase 2 tail / C0 ("CHR-0")** | FR-1 (join miss → chain → `Rebuild`, i.e. DP-1 minimal), FR-2, FR-3 counters + Suspect→Rebuild, FR-4 young probe (Blink), FR-6, FR-7, `accessible-parent` → hint; counters in `gorca_phase`; T-1..T-5. **No mode-policy code, no WIT** (DP-3(a) is today's behaviour) | nothing; touches doc actor, registry, doc source, browse `Loading` resolution only |
| M7b / W3 slot | `Attach` repair, `DriftCheck` real implementation | CHR-0, M-F1 at 20k |
| **M22a** | Blink S-1 quirk rows (incl. EMBEDDED→application recovery if verified), FR-I1 writer handoff, `Unknown` for adopted nodes, `repair:<gen>` provenance | ADR-0022 |
| M14a | user mode override per app/site, "save mode as default" | profiles |
| **M18a** | §2.5 items 1-7 in WIT 0.9; native first-party presets migrate to manifests; VS Code `buffer = Off` preset | M22a |
| Track U | upstream Chromium: children-changed on un-ignore/reparent | — |

**DP-4** CHR-0 lands before or after the Phase 2 3 h soak/push? It is self-contained and gated by
T-5; recommendation: right after the in-flight Phase 2 branches merge, before M22a starts, so M22a's
writer-handoff rule is written against the real repair path.

**DP-5** Bake-off: not recommended. The contested sub-choice (DP-1 Attach vs Rebuild) is a parameter
study, decided by M-F1 at 5k/20k nodes on T-1, not by competing implementations.

### 8. Open questions / unverified

- Item 3 of I.1 (orphaned growth event) is inferred from code + live counts; T-1 `under-ignored`
  must reproduce it, or the live debug run (T-6) must show the event reaching the source.
- Whether the connector subscribes to `PropertyChange` for `accessible-parent` at all (only the
  doc-source mapping was checked).
- `xml-roles=application` on `EMBEDDED`: verify Chromium exposes it on Linux before writing the
  S-1 row.
- Slack/Discord/VS Code tree sizes and growth patterns are unmeasured (M-F1 20k is a guess).
