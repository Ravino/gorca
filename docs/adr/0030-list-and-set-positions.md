# ADR-0030: List and set positions: sources declare, the core counts

Status: **Proposed** (r1, 2026-10-26). Owner: architect.
Amends: ADR-0022 §2.4 (computed positions leave `Semantics` and become a context value), ADR-0022 PR-4 (the spoken
rule is now this ADR's §2.5), ADR-0024 §2.6 admission (plugins may not assert `setpos`), ADR-0029 §2.6 (one
structural fact, `extent`, in the same WIT 0.11 bump). Builds on: ADR-0003 (`WebDoc` order-statistics mirror),
ADR-0016 / ADR-0028 §2.2.1 (report fields), `docs/reports/virtual-list-navigation-math.md` (branch
`math/virtual-list-nav`, commit `db16616d`, virtual-list detector rule D).
Slot: `docs/reports/list-position-math.md` (math-researcher, in flight) supplies the index structure of §2.7.

## 1. Context

The owner asked whether gorca should say list positions ("3 of 30") at all, and whether that is the core's job. An
implementer had proposed letting plugins send positions (`Value::Position`, today dropped by the plugin host,
`crates/gorca-plugin-host/src/host.rs:650`) and adding a presenter path for them.

**What exists (verified on `main` at `5042af6c`).**

- The model already has `SetPos { pos, size: Option<NonZeroU32>, src: PosSrc }` with `PosSrc::{Author, Engine,
  Computed}` (`crates/gorca-model/src/sem/states.rs:130,196`). The AT-SPI normaliser fills it from the `posinset` /
  `setsize` attributes as `PosSrc::Engine` (`crates/gorca-sem-atspi/src/attrs.rs:237`).
- Nothing in `gorca-present` or browse mode reads `sem.pos`. Positions are said in one place only, where-am-I, which
  counts **all** children of the parent and ignores the source value (`crates/gorca-core/src/commands/info.rs:58`).
- Browse mode says "list with N items" on entry by counting `ListItem | Option` children with an O(c) scan
  (`crates/gorca-core/src/browse/present.rs:183`, `crates/gorca-present/src/browse.rs:290`).
- ADR-0022 §2.4 puts computed positions **inside** `Semantics` with a sibling dependency edge; it was never built.
- The Telegram Web plugin re-roles `div`s into lists and list items (`docs/reports/telegram-web-live-diagnosis.md`
  §"Messages as a list", §"Chat list as a list"). The engine computed no position for those nodes: in the engine's
  tree they are not list items.

**Prior art.**

| | Per-item position | "list with N items" | Source of the numbers |
|---|---|---|---|
| Orca | setting `speak_position_in_set`, **default off** (`refs/orca/src/orca/speech_presenter.py:252-264`); said only for the focused object, never for ancestors, and only on the last line of its content (`speech_generator.py:1338-1366`); where-am-I detailed says it regardless | setting `announce_list`, **default on** (`speech_presenter.py:769-782`); "List of unknown size" when `setsize=-1` (`messages.py:2453-2467`) | `posinset`/`setsize` attribute first, then the parent's, then counts set members; over 500 children it uses index-in-parent and child count (`ax_utilities.py:1279-1314,1349-1374`) |
| NVDA | `reportObjectPositionInformation`, **default on** (`refs/nvda/source/config/configSpec.py:138`), for focus and object navigation; off for mouse (`speech/speech.py:977-990`) | `reportLists` default on (`configSpec.py:279`); browse control fields say "with N items" from `_childcontrolcount` for read-only lists (`speech/speech.py:2424-2437`) | browse control fields pass only the level, never the index (`speech/speech.py:2393`): **no "3 of 30" in browse mode** |
| Gecko | — | — | `Accessible::GroupPosition`: ARIA `aria-posinset`/`aria-setsize` first, else computed from the hierarchy (`refs/firefox/accessible/basetypes/Accessible.cpp:247-312`), exported to AT-SPI as `posinset`/`setsize` (`atk/AccessibleWrap.cpp:662`, `base/nsAccUtils.cpp:30-47`) |
| Chromium | — | — | ordered-set computation with `kSetSize`/`kPosInSet` (`refs/chromium/ui/accessibility/ax_tree.cc:2950-3070`), same AT-SPI attribute names |
| GTK4 | — | — | `posinset`/`setsize` exported from the list model (gtkatspicontext.c, see ADR-0022 §1.1) |

