# Chromium-engine base design review (draft ADR-0023, freshness math, Signal research)

Date 2026-10-19, reviewer. Main at 69490de6. Inputs: `chromium-electron-a11y-research.md`,
`chromium-engine-base-architecture.md` (draft ADR-0023), `chromium-mirror-freshness-math.md`,
`tools/research/chromium_*.py`. Read-only review; no product code changed.

Verdict: **ACCEPT the direction, NARROW CHR-0, CORRECT the root cause.** The drafts explain the
Signal failure with a plausible but secondary mechanism (orphaned growth event). The source shows a
deterministic primary mechanism that neither report found, and it changes what CHR-0 must trigger on.

## 1. Root cause: re-derived from source

### 1.1 New finding RC-1 (high confidence, source-level): Chromium sends no `add` before `loaded`

`refs/chromium/ui/accessibility/platform/browser_accessibility_manager_auralinux.cc:254-263`:

```cpp
void BrowserAccessibilityManagerAuraLinux::FireSubtreeCreatedEvent(BrowserAccessibility* node) {
  // Sending events during a load would create a lot of spam, don't do that.
  if (!GetTreeData().loaded)
    return;
```

All `children-changed::add` events are suppressed while the frame is still loading. Removals are not
gated (`OnSubtreeWillBeDeleted`, `:537-546`). At the end of the load Chromium sends
`state-changed:busy=false` plus `load-complete` on the document (`FireLoadingEvent`, `:129-139`,
`:192-201`), and no structural event.

On the gorca side, three code facts then make the 4-node mirror permanent:

| # | Code | Effect |
|---|---|---|
| G-1 | `doc_registry.rs:287 ensure` creates the mirror on the first focus, even while the document is BUSY | Signal: built at 4 nodes, 2 s after app start (live journal) |
| G-2 | `app_actor/doc_route.rs:79-87` drops `load-complete` for `page_events_settle_ms` after `Appeared`; `gorca-connector-atspi/src/toolkit.rs:151` sets it to **3000 ms for Chromium** | Signal's load-complete probably fell inside the window (the probe saw the tree change at t≈2 s; timing not recorded) |
| G-3 | `doc_registry.rs:533-544` `HubMsg::Loaded` only notifies the reader (auto-read, `browse/sayall.rs:195`) and adopts *navigation* docs; it **never rebuilds an existing mirror** | even if load-complete gets through, nothing refetches |
| G-4 | Once loaded, Chromium's `add` fires on the platform parent of the new subtree root. That parent was created during the load, so it is unknown to the mirror, and `source.rs:454 to_atspi` (`key_of(node)?`) drops the event | the math report's absorbing state (Prop. 1), entered deterministically, with p_add = 0 during load rather than through random loss |

This explains every live number (nodes=4, 0/312 hits, locate always resolving to the existing doc,
no WARN) without assuming any event loss. It also explains why the rig passed. `delay-wrapper.sh`
re-adds the content via `setTimeout` *after* DOMContentLoaded. With no slow subresources, `load`
has already fired by then, so `loaded=true` and the `add` is sent.

### 1.2 Corrections to the drafts

- **C-1** The architect (I.1 item 2, L1 cap `sparse_subtree_events`) and the research (§1) say that
  ignored-flag flips are silent. They are not. `ax_event_generator.cc:400-409` emits `SUBTREE_CREATED`
  on un-ignore. `OnIgnoredWillChange` (`browser_accessibility_manager_auralinux.cc:514-535`) emits
  `remove` on ignore. Both are subject to `CanEmitChildrenChanged`, and the add also to `loaded`.
  Reorders really are silent.
- **C-2** The architect's I.1 item 4 says `accessible-parent` is dropped by `events.rs::from_object_signal`.
  That function only runs in the B-6 harness path (`bus::subscribe`, used by `gorca-bench-web/src/arm.rs:180`).
  In production, `gorca-dbus/src/drain/map.rs:337-346` maps every unknown `PropertyChange` detail,
  `accessible-parent` included, to `PropertiesChanged{ATTRS}` on the child. The doc source turns that
  into a `"props"` re-read: if the child is known, a reparent is treated as an attribute change; if not,
  the event is dropped by G-4. The fix is the same, but it goes in the connector mapping, not in `events.rs`.
