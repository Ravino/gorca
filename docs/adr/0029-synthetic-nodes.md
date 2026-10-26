# ADR-0029: Synthetic nodes: virtual containers and virtual buttons in the model

Status: **Proposed** (r1, 2026-10-26). Owner: architect.
Amends: ADR-0022 §2.9 (a third structure patch beside `hide` and `flatten`: `group`), ADR-0028 §2.2 (virtual buttons
are built by this mechanism), ADR-0026 §2.7 (new grant `structure`). Builds on: ADR-0003 (web mirror, `WebDoc`,
categories), ADR-0006 (object navigation), ADR-0024 (facts, tiers), ADR-0016 / ADR-0028 §2.2.1 (report fields).
Draft WIT (paper only): `wit/drafts/gorca-ext-0.11-synth/structure.wit`, part of the single 0.11 bump WIT-11
(parsed with `wit-parser` 0.261 together with the 0.10 `types`/`host` files and the ADR-0028 `vactions` draft:
`ok gorca:ext@0.11.0 interfaces=12 worlds=2`).

Why a separate ADR and not ADR-0028 r4: this is model structure, not actions. A synthetic node changes the tree every
surface walks (browse lines, quick nav, object navigation, where-am-I, focus ancestry), and ADR-0028's virtual button
is just the leaf case of the same thing. One mechanism, one ADR.

## 1. Context

**Owner requirement (2026-10-26).** The Telegram Web A chat header is avatar, title (`h3.fullName`), status and the
search / call / menu buttons laid out as **flat direct children of `div.messages-layout`**, followed by
`div.Transition` (the message list). There is no node to patch into a "Chat header" landmark (re-role needs one node;
`docs/reports/telegram-web-live-diagnosis.md` §"Chat header", fixture `tests/fixtures/web/telegram-a-open-chat.json`
node 2). The plugin must be able to declare a virtual container in gorca's model that groups a contiguous run of
siblings, with a role and a catalog name, while the DOM stays untouched.

**What exists.**

- The web mirror `WebDoc` (`crates/gorca-doc/src/web/doc.rs:101`) is an order-statistics tree with per-category
  counts (`cnt: Vec<[u32; RANKED_N]>`), changed only by `DocOp`s (`web/ops.rs:25`): `InsertSubtree`, `MoveSubtree`
  (ids preserved), `SyncChildren` (minimal moves, |C| − LIS), `RemoveSubtree`, `SetProps`. Quick-nav categories `Cat`
  are a pure function of `(role, level, flags)` (`web/cats.rs`). So inserting a node and moving a run under it is
  already an incremental, id-preserving operation with O(k log n) index maintenance.
- ADR-0022 §2.9 already puts one structure patch, `flatten`, in the mirror structure (not in `Semantics`); `hide` is
  the S1 existence group. A container is the inverse of `flatten`.
- Browse mode already re-anchors a cursor whose node moved or vanished (`cursor_chain`, `cursor_item`,
  `crates/gorca-core/src/browse/mod.rs`) and already announces landmark/region/group/list entry and exit for real
  containers.
- Known defect being fixed elsewhere (2026-10-26): plugin role patches are presented when arrowing but `H` does not
  find them, i.e. the category index is fed from the source spec, not from the fused value. This ADR depends on that
  fix (SN-5).

Prior art: Orca and NVDA have no synthetic containers; NVDA add-ons override `NVDAObject.parent`/`children` in app
modules (imperative, per toolkit). Chromium's and Gecko's own accessibility trees insert generic containers and
prune others, which is the same idea at the engine level. ARIA `aria-owns` re-parents, which we explicitly do not do
(no reordering, SN-3).

## 2. Decision

**Synthetic nodes** are nodes of gorca's published model that no source has. There are exactly two kinds, built by
one mechanism in the node writer, after `FUSE`, in the same publish:

| Kind | Declared by | Shape | Used for |
|---|---|---|---|
| **Container** | `structure.group` (this ADR) | interior node inserted between a parent and a contiguous run of its children | regions, landmarks, groups, toolbars, lists the page did not mark up |
| **Button** | `vactions.offer` with `show = button` (ADR-0028) | leaf inserted right after its host | a first-party offered action as a browse / object-nav stop |