Three facts decide the design:

1. Over AT-SPI the consumer cannot tell an author's `aria-posinset` from the engine's computed one: both arrive as
   `posinset`. The engine's value is right for the engine's tree.
2. Once a plugin re-roles nodes, hides or flattens siblings, or inserts a synthetic container (ADR-0029), the fused
   tree is no longer the engine's tree, and the engine's numbers are missing or wrong.
3. Virtualised lists (Telegram, most chat and feed UIs) render only a window. A count of rendered items is a window
   size, not a list size. Saying "list with 40 items" for a 30 000-message chat is a lie.

## 2. Decision

### 2.1 Yes, positions are spoken, but where the conventions say so

Position is useful orientation for a list the user moves through item by item, which is focus mode (native lists,
menus, tabs, web listboxes, roving-focus chat lists) and the where-am-I query. In browse mode the line already carries
the content, and both NVDA and Orca keep the index out of it. gorca follows them (§2.5), with an opt-in key.

### 2.2 Ownership: sources declare, the core counts, plugins shape structure

- **Sources declare.** Connectors deliver what the platform says (`posinset`/`setsize`, AccessKit
  `position_in_set`/`size_of_set` through Newton) as `SetPos { src: Engine | Author }` in `Semantics`, unchanged.
- **The core counts.** Everything not declared is computed by one core resolver (§2.3) over the **published fused
  structure**, the same tree browse mode, quick nav and object navigation walk, including synthetic containers.
- **Plugins shape structure, they do not count.** A plugin makes a list by re-roling nodes or declaring a synthetic
  container (ADR-0029); the core then counts its items like any other list's. The implementer's proposal (admit
  `Value::Position` from plugins and add a presenter path for it) is **rejected**: every plugin would re-implement
  counting, its numbers would go stale under the core's own patches and churn, and it would make a plugin
  authoritative over a fact the core can compute exactly.
- **The one thing a plugin may say: the list is open-ended.** Only the plugin can know reliably, before any scroll,
  that the app virtualises a list. It may assert `extent = open-ended` on a list node (§2.4). It never asserts a
  number.
- `Value::Position` from a plugin stays rejected, now visibly: the host maps it to `NotAdmitted(setpos)` (counted,
  shown in `explain`), not to the silent `Value::Other`. The fact API keeps `setpos` for the Primary class (connectors,
  ADR-0024 §2.1 table, T1) and for user overrides (T0), not for the Patch class (plugins, T2/T3). A future case with a strong reason (an app whose
  data has a total the DOM lacks) needs an amendment of this ADR, not a plugin flag.

### 2.3 The resolver: precedence

One pure function in `gorca-present`'s context layer, `resolve_position(item) -> Option<PosAnswer>`, used by every
surface (focus, browse, quick nav, object navigation, where-am-I; LP-7).

**Set membership (computed).** The set of an item is the run of siblings under the same published parent (synthetic
containers count as parents; hidden nodes and flattened levels are already out of the published view) whose fused
role is in the item class of the parent's role:

| Parent role | Item roles |
|---|---|
| `List` | `ListItem` |
| `Listbox` | `Option` |
| `Menu`, `Menubar` | `MenuItem`, `MenuItemCheckbox`, `MenuItemRadio` |
| `TabList` | `Tab` |
| `Tree`, `Group` under a tree item | `TreeItem` (same level) |
| `RadioGroup` | `Radio` |
| `Feed` | `Article` |

Other children (separators, wrappers the plugin did not re-role) are skipped, not counted. Nested lists are their own
sets. Tables and grids keep ADR-0022 `GridPos` and are out of scope here.

**Precedence, per field (position and size resolved independently):**

1. **Declared, structure unpatched.** `sem.pos` from the source (`Engine` or `Author`) is used when the item's role,
   its parent's role and the parent's child set are all unpatched: the item's `ProvTag` role nibble is the observed
   stratum S0, and the parent's `struct_patched` counter (§2.7) is 0. This is the common case (real `<ul>`, ARIA
   listboxes, GTK lists) and keeps an author's `aria-posinset="1003" aria-setsize="30000"`.