- **C-3** `GetTree` is not a Chromium API and not a gorca helper. It is `org.a11y.atspi.Collection.GetTree`,
  implemented generically by **at-spi2-atk** (`refs/at-spi2-core/atk-adaptor/adaptors/collection-adaptor.c:1142`)
  over ATK. Chromium loads that bridge in-process (`ui/accessibility/platform/atk_util_auralinux_gtk.cc:17
  atk_bridge_adaptor_init`), so Chromium "supports" GetTree, GetMatches, GetMatchesFrom and GetMatchesTo
  only because ATK does. Consequences:
  - **F4 (atomic snapshot) holds by construction.** `impl_GetTree` is one synchronous recursion inside
    one D-Bus dispatch on the browser UI thread, and Chromium applies AX updates in UI-thread tasks.
    Upgrade F4 from "inference" to "proven from source".
  - **GetTree has no size or depth bound.** It returns the whole subtree (`append_accessible_properties`),
    stops only at `MANAGES_DESCENDANTS`, and caps children at `ATSPI_MAX_CHILDREN`. The math's
    "chunks ≤ 512 nodes" therefore cannot be requested. A chunk is bounded only by choosing roots whose
    *current* subtree is small, and in a stale region the current size is exactly what is unknown.
    `fetch.rs` `Split::Frontier` bounds the *number* of subtrees, not their size. A skewed page still
    issues one large call that blocks Chromium's UI thread for about 10 µs per node (5k nodes → ~50 ms
    jank).
  - **GetMatches** is the same generic `query_exec` walk (`collection-adaptor.c:1187-1222`). F6
    (3 µs/node) remains unmeasured.

### 1.3 The minimal experiment (no owner restart needed)

E-1 (decisive, in-loop): add a T-1 variant `preload-growth` to the architect's planned
`tests/web/lazy-shell/` page.
- The page holds `load` open with a slow subresource, e.g. `<img src="http://127.0.0.1:$PORT/slow?ms=8000">`
  from a 20-line Python server started by the rig and killed by PID.
- At DOMContentLoaded+300 ms it builds 900 nodes; it keeps a titled landmark so `find_doc` works.
- The rig starts gorca first and focuses inside the page before 8 s.
- Expected on main: the probe (`tools/electron-probe/probe_app.py`, with timestamps added for
  `load-complete` and `busy`) shows **0 `add` before load-complete**; the reader shows `web_focus hit=false`
  forever and `phase=create nodes<32`.
- Control A: the same page without the slow image passes (as `el-delay-*` did). Control B: run E-1 with
  `page_events_settle_ms=0`; it must still fail on main, which proves G-3 independently of G-2.
- Run on Chrome 155, Chromium 154 and the Electron 44 wrapper.

E-2 (cheap, read-only, already allowed): rerun `probe_app.py` against a private Signal profile, recording
monotonic timestamps of `busy`, `load-complete` and both `add` events relative to app appearance. If
load-complete lands < 3 s after the app appears, G-2 is confirmed for Signal specifically.

The live debug restart (T-6) is **not** needed to establish the cause. Keep it as the M-F7 acceptance
check after CHR-0.