### 2.1 Model and identity

```rust
// gorca-model (neutral; paper only)
pub struct SynthKey { owner: SourceSlot, anchor: AnchorId, local: u16 }   // NodeKey::Synth(SynthKey)
pub enum SynthKind {
    Container { role: SynthRole, name: ExtMsg, span: SpanSpec },          // SpanSpec = the WIT `span`
    Button { host: NodeKey, action: u16 },                                // ADR-0028
}
pub struct SynthNode { key: SynthKey, kind: SynthKind, basis: RevToken, state: SynthState }
pub enum SynthState { Resolving, Active { first: NodeKey, last: NodeKey, len: u16 }, Dissolved(Dissolve) }
pub enum Dissolve { MembersGone, NotContiguous, Overlap, Secret, Withdrawn, SourceMasked, ScopeClosed, Limit }
```

- **Identity.** `SynthKey` = owner slot + the host-minted anchor of the declared parent (ADR-0026 §2.3.3, a DOM id or
  structural anchor that survives re-renders) + the owner-local id. The `DocNodeId` the mirror allocates for it is
  stable for the life of the declaration, so a cursor or object-nav position on the container survives member churn.
  Keys live in a reserved space: no connector request ever names one, no selector ever returns one, no W-lane read
  sees one (ADR-0026 PF-2 unchanged).
- **Declaration is data.** The span is resolved by the host, not computed by plugin code, on every change of the
  parent's children, so a re-rendered header (new node ids, same classes) stays grouped without a new plugin call.
- **Lifetime.** A synthetic node is a fact of its owner's source: it goes when the owner withdraws it (`ungroup`,
  `withdraw`), the parent (container) or host (button) is removed, the source epoch bumps (reload, settings, re-derive),
  the slot is masked, backs off or is disabled, the scope closes, or a T3-trusted declaration's TTL lapses.

State machine of a container (per declaration, owned by the node writer):

| State | Event | Next | Mirror ops |
|---|---|---|---|
| — | `group` admitted | Resolving | — |
| Resolving | span resolves to ≥ 2 contiguous children, no rule broken | Active | `InsertSubtree(parent, before = first, synth)` + `MoveSubtree` of each member under it |
| Active | parent's children changed | Resolving → Active (re-anchor) or Dissolved | `SyncChildren(parent, children with the run replaced by the container)` and `SyncChildren(container, run)`; members keep their ids |
| Active / Resolving | members gone, run not contiguous, overlap, secret inside, limit | Dissolved(reason) | members moved back to the parent at the container's place, `RemoveSubtree(container)` |
| any | withdraw, source masked, scope closed | Dissolved | same |
| Dissolved | span resolves again (only for `MembersGone`/`NotContiguous`, within the same declaration) | Active | as Resolving |

A dissolve or re-anchor never moves the browse cursor off a member (ids preserved). A cursor *on* a dissolved
container re-anchors to its first former member through the existing `cursor_chain`.

### 2.2 Rules (limits)

- **SN-1 Contiguous siblings only.** Members are ≥ 2 consecutive children of one parent in source order. `set` spans
  whose matches are not contiguous are not built (`NotContiguous`, counted, visible in `explain`).