2. **Declared size, patched structure.** A declared `setsize` (from items or from `setsize=-1`) is still taken under
   patches when it is the same on every item that carries one; a declared `posinset` is not (the plugin may have
   moved the item into a different set).
3. **Computed.** Otherwise `pos = 1 + |preceding set members|`, `size = |set members|`, `src = Computed`, subject to
   the extent rule (§2.4).

Validation (from ADR-0022 PR-4, kept): `pos ≥ 1`; a size that is 0 or smaller than `pos` is dropped; a `setsize`
on the container node itself is ignored.

```rust
// gorca-present (context layer). Sketch, not frozen.
pub enum ListExtent {
    Complete(u32),   // computed over a list the core knows to be whole
    Declared(u32),   // author or engine setsize
    OpenEnded,       // setsize = -1, INDETERMINATE, plugin `extent`, or virtual-list detector
    Pending,         // mirror coverage of the list is not complete yet (ADR-0003 coverage)
}
pub struct PosAnswer { pub pos: Option<u32>, pub extent: ListExtent, pub src: PosSrc }

pub trait SetIndex {                     // implemented by WebDoc and by the app actor's node store
    fn item_pos(&self, item: NodeRef) -> Option<PosAnswer>;   // O(log n), §2.7
    fn extent(&self, list: NodeRef) -> ListExtent;            // O(1)
}
```

### 2.4 Honest totals: the extent rule

A total is spoken only when it is `Declared` or `Complete` (LP-2).

| Extent | List entry | Per item |
|---|---|---|
| `Declared(n)` | "list, n items" | "p of n" (declared or computed `p`) |
| `Complete(n)` | "list, n items" | "p of n" |
| `OpenEnded` | "list" (no count) | declared `posinset` only, as "item p" (Orca's `GROUP_INDEX_TOTAL_UNKNOWN_SPEECH`); a computed `p` is a window index and is **not** said |
| `Pending` | "list" (no count), `may_refine` | nothing until coverage completes |

`OpenEnded` holds when any of:

- a declared `setsize = -1` on the items, or the `INDETERMINATE` state on the list (Orca's
  `get_set_size_is_unknown`);
- a plugin's `extent = open-ended` on the list node (T2 built, T3-trusted built and tainted, T3 advisory; the
  ADR-0029 trust table);
- the virtual-list detector's rule D for that container: declared (`setsize` greater than rendered items, or the
  first rendered `posinset > 1`) or at least 2 scroll-attributed churn episodes
  (`virtual-list-navigation-math.md` §4: FPR ≈ 2.9 %, FNR ≈ 8.6 % at 2 episodes). The mark is sticky for the
  document's lifetime.

`Complete(n)` for web content needs a list that is not `OpenEnded` and whose subtree coverage in the mirror is
complete. Native lists: the toolkit's child list is the whole list (GTK3 tree views export every row, GTK4 exports
`setsize`), so a `ChildList::Known` parent is `Complete`; with only `ChildList::Count(n)` and no cached roles, the
Orca shortcut applies for item-only parents (`List`, `Listbox`, `Tree`): index-in-parent and child count. No IPC on
the presentation path (ADR-0022 SM-6): an unknown answer is no answer.

Known limit: an undeclared virtual list without a plugin is `Complete(window)` until the detector has seen two
scrolls (detector FNR ≈ 60 % before that). This is the same as Orca and NVDA today. The Telegram plugin asserts
`open-ended` on its message list and chat list, so Telegram never hears a window count.

### 2.5 When it is spoken, and the report fields

| Reason (ADR-0022 §2.5) | Per-item position | List entry count |
|---|---|---|
| `Focus` (focus mode, native and web) | **on**: `report.position` ≥ `focus` | focus ancestry enters the list with "list" only; the item's "of n" carries the total |
| `WhereAmI` | always (a query; not a report field) | always |
| `BrowseLine`, `QuickNav` (`I`, `L`), `SayAll` | off: `report.position = always` turns it on | **on**: `report.list_item_count` |
| object navigation (`Review`) | as `Focus` | as `Focus` |
| `Change`, `ListEntry` (chooser) | never / chooser keeps its own counter | — |

The position is the **last** field of the utterance ("Alice, link, 3 of 30"), after name, role, states and
description, so a fast listener's next key cuts it off. It is a catalog message (`info-position` already exists:
`{at} of {of}`; new `position-no-total` = `item {at}`).

