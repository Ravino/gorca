# ADR-0030: List and set positions: sources declare, the core counts

Status: **Accepted** (r2, 2026-10-26; owner answers Q-1..Q-3 in §8, index and virtualised-list rule from
`docs/reports/list-position-math.md` in §2.3, §2.4, §2.7). Owner: architect.
Amends: ADR-0022 §2.4 (computed positions leave `Semantics` and become a context value), ADR-0022 PR-4 (the spoken
rule is now this ADR's §2.5), ADR-0024 §2.6 admission (plugins may not assert `setpos`), ADR-0029 §2.6 (one
structural fact, `extent`, in the same WIT 0.11 bump). Builds on: ADR-0003 (`WebDoc` order-statistics mirror),
ADR-0016 / ADR-0028 §2.2.1 (report fields), `docs/reports/virtual-list-navigation-math.md` (branch
`math/virtual-list-nav`, commit `db16616d`), `docs/reports/list-position-math.md` (design A, rule RL; code and
test vectors in `tools/research/list_position/`).

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

**Set membership (computed), r2: nearest-list ownership.** An item belongs to its **nearest list ancestor** in the
published fused tree (synthetic containers count; hidden nodes and flattened levels are already out of the published
view), crossing any non-list wrappers between them; an item inside a nested list belongs to the nested list only.
Formally (math report §1): `owner(x)` = nearest proper ancestor `y` with `LIST ∈ cats(y)`; `members(L)` = the items
with `owner = L`, in document order.

Why nearest-list and not direct children:

- **Real pages wrap items.** React-style renderers put generic `div`s between a list and its items (Telegram's chat
  list has both wrapped and bare rows, `docs/reports/telegram-web-live-diagnosis.md` §"Chat list as a list"), and
  ARIA 1.2 allows `generic` between `list` and `listitem`. Direct-children counting says "0 items" for such a list,
  and a plugin would have to re-role or flatten every wrapper just to get a count. That is the drift this ADR
  removes.
- **Same answer as the user's model.** Quick nav `I` and the elements list already treat such items as items of
  that list; the count must agree (LP-7).
- **Cost.** With the same signed weights, direct-children semantics gives unowned items weight 0 and makes a list
  toggle (a late role patch, a synthetic wrap) cost O(children); nearest-list keeps every op at O(log n + D) (math
  report §2.1, §6).
- Nested lists stay separate, as in Gecko's and Chromium's group computation and in both screen readers.

Design A (§2.7) covers the list family (`List`, `Listbox` with `ListItem`, `Option`: the `LIST` / `LIST_ITEM`
categories). The other families in the table keep declared values (engines compute them for real ARIA widgets,
which plugins rarely re-shape). When one of them is patched, its computed position is a count over the parent's
direct children, bounded at 256 members (beyond that: no computed position, no total). A per-family weight triple
(+12 B/node each, math report §5) is added only when a measurement shows a patched large menu, tab list or tree.

Item classes per family:

| Parent role | Item roles |
|---|---|
| `List` | `ListItem` |
| `Listbox` | `Option` |
| `Menu`, `Menubar` | `MenuItem`, `MenuItemCheckbox`, `MenuItemRadio` |
| `TabList` | `Tab` |
| `Tree`, `Group` under a tree item | `TreeItem` (same level) |
| `RadioGroup` | `Radio` |
| `Feed` | `Article` |

Other descendants (separators, wrappers) are crossed, not counted. Nested lists are their own sets. Tables and grids keep ADR-0022 `GridPos` and are out of scope here.

**Precedence, per field (position and size resolved independently):**

1. **Declared and consistent, structure unpatched.** `sem.pos` from the source (`Engine` or `Author`) is used when
   (a) the item's `ProvTag` role nibble is the observed stratum S0 and its owner list's `struct_patched` counter
   (§2.7) is 0, and (b) the declared values are consistent (RL-1, §2.4). This is the common case (real `<ul>`, ARIA
   listboxes, GTK lists) and keeps an author's `aria-posinset="1003" aria-setsize="30000"`.