The orphan mechanism (the drafts' ranked #1) stays real for post-load growth under a node created by a
later silent path (reorder, removed-then-re-added subtree). E-1 with `under-ignored` after load covers it.

## 2. Reconciled CHR-0 scope

Principles: repair is triggered by exact signals (Chromium tells us when `add` starts being reliable),
not by polling. Nothing runs on the key path. The browser UI thread is the scarce resource.

| Item | Architect | Math | CHR-0 decision |
|---|---|---|---|
| Rebuild on load-complete / busy→false of a mirrored doc | absent (FR-4 probes approximate it) | item 4 (partial) | **IN: primary trigger (RC-1)** |
| Ignore the settle window for docs that already have a mirror | absent | absent | **IN** |
| Focus join miss → locate chain → repair | FR-1 (Rebuild) | D2 → LKA diff | **IN, Rebuild only** |
| Orphan events → repair | FR-3 count → Suspect → Rebuild | D1 → LKA diff | **IN, counted + deferred one settle + Rebuild with cost backoff** |
| Young-doc probes 0.5..16 s, 30 s | FR-4 | — | **REDUCED: backstop only, 3 probes (2, 6, 16 s), only if built BUSY or < 32 nodes and no load-complete seen** |
| LKA GetTree diff (`Attach`) | M7b | core | **OUT → M7b** (needs a size-safe chunking that GetTree cannot give, see C-3) |
| Budgeted GetTree sweep 2000 nodes/s | rejected | S2 | **OUT → M7b `DriftCheck`**, active document only, opt-in per profile |
| M2 identity / relocation classes | — | §3 | **OUT → M22a/M7b** (§3) |
| Mode policy struct, presets | §2.3/§2.5 | — | **OUT → M14a/M18a**; one exception discussed in §4 |

Why not the sweep now: in the sim (`chromium_mirror_sim.py`, rerun here, numbers reproduce) it costs
17.7 ms/s of UI time at 790 nodes and 25 ms/s at 45k. That is a continuous ~2-2.5 % tax on the browser
UI thread for every mirrored Chromium document, and it breaks "dormant = 0 IPC". It also needs bounded
chunks, which GetTree cannot guarantee (C-3). It buys detection of *silent* creates (p95 1.3 s vs 2.7 s
with S1). Those affect reading accuracy, not whether browse mode engages, and Revalidate-on-read
(AM-7) already covers the cursor's neighbourhood.

Why Rebuild, not the LKA diff: Rebuild exists (`DocRequest::Rebuild`, ids kept via IL-R5) and has been
tested for the overflow and wake paths. Signal is about 900 nodes, so ~100-250 ms of fetch and ~9 ms of
UI time. The diff's 6x saving matters only above about 5k nodes, and the diff needs new chunking code.

### Blocking issues CHR-0 must address (found in current code)

- **B-1 head-of-line blocking (medium-high).** `source.rs:648-652` awaits `handle_request` inline.
  `DocRequest::Rebuild` (`:1326` → `rebuild` → `full_resync`) therefore blocks `Action`, `Text`,
  `Revalidate` and `Validate` for the whole rebuild. The routed event channel is not drained in that
  time either, so the bounded queue fills, EF-2 drops events and overflow triggers another rebuild.
  This already exists for overflow and wake. CHR-0 turns rebuilds into a routine Chromium event, and
  20k-node Electron docs (Slack, VS Code) take seconds. Failure scenario: a user presses Enter on a
  Signal message while the load-complete repair runs; the activation waits behind the rebuild past
  `ACTION_DEADLINE` (500 ms) and fails silently or late.
  Fix: `full_resync` polls the request channel between frontier chunks and serves interactive requests
  first (against the old mirror). Alternatively, run the repair fetch as a separate task that returns
  a block, applied by the loop as a message (ADR-0001 R3).
- **B-2 grace fallback strands the cursor (medium).** After `LOADING_GRACE` (1 s,
  `browse/commands.rs:2030-2052`), a known non-field focus enters Browse **at the root** of the stale
  mirror. A later repair calls `web_changed` in Browse mode (`:1825-1835`), which only follows the
  cursor and never re-joins the focus. Even a perfect repair leaves Signal at the top of the document
  until the user moves focus. Fix: when a repair started with a `FocusTicket` completes, re-run the
  focus join if the focus seq is still current and the cursor is still the grace-fallback root that the
  user has not moved.
- **B-3 UI-thread slice size (medium).** See C-3. Frontier split by count does not bound the largest
  GetTree. Gate: record `heaviest_get_tree_ms` (already in `source.rs:727`) on T-1 at 5k/20k. If it
  exceeds 30 ms, deepen the frontier on subtrees whose `ChildCount` is large, or fall back to
  `GetChildren` walks there.

## 3. Cursor identity (M2) — defer, with one guard

Do not feed Exact/Confident/Flagged/Lost into the displaced rule in CHR-0:
- The displaced-cursor rule is **not on main** (`55c33be6` lives only on `worktree-agent-a6afeefd1474b60fc`,
  r2 MERGE-WITH-FIXES with R2-1..R2-3 open). CHR-0 would build on an unmerged, still-moving rule.
- M2 needs whole-tree Merkle hashing in `identity.rs`, plus the item-hash demotion of IL-1. That is an
  identity-layer change on the Firefox path too, and it needs its own A/B (the math report's M1 is a
  Python approximation; its 78-94 % wrong-confident figures are simulated, not measured on Signal).
- M2 belongs with M22a's adoption and provenance work, or with the M7b DriftCheck slot.

CHR-0 does increase exposure, though. Fresh mirrors make browse mode engage in Electron apps whose rows
are recycled (Signal uses virtualized lists), and every Rebuild re-matches through the ladder. Minimum
for CHR-0:
1. Land the FF SPA branch fixes first, so the displaced mark exists on main.
2. Add the math §3 "act only on what the user heard" guard, which is engine-agnostic: keep a 64-bit
   hash of the cursor's spoken (role, name) and compare it on Enter with the re-read the activation
   already does. It is cheap and independent of the matcher, and it would have prevented the real
   misclick the SPA review probe demonstrated (2nd "Reply" → 3rd). It can ride in either branch.
3. Add a T-1 `virtual-list` variant to the CHR-0 gate: Enter after a recycle must announce, never act
   on the neighbour.

## 4. DP-3 mode default (owner decides) — reviewer view

Core default (a), NVDA-like browse + auto pass-through, is correct and needs no code. But note that
**CHR-0 changes behaviour in every Electron app at once.** Today their mirrors may also go stale (same
G-1..G-4 path), which hides browse mode. After CHR-0, VS Code, Slack and Discord all get a buffer.
NVDA explicitly disables the buffer for VS Code (`refs/nvda/source/appModules/code.py`), and Orca forces
sticky focus mode for all Electron apps (`document_presenter.py:175-190, 850-861`).

Recommendation: (a) as the default, plus one first-party data row shipped *with* CHR-0 (or before it is
pushed): `code` → `buffer = Off`. This is the native `ext-chromium` table the architect already allows
before M18a (§2.5), so it is not a core special case. Signal's focus lands on `button`, which does not
pass through (`forms.rs:57`). Its tabs and focusable grid rows do pass through (`:41-43`), and that
matches NVDA.

## 5. What waits

- **M7b / W3:** LKA `Attach` diff with size-safe chunking; `DriftCheck` sweep (active doc only, GetTree
  vs GetMatches decided by measuring F6); orphan Parent-walk negative cache, if CHR-0 counters show it is
  needed.
- **M22a:** Blink S-1 quirk rows (EMBEDDED→application, after verifying `xml-roles`); SM-8 writer
  handoff and FR-I1; `Unknown` for adopted nodes; `repair:<gen>` provenance; the M2 identity rung and
  relocation classes feeding the displaced rule.
- **M14a:** per-app/site user mode override, "save mode as default".
- **M18a:** `DocModePolicy` in WIT 0.9, manifest presets, `request-mode`, `freshness` accessor, migration
  of the native `code` row.
- **Track U:** upstream request to emit structural events at load-complete (or to drop the `loaded`
  gate for documents an AT has already traversed).

## 6. Evidence

- Source: `refs/chromium/.../browser_accessibility_manager_auralinux.cc:129-139,192-201,254-263,514-546,589-597`;
  `ax_platform_node_auralinux.cc:4008-4056`; `ax_event_generator.cc:400-409`;
  `refs/at-spi2-core/atk-adaptor/adaptors/collection-adaptor.c:1055-1235`; `refs/nvda/.../chromium.py:79-119`.
- gorca: `doc_route.rs:55-87`, `toolkit.rs:149-159`, `doc_registry.rs:287-314,533-544`,
  `source.rs:454-498,640-690,1265-1360`, `gorca-dbus/src/drain/map.rs:337-361`,
  `browse/commands.rs:984-1135,1825-1835,2010-2052`, `browse/forms.rs:23-70`,
  `identity.rs:805-860`, `browse/sayall.rs:195-213`.
- Ran: `python -I tools/research/chromium_mirror_sim.py` (project venv, rc=0). The S0/S1/S2/S3 numbers
  reproduce the math report (Chromium profile S0 0.849 hit / 1 of 8 stuck; S2 17.7 ms/s UI). The sim does
  **not** model the `loaded` gate or load-complete.

## 7. Unverified

- E-1/E-2 not run. RC-1 is proven from source for Chromium 155 refs; the exact Electron 44 / Chrome 152
  code was not checked (the gate has existed for years, but its presence in 152 is unverified).
- Signal's load-complete timing relative to the 3 s settle window (G-2) is unknown; G-3 alone is
  sufficient.
- Whether Signal's virtualized rows recycle DOM nodes (and so AT-SPI paths) is unverified.
- B-1 latency under a 20k-node repair is not measured.