Report fields (ADR-0016 `report.*`, compiled to `ReportFlags`; global, per profile, per app and site rows; bundled in
the presets; each with a quick toggle, unbound by default):

| Key | Values | Default | Quick toggle | Governs |
|---|---|---|---|---|
| `report.position` | `off` / `focus` / `always` | `focus` | `report.position_cycle` (off → focus → always → off) | per-item "p of n" / "item p" |
| `report.list_item_count` | bool | `true` | `report.list_item_count_toggle` | ", n items" on list entry (browse, quick nav `L`, say all) |
| `report.lists` | bool (ADR-0029 §2.4) | `true` | existing | the list entry and exit words themselves |

Presets: "Beginner" = `always` + count on; "Default" = `focus` + count on; "Terse" = `off` + count off. Silencing any
of them changes no navigation and no where-am-I answer (VA-11, LP-6).

### 2.6 Pipeline placement (amends ADR-0022 §2.4)

Computed positions move from class 1 (inside `Semantics`, dependency edge to siblings) to class 2 (context values,
read at compose time). Reason: under class 1, one insertion at the head of a 10 000-item list bumps the `sem_rev` of
every following sibling (O(n) writes, cache invalidations and change-diff candidates per `children-changed`), and a
virtualised list churns exactly there. `SemEntry.sem.pos` keeps only declared values (`Author`, `Engine`, connector
facts). `PosSrc::Computed` never appears in a `SemEntry`; it appears in `PosAnswer` and in the `PresIr` field's
`Origin`, so `explain` shows "computed: 3 of 30 over 30 list items under <list>".

Changes to positions are never spoken by themselves (an item becoming "4 of 31" because a message arrived is not a
state change, ADR-0022 ST rules).

### 2.7 Cost model (slot for the math result)

- **Key path:** `item_pos` is O(log n) and `extent` O(1); no scan of siblings, no IPC. Budget: p99 ≤ 5 µs per call
  at a 100 000-node document with a 10 000-item list, inside the 15 ms keypress budget with three orders of margin.
- **Update path:** each `DocOp` (`InsertSubtree`, `MoveSubtree`, `SyncChildren`, `RemoveSubtree`, `SetProps` of a
  role) maintains the counts in O(k log n) for k touched nodes, like the existing per-category counts
  (`crates/gorca-doc/src/web/doc.rs:1144,1174`, `cats.rs` `RANKED` already has `LIST` and `LIST_ITEM`). The writer
  keeps `struct_patched: u16` per parent (children with a role patch, `hide`, `flatten`, or a synthetic level),
  updated in the same `FUSE` step, O(1).
- **Structure:** the exact per-parent order-statistics design (counting set members among a parent's children,
  excluding nested lists, under patches and window shifts) is the math-researcher's
  `docs/reports/list-position-math.md` (branch in flight). Its result fills this section in r2; LP-4 and the M-1
  budget are the acceptance criteria it must meet.
- **Interim (until the index lands):** an O(c) count of the parent's children, capped at c ≤ 512; above the cap the
  answer is `Pending` (no total, no computed position). The current browse entry count (`browse/present.rs:183`) is
  replaced by the resolver with the same cap.
- **Native:** the app actor's `ChildList::Known` gives index and count; c is small (menus, tab lists); the same
  512 cap applies.

### 2.8 Plugin API changes (WIT 0.11, the single WIT-11 bump)