- **SN-2 Proper nesting.** Containers may nest (a run inside another container's run) or be disjoint; a partial overlap
  with an existing container (any owner) is refused for the later declaration (`Overlap`). A container never spans a
  synthetic button's host without its button (the button moves with its host).
- **SN-3 No reordering, no hiding, no text.** A container adds one tree level; it does not change the order of
  anything, does not hide or flatten anything, and has no text of its own. The flat text changes only by the line
  breaks its role normally implies (`NodeFlags::BREAK_BEFORE/AFTER` for block roles), so offsets of existing text
  move but their order never does.
- **SN-4 No secrets.** A run that contains a secret node or a subtree with one is refused (`Secret`), and an active
  container dissolves when one appears (AC-1, SS-3).
- **SN-5 One index path.** Synthetic nodes enter the browse index through the same `DocOp`s and the same fused-value
  category feed as every node; the feed is the post-`FUSE` published value (the fix in flight for "role patch visible
  when arrowing, not found by H"). No consumer has a synthetic-node special case.
- **SN-6 Bounds.** Per scope: ≤ 64 containers per owner, ≤ 256 synthetic nodes in total (buttons included), ≤ 256
  members per container, nesting depth of synthetic containers ≤ 2. Over a bound: `Limit`, counted, oldest kept.
- **SN-7 Roles.** `region` (name required), `group`, `toolbar`, `list` (built only when every member is already a list
  item in the published view, e.g. after the owner's own re-role facts), and the landmarks `banner`, `navigation`,
  `main`, `complementary`, `contentinfo`, `search`, `form`. Nothing interactive, no `dialog`, no `application`, no
  `document` (they would change modes and key handling).

### 2.3 Trust tiers

| Tier | Containers | Buttons (ADR-0028) |
|---|---|---|
| T2 first-party | built | built |
| T3-trusted | built, tainted (the name's provenance is tainted; `explain` names the add-on); landmark roles allowed | never (ADR-0028 VA-5) |
| T3 | **advisory only**: not built, shown in `explain` (structure cannot be hedged the way a value can, ADR-0026 PF-5) | never |

Grant: `structure { roles }` in the manifest, spoken on install / enable.

### 2.4 Presentation (no special cases)

A built container is an ordinary node of its role to every consumer:

- **Browse mode**: entering and leaving are announced like a real landmark, region, group, toolbar or list ("Chat
  header region" on entry; "leaving Chat header" / «выход из …» on exit), at the same verbosity as real ones.
- **Quick nav**: `D` / Shift+`D` (landmarks) and region navigation stop on landmark and named-region containers, `L`
  on lists; the elements list shows them under landmarks.
- **Object navigation**: the container is a parent level: parent → container → members; "parent" from a member
  reaches the container, "first child" from it reaches the first member (ADR-0006).
- **Where-am-I**: the ancestor chain includes the container.
- **Focus**: when focus lands on a member (the Search button), focus-ancestor speech enters the region exactly as for a
  real one ("Chat header region, Search, button"); leaving it is said when focus moves outside.
- **Actions menu** (ADR-0028 L4): a container has no native actions and cannot host offered actions (an action host
  must be an observed node); `gorca+Shift+A` on it says "No actions". Its members keep theirs.
- **Never said**: the word "virtual". `explain` (and only `explain`) says that a node is synthetic and who made it.
- **Verbosity (ADR-0028 §2.2.1 rule).** Containers add no new announcement kind: they are governed by the existing
  landmark / region / group / list report fields (M22d `report.landmarks`, `report.groups`, `report.lists`; until M22d
  lands, by the same code paths as real containers). Buttons are governed by `report.virtual_buttons`. One more report
  key, `report.synthetic_structure = true | false` (default true, global and per profile, toggle command
  `report.synthetic_structure_toggle`, unbound), switches all synthetic containers off at once for a user who prefers
  the page's own structure; it removes the level from the published view (dissolve-all, kept declarations) and never
  touches buttons or actions.

### 2.5 Cost

- Re-anchoring is incremental and local: only parents whose children changed and that carry a declaration are
  resolved (selector evaluation on that parent's children, native, O(c)), then `SyncChildren` on the parent and the
  container: O((c + moves) log n), counts maintained by the tree.
- The writer keeps `synth_by_parent: HashMap<DocNodeId, SmallVec<[SynthKey; 2]>>`, so a children change of an
  undeclared parent costs one hash miss.
- Targets (SM-1..SM-4 in §2.8).

### 2.6 WIT 0.11 (same single bump, WIT-11)

`structure` import, W lane, buffered like `facts`: `group(parent: fact-target, g: vgroup)`, `ungroup(parent, id)`;
`vgroup { id, role: synth-role, name: msg-ref, span }`; `span` = `range(first, last)` | `until(first, stop-selector)` |
`set(selector)`; `member-spec` = `node` | `first-child` | `first-match(sel)` | `last-match(sel)`. Manifest data form
`[[vgroup]]` with the same fields (PM-2: a K0 package can group without code). Native extensions use the host-side
`FactSink::group(parent, VGroup)`; one ABI.

### 2.7 First slice: Telegram chat header (SN-TG1)

```toml
[permissions]
grants = [..., "structure"]
[permissions.structure]
roles = ["region"]

[[selector]]          # end of the header run: the message pane
id = 20
class = "Transition"

[[vgroup]]            # "Chat header" / «Заголовок чата»
parent = { watch = 5 }                   # div.messages-layout
id = 1
role = "region"
name = "vgroup.chat_header"
span = { until = { first = "first-child", stop = 20 } }
```

`until(first-child, Transition)` groups everything before the message pane, whatever the avatar/status/button
classes are; the members are checked against a fresh live dump in the slice (the committed fixture keeps only the
title). The title keeps its level-1 heading fact; the "Messages" region (already a real node) is unaffected.

Slices (≤ 2 days each), after WIT-11: **SN-1** model, writer stage, `DocOp` translation, state machine, bounds,
`explain`, report key (behind `[plugins] synthetic = true`, default off); **SN-2** buttons of ADR-0028 VA-3 moved onto
SN-1; **SN-TG1** the header row + fixture test + one owner live run; **SN-3** app-actor (native) model, after M22b3.
Dependency: the category-feed fix (SN-5) must be merged before SN-1's quick-nav tests.

### 2.8 Metrics to validate

| Id | Metric | Target |
|---|---|---|
| SM-1 | re-anchor of a declared parent with 100 children after a re-render | p99 < 50 µs writer time |
| SM-2 | undeclared parent children change, overhead | < 100 ns (one hash miss) |
| SM-3 | quick nav `D` finds a synthetic landmark | in the same publish revision as its creation (0 stale misses over the Telegram soak) |
| SM-4 | adversarial set SN-A1..A8 (overlap, non-contiguous set, secret inside, T3 build attempt, > bounds, forged parent handle, interactive role, member removed mid-cursor) | 0 built where refused; cursor never lost |

## 3. Alternatives

| Alternative | Verdict |
|---|---|
| Overlay ranges consulted by each consumer (no tree level) | rejected: every surface (quick nav, object nav, where-am-I, focus ancestry, braille) would need its own range logic; the tree insertion reuses all of them |
| `aria-owns`-style re-parenting of arbitrary nodes | rejected: reorders content and lets a plugin move text away from where the page shows it |
| Re-role one existing node | impossible here (no single node covers the run) |
| Page modification (wrapper div through a browser extension) | rejected (owner: the DOM is the page's) |
| Separate mechanisms for containers and ADR-0028 buttons | rejected: two key spaces, two lifetime engines, two index paths |

## 4. Consequences

- Plugins can give flat layouts the structure a sighted user sees, for every navigation surface at once.
- The published tree may differ from the observed tree by inserted levels; every tool that compares them (`explain`,
  the cut-over diff, golden tests) must know `NodeKey::Synth`.
- New bounded writer work per declared parent; nothing for undeclared ones.

## 5. Invariants

- **SN-I1** Synthetic nodes exist only in the published model; never in the observed view, a selector result, a
  connector request or the page.
- **SN-I2** A container's members are contiguous siblings of one parent; the source order of all content is unchanged.
- **SN-I3** Nothing secret is inside a container.
- **SN-I4** A synthetic node vanishes with its owner's source state, its parent/host, or the scope.
- **SN-I5** Synthetic nodes reach every index through the same ops as real nodes; no consumer special-cases them.
- **SN-I6** T3 never builds structure; T3-trusted containers are tainted and attributed in `explain`.

## 6. Open questions

- **QS-1** Telegram header members: confirm with a live dump that nothing besides the header precedes
  `div.Transition` (pinned-message bar, audio player); if something does, use `range` with a `last-match`.
- **QS-2** Should `report.synthetic_structure` default on? **Decided by the owner 2026-10-26: default OFF** (users get the page's own structure unless they opt in); the owner's own build enables it.

## 7. Changelog

**r1 (2026-10-26)** — initial proposal.