2. **Declared size, patched structure.** A declared `setsize` (including `-1`) is still taken under patches when it
   is consistent (RL-1); a declared `posinset` is not (the plugin may have moved the item into a different set).
3. **Computed.** Otherwise `pos = 1 + |preceding set members|`, `size = |set members|`, `src = Computed`, subject to
   the extent rule (§2.4).

Validation (from ADR-0022 PR-4, kept): `pos ≥ 1`; a size that is 0 or smaller than `pos` is dropped; a `setsize`
on the container node itself is ignored.

```rust
// gorca-present (context layer). Sketch, not frozen.
pub enum ListExtent {
    Complete(u32),   // computed over a list the core knows to be whole
    Declared(u32),   // author or engine setsize
    OpenEnded,       // setsize = -1, INDETERMINATE, plugin `extent`, or a suspect list (RL-3)
    Pending,         // mirror coverage of the list is not complete yet (ADR-0003 coverage)
}
pub struct PosAnswer { pub pos: Option<u32>, pub extent: ListExtent, pub src: PosSrc }

pub trait SetIndex {                     // implemented by WebDoc and by the app actor's node store
    fn item_pos(&self, item: NodeRef) -> Option<PosAnswer>;   // O(log n + D), §2.7
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

The total and position rule (r2; the math report's rule RL, §3.2), in order:

- **RL-1 Author data.** Declared values are consistent when every rendered member that carries a `setsize` carries
  the same value `> 0`, `setsize ≥` the largest `posinset`, and the `posinset` values are distinct and increase in
  document order. They then give both the total (`Declared(n)`) and the position. The check is incremental: each
  query compares the item with its previous and next members (`list_kth`, O(log n)). A failed check marks the list
  `decl_inconsistent` (sticky until its members change), and declared values are then ignored for it.
  `setsize = -1` on the items, or the `INDETERMINATE` state on the list (Orca's `get_set_size_is_unknown`), makes
  the list `OpenEnded`.
- **RL-2 Plugin fact.** A plugin's `extent = open-ended` on the list node makes it `OpenEnded` (T2 built,
  T3-trusted built and tainted, T3 advisory; the ADR-0029 trust table). Unlike the option in the math report, a
  plugin never supplies a total (LP-1).
- **RL-3 Suspicion.** The list is *suspect*, hence `OpenEnded`, once any of these is observed: the first rendered
  `posinset > 1`; a declared `setsize` larger than the rendered members; the role `Feed`; **one** scroll-attributed
  churn episode (members added or removed at an end of the list within 300 ms of a scroll, a gorca `ScrollTo` or a
  focus move inside it). One episode is enough because a false alarm only withholds a number. Math report §3.2:
  0 % of correct totals withheld; false totals 3.2 % (general web) and 11.2 % (chat apps), against 15.2 % and
  53.4 % for the NVDA/Orca behaviour. `aria-busy` alone does not mark a list: it already suppresses speech
  (ADR-0022 §2.7), and a list that loads once would be withheld forever.
- **RL-4 Memory per list identity.** Suspicion is remembered per list identity: document origin + list signature
  (the ADR-0029 anchor of a synthetic list, else the role/class path from the nearest landmark). It survives
  revisits and reloads for the session, in RAM (ADR-0002 PR-4: no new persisted data), and is cleared only when a
  source epoch bump changes the signature. Persisting it across sessions (hashes of origin and signature, no
  content) needs a later amendment, once measured.
- **RL-5 Otherwise** design A's DOM count and rank are exact: `Complete(n)`, "p of n".

`Complete(n)` for web content needs a list that is not `OpenEnded` and whose subtree coverage in the mirror is
complete. Native lists: the toolkit's child list is the whole list (GTK3 tree views export every row, GTK4 exports
`setsize`), so a `ChildList::Known` parent is `Complete`; with only `ChildList::Count(n)` and no cached roles, the
Orca shortcut applies for item-only parents (`List`, `Listbox`, `Tree`): index-in-parent and child count. No IPC on
the presentation path (ADR-0022 SM-6): an unknown answer is no answer.

Known limit (proven unavoidable, math report §3.1): on its first visit, an undeclared virtual list without a plugin
is `Complete(window)` until one window move is seen, because an idle window cannot be told apart from a short static
list. This is no worse than Orca and NVDA, and RL-4 makes it happen once per list per session. The Telegram plugin asserts
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

### 2.7 Index: signed-weight set rank on the document treap (design A, the specified implementation)

r2 replaces the r1 interim 512-item cap. Source: `docs/reports/list-position-math.md` §2.1 (Invariant W and
corollaries R and K proven; property test `prop_test.py 300 80`, 24k random ops, full agreement with the oracle).

- **Weights.** Each `WebDoc` treap node gets `w: i32`: `+1` for a visible list item, `−c(L)` for a visible list
  `L` that owns `c(L)` items, `0` otherwise. The document root is a sentinel list and is never reported. The treap
  summary gains `wsum` and `wmax` (the maximum non-empty prefix sum), folded into the existing pull next to `cnt`
  (`crates/gorca-doc/src/web/doc.rs`). The kind comes from the node's own cats (`LIST_ITEM`, `LIST`;
  `crates/gorca-doc/src/web/cats.rs`), so hidden and presentational nodes drop out for free. +12 B/node (1.2 MB at
  100k nodes).
- **Queries** (read-only, no splits, deterministic): `list_count(L)` is O(1) (stored `c(L) = −w(L)`);
  `list_rank(x) -> (owner, p)` = `1 + Σ w over (pos(L), pos(x))` is O(log n + D), where D is the owner-walk depth;
  `list_kth(L, k)` is a `wmax`-guided descent, O(log n). `list_kth` serves the RL-1 neighbour checks and the
  first/last/item-k commands.
- **Updates.** Every `DocOp` changes the weight of at most three nodes (the node, its old owner, its new owner).
  This covers insert, remove, move, `SetProps` role or hidden toggles, a node becoming or ceasing to be a list, and
  ADR-0029 wrap and dissolve. Each costs O(log n + D) on top of the treap op already paid; reorder costs nothing. A
  role patch that turns a node with 10k item children into a list costs O(log n + D), not O(10k). Hook points: the
  `SetProps`/patch path (`Translator::patch_sem` and the cats update in `doc.rs`), the `InsertSubtree` build, and
  the split during remove/detach (math report §5). An optional `WrapRun` op makes an ADR-0029 wrap a single treap
  insert.
- **`validate()`** gains `wsum(root) == 0` and `c(L) == Σ w(interior L)` for every list. The test vectors
  `tools/research/list_position/vectors.json` (5 scenarios, 148 ops) become a `gorca-doc` test.
- **Measured** (Python model, in steps): at 100k nodes with a 10k-item list, key rank + count p99 is 112 steps
  (≈ 0.5–1.2 µs in Rust) and kth p99 is 171; patch ops p99 are 56–430 steps, including the existing treap work;
  Telegram fixtures key p99 is 42–52 steps. Worst case D = 500 (the Blink parser's nesting cap): ≈ 3 µs.
- **Bounded key path:** "p of n" costs ≤ D + 4h steps and "n items" O(1); no scan, no IPC, independent of list
  length. The RL-1 neighbour check adds ≤ 2 kth descents.
- **`struct_patched: u16` per list** counts the members with a role patch, `hide`, `flatten` or a synthetic level.
  It is kept in the same `FUSE` step, O(1), and feeds precedence step 1.
- **Native:** the app actor's `ChildList::Known` gives index and count over direct children (native toolkits do not
  wrap list items). Native sets are small (menus, tab lists) or declared (GTK4 `setsize`); the 256-member bound of
  the non-list families applies.

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
| F. Orca's default (focus position off) | NVDA's default is on, ADR-0022 PR-4 already chose on, and the field is last so it costs nothing to a fast listener who moves on; owner chose on (Q-1, §8) |

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
- **LP-4** The key path does O(log n + D) work for a position and O(1) for a total; no IPC; independent of list
  length.
- **LP-5** A declared `posinset` is used only while the item's role and its parent's child set are unpatched.
- **LP-6** Report fields gate words only; navigation and where-am-I answers are unchanged by them.
- **LP-7** The same item gives the same `PosAnswer` on every surface (ADR-0022 PR-3).

## 6. Metrics to validate

| Id | Metric | Target |
|---|---|---|
| M-1 | `item_pos` p99, 100k-node doc, 10k-item list, under 1k ops/s churn | ≤ 2 µs (Rust, design A) |
| M-2 | `DocOp` throughput loss from the weights | ≤ 10 % vs `main` on the B-2 web corpus |
| M-2b | `validate()` W invariant and the 148-op vector suite, after every op | 100 % pass |
| M-3 | Resolver vs engine agreement on unpatched Firefox and Chromium list fixtures | 100 % (`pos`, `size`) |
| M-4 | Telegram fixtures (message list, chat list) | entry says "list", no "of n" with a window count, 0 utterances with a total |
| M-5 | Keypress → speech p99 (CLAUDE.md budget) on the Telegram fixture with `report.position = always` | unchanged within noise |
| M-6 | Suspect-marked lists on the B-2 corpus that are not virtual | ≤ 3 % |
| M-7 | Undeclared-virtual share of lists in Telegram, Slack, Signal (re-tunes RL-3) | measured and reported |

## 7. Slices

- **LP-a** Resolver + native focus position (declared values from GTK4/Gecko/Chromium, last field), where-am-I on the
  resolver, `report.position`, `info-position` / `position-no-total`.
- **LP-b** Design A in `WebDoc` (weights, `list_count` / `list_rank` / `list_kth`, `validate`, the vector suite),
  nearest-list membership, `struct_patched`, RL-1 consistency, `report.list_item_count`; browse entry count and
  where-am-I through the resolver. The O(c) `facts_of` scan (`browse/present.rs:183`) is deleted in the same slice
  (ADR-0022 §8.3). M-1, M-2, M-2b measured.
- **LP-c** `extent` structural fact (WIT-11), plugin `setpos` → `NotAdmitted`, the Telegram plugin asserts
  open-ended on its message and chat lists, RL-3 suspicion (churn attribution) and RL-4 per-identity memory.

## 8. Owner answers (2026-10-26)

- **Q-1** Positions in focus mode are **on** by default (NVDA): `report.position = focus`.
- **Q-2** A list of unknown size is announced simply as **"list"**, with no "unknown size" wording; the catalog has
  no such message.
- **Q-3** Positions stay **off in browse mode for everyone**, Telegram included; no site or app row overrides the
  default. `report.position = always` stays a user's own choice.

## 9. Changelog

**r1 (2026-10-26)** — first version: sources declare, core counts, plugins only shape structure and may say
`open-ended`; precedence and extent rules; NVDA/Orca-aligned defaults; `report.position` and
`report.list_item_count`; computed positions moved out of `Semantics`; cost slot for `list-position-math.md`.

**r2 (2026-10-26)** — Accepted with the owner's answers (§8). Nearest-list ownership instead of direct children
(§2.3; justified by wrappers, consistency with quick nav, and cost). Design A (signed weights on the document treap)
is the specified index and replaces the 512-item cap (§2.7). The math report's rule RL is adopted as RL-1..RL-5
(§2.4): one scroll-attributed churn episode marks a list suspect, remembered per list identity for the session;
plugins still never supply totals. Metrics and slices updated (LP-d folded into LP-b).