- `structure` import (ADR-0029 draft `wit/drafts/gorca-ext-0.11-synth/structure.wit`) gains
  `set-extent: func(list: fact-target, e: extent)` with `enum extent { open-ended, unknown }` (`unknown` withdraws
  the plugin's claim). Grant `structure`. Bounded like ADR-0029 SN-6 (≤ 64 per owner per scope).
- `facts.assert` with group `setpos` from a plugin: `NotAdmitted`, counted, visible in `explain`.

## 3. Alternatives considered

| Alternative | Why not |
|---|---|
| A. Plugins send positions (the implementer's proposal) | duplicate counting in every plugin; wrong after the core's own patches and synthetic levels; the plugin cannot see the fused tree; turns a computable fact into a trusted claim |
| B. Trust only the engine's `posinset`/`setsize` | missing for every plugin-made list (Telegram's `div`s) and wrong under `hide`/`flatten`/synthetic containers |
| C. Computed positions inside `Semantics` (ADR-0022 §2.4 as written) | O(n) `sem_rev` bumps per head insertion; worst exactly on churning virtual lists |
| D. Always say a count (rendered children), like the current browse code | dishonest for virtual lists: "40 items" for a 30 000-message chat |
| E. Position in browse lines by default | neither NVDA nor Orca does it; extra words on every line for a 1000 wpm listener; available as `report.position = always` |
| F. Orca's default (focus position off) | NVDA's default is on, ADR-0022 PR-4 already chose on, and the field is last so it costs nothing to a fast listener who moves on; open question Q-1 |

## 4. Consequences

- One resolver answers every surface; where-am-I stops counting non-items ("3 of 7" in a list with a heading and a
  separator becomes "3 of 5").
- The Telegram message list and chat list get honest speech without any plugin counting: entry says "list", focus
  on an item says no window index; after the owner opts into `always` nothing else changes.
- ADR-0022 §2.4 loses the sibling dependency edge; `sem_rev` no longer moves with siblings.
- WIT 0.11 grows one function; `Value::Position` handling in the host changes from silent drop to `NotAdmitted`.

## 5. Invariants

- **LP-1** Positions and totals are computed only by the core resolver. Plugins never assert a number.
- **LP-2** A total is spoken only when the extent is `Declared` or `Complete`.
- **LP-3** Computed positions are never stored in `SemEntry`; a sibling's insertion bumps no `sem_rev`.
- **LP-4** The key path does O(log n) work for a position and O(1) for an extent; no IPC; bounded by the 512 cap
  until the index lands.
- **LP-5** A declared `posinset` is used only while the item's role and its parent's child set are unpatched.
- **LP-6** Report fields gate words only; navigation and where-am-I answers are unchanged by them.
- **LP-7** The same item gives the same `PosAnswer` on every surface (ADR-0022 PR-3).

## 6. Metrics to validate

| Id | Metric | Target |
|---|---|---|
| M-1 | `item_pos` p99, 100k-node doc, 10k-item list, under 1k ops/s churn | ≤ 5 µs (interim scan: ≤ 50 µs at c = 512) |
| M-2 | `DocOp` throughput loss from set counts | ≤ 10 % vs `main` on the B-2 web corpus |
| M-3 | Resolver vs engine agreement on unpatched Firefox and Chromium list fixtures | 100 % (`pos`, `size`) |
| M-4 | Telegram fixtures (message list, chat list) | entry says "list", no "of n" with a window count, 0 utterances with a total |
| M-5 | Keypress → speech p99 (CLAUDE.md budget) on the Telegram fixture with `report.position = always` | unchanged within noise |
| M-6 | Detector-marked lists on the B-2 corpus (non-virtual lists marked `OpenEnded`) | ≤ 3 % (math report FPR) |

## 7. Slices

- **LP-a** Resolver + native focus position (declared values from GTK4/Gecko/Chromium, last field), where-am-I on the
  resolver, `report.position`, `info-position` / `position-no-total`. Interim cap.
- **LP-b** Web computed positions over the fused tree, `struct_patched`, extent from declared values and coverage,
  `report.list_item_count`; browse entry count via the resolver.
- **LP-c** `extent` structural fact (WIT-11), plugin `setpos` → `NotAdmitted`, Telegram plugin asserts open-ended,
  detector hook (rule D) when the virtual-list cursor lands.
- **LP-d** Order-statistics index from `list-position-math.md`; removes the cap; M-1/M-2 measured.

## 8. Open questions (owner)

- **Q-1** Focus-mode position default: **on** (NVDA, this ADR) or off (Orca)?
- **Q-2** An open-ended list on entry: "list" (this ADR) or "list of unknown size" (Orca)?
- **Q-3** For Telegram specifically, do you want positions in browse mode (`report.position = always` in the
  telegram site row), knowing they will be declared-only ("item 1003") or absent, never window indices?

## 9. Changelog

**r1 (2026-10-26)** — first version: sources declare, core counts, plugins only shape structure and may say
`open-ended`; precedence and extent rules; NVDA/Orca-aligned defaults; `report.position` and
`report.list_item_count`; computed positions moved out of `Semantics`; cost slot for `list-position-math.md`.
