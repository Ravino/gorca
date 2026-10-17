# ADR-0022 adversarial design review (semantic model and presentation pipeline)

Date: 2026-10-18. Role: reviewer. Read-only design review of `main` at `e1bcb019`.
Inputs: `docs/adr/0022-semantic-model-and-presentation-pipeline.md` (draft),
`docs/reports/aria-semantics-architecture.md`, `docs/reports/aria-coverage-audit.md`,
`docs/reports/aria-test-coverage-math.md`, ADR-0019, ADR-0021, ADR-0018, ADR-0008, CLAUDE.md
(core-as-API), and the current code they describe.

**Verdict: ACCEPT-WITH-CHANGES.** The direction is right: a typed `Semantics` value, exhaustive
tri-state enums, one composer keyed by `Reason`, a rule table shared with the test oracle. But the
ADR gives one *presentation* point, not one *fusion* point. Semantics are still merged in at least
four places with no written precedence, and several existing producers are missing from the
inventory. P2-ARIA-0 can start now. M22a should not start until MC-1..MC-8 below are in the ADR.

---

## 0. What I verified by running or reading code

| Claim | Check | Result |
|---|---|---|
| "≤ 64 B inline per node" (M-9) | Compiled the §3.1 sketch with `rustc -O` (scratch `semsize/main.rs`) | Without `provenance`: **64 B**. With `provenance: Connector \| Quirk(&'static str) \| Patch(Arc<str>)` as sketched: **88 B**. With a compact `u16/u32` provenance: 72 B. **As written, the sketch breaks M-9.** |
| "Engine quirks stay in ADR-0008 tables" | `grep QuirkAction\|EngineProfile\|QuirkRule crates/` | **No such table exists.** Quirks are still `is_gecko`/`is_blink` booleans (`gorca-doc-source-atspi/src/factory.rs:80-84`), and some live in core code (below). |
| "Four presenters" (§1, report §1.1) | `grep role.spoken()` in `gorca-core` | At least **5 more role-word producers**: `present/notification.rs:115`, `present/window.rs:160`, `commands/info.rs:130-231` (where-am-I/title), `review/commands.rs:696`, `review/cluster.rs:132`. Also `doc_actor/live.rs` and `doc_actor/translate.rs` read raw `web_attrs`. None of these are in the §4.6 deletion list. |
| Two AT-SPI normalisers today | `connector-atspi/src/decode.rs:77` maps INDETERMINATE→MIXED; `doc-source-atspi/src/roles.rs::map_states` does not | Confirmed (this is audit G1). SM-4 targets the right defect. |
| Focus fetch carries ARIA attrs | `present/flow.rs:117-120` `FOCUS_FIELDS = PRESENT − DESCRIPTION`; `PropMask::PRESENT` has no `ATTRS`/`RELATIONS`; review `objnav.rs:17` `NAV_MASK = PRESENT\|CHILDREN\|PARENT` | **No.** On the app connector path, `Semantics` would be built without `xml-roles`, `level`, `posinset`, `setsize`, `placeholder-text`. On the web, the attrs come from the core-side `web_overlay`. |
| Baseline unit tests | `cargo test -p gorca-core --lib -- aria browse::present` | 24 passed (758 lib tests in total). `browse/tests.rs` has 72 exact-string `assert_eq!`, so it pins today's wording, not invariants. |

---

## 1. One fusion point? Source → where it enters → where it is normalised

| # | Source | Enters (ADR-0022) | Normalised / fused | Side channel that remains |
|---|---|---|---|---|
| S1 | AT-SPI, app connector (focus, native, review) | `gorca-sem-atspi` in `gorca-connector-atspi` → `NodeSnapshot.sem` | in the connector | Built from a **partial** snapshot (no ATTRS on the focus or review mask): a different `SemRole` (ToggleButton vs Switch) and no position or level for the same node |
| S2 | AT-SPI, doc source (web) | `gorca-sem-atspi` in `gorca-doc-source-atspi` → `SrcNode` → `NodeMeta` | in the connector, then `doc_actor/translate.rs` | A second carrier for the same web node, at a different revision |
| S3 | Engine quirks | "before normaliser, `QuirkAction`" | doc source only (the table does not exist) | The app connector has no quirk stage, so S1 and S2 diverge for web focus nodes |
| S4 | **Core-side web fusion**: `browse/commands.rs:1157-1219 web_overlay` | **not mentioned** | `gorca-core` | Field-level precedence lives in core code: name filled from mirror subtree text (`subtree_label`, glyph/U+FFFC filter), list/tree-item name replaced from the mirror ("web source quirk row", which is engine knowledge in core and breaks WE-1), fetched `error-text` wins over the mirror's, mirror description as fallback, `READ_ONLY\|HAS_POPUP` OR-merged. This is the real fusion point today, and the ADR neither replaces nor names it. |
| S5 | Name computation | Outside `Semantics` ("Semantics + name") | spread across engine, S4, browse `takes_authored_name`, patch `name-from = label-by\|sibling-prev\|child-text` (ADR-0019 report §273) | No single name stage. AccessKit/Newton needs **consumer-side** name computation (`accesskit_consumer/src/node.rs:744-760` `label`/`labelled_by`) and has no `NameFrom`, so the Newton connector must implement accname and derive `NameFrom`. Not stated. |
| S6 | AccessKit / Newton | connector, "1:1" | connector | Not 1:1: `Toggled` maps to `Check` or `Press` depending on role. AccessKit has 180 roles; the sketch lacks Comment, Suggestion, Details, DisclosureTriangle, the input types, Time, Iframe, DescriptionList, etc. Comment and suggestion are in the audit (G3), so M-10 fails. Precedence when one app is visible through both connectors (phase-3 N2 "spoken once") is undefined. |
| S7 | Add-on node patches | "typed edits at mirror/snapshot commit" | **two** commit sites (doc actor, app mirror) | `hide`, `flatten`, `claim`, `sleep`, `category` are not `Semantics` edits. `category = name` **contradicts SM-1** ("Cat is a pure function of Semantics"). There is no order between two add-ons patching the same field. Selectors such as `has-descendant(...)` (the report's own example) need upward invalidation, which is not costed against XP-8. |
| S8 | Add-on `present` hook | receives `PresIr` | after compose | WIT 0.2 `present` returns `list<segment>` and replaces everything (`gorca-ext.wit:145-146`). There is no IR in WIT yet, so `Field` must become open codes as well. |
| S9 | Add-on `policy` / `on-event` / `host.speak` / `command` / site `describe` | **not mapped** | — | Live text side channels: `present/flow.rs:405-420` (`PolicyVerdict::Value/Say`), `present/notification.rs:350`, `app_actor.rs:1067` (`ExtVerdict::Rewrite`), `host.speak`, `command-result::speak`, ADR-0019 site `describe`. They bypass PR-1 dedupe, braille render and the catalog. `ext-gnome-shell` uses `Value`/`Say` today. |
| S10 | Browse mirror derived facts | MI-1 anchor in "`gorca-doc` flat builder" | today `doc_actor/translate.rs::markers` | Fine as a derived stage, but it is listed as living in the wrong crate, and it must collapse the icon child's marker (see MC-8) |
| S11 | User profile / verbosity | `ReportFlags` in compose | compose | Good. User or site **semantic** overrides (data add-ons, vendor profiles) have no precedence relative to S7. ADR-0016 `ReportFlags` is not written yet, so M22b needs an interim type. |
| S12 | Terminal (ADR-0018), editor echo, live text, ADR-0021 virtual copy | — | — | These are *content* streams and should stay outside PR rules, but the ADR must say so explicitly. ADR-0021 "copy what the user heard" must come from flat text, never from `render_speech` output (role words would land in the clipboard). |

**Conclusion for Q1:** the ADR unifies *compose/render*. *Fusion* still happens in S1, S2, S4 and S7 (two sites), S9 is untouched, and S5 is scattered. The owner's model ("fused at one point into one representation") requires an explicit fusion stage with layer order (MC-1).

---

## 2. Core invariants

- **Backend-agnosticism.** `gorca-sem-atspi → gorca-model` only, so the core never sees AT-SPI types. Good. For Newton the gaps are S5 (name computation) and S6 (role coverage, Toggled split, dual-connector precedence). Feeding the model from Newton needs no *core* change, but it needs more connector work than "1:1" suggests.
- **Never block.** Compose and render are pure. Good. Native parity, however, requires **ATTRS on the focus fetch** (GTK4 exports posinset/setsize/level/placeholder as attributes), which is one more D-Bus call on the cold focus path that B-1 deliberately kept out (`flow.rs:117` comment). M-6 measures only the *gorca-side* Δp99, so the metric hides this regression. An end-to-end cold-focus metric is needed, or attrs must be fetched lazily with `may_refine`.
- **Partial data.** `NodeSnapshot.have` is a `PropMask`, and `CACHE_ITEM` carries no ATTRS. The `Semantics` tri-states cannot express "not fetched": `Check::NotCheckable` and "unknown" are conflated. Failure: a review or object-nav node built from `NAV_MASK` composes as "toggle button" while the focus or browse path says "switch, on". PR-3 and M-1 are violated by construction.
- **Cross-node derived fields break SM-7.** `error_text` (target text copied at normalisation), names from `labelled-by`, `PosSrc::Computed` (from siblings), container item counts and descendant selectors all change when *another* node changes, without bumping this node's `sem_rev`. Failure: inserting an option makes the cached BrowseLine field "3 of 5" stale, because the cache key `(node, sem_rev, name_rev, report_epoch, lang_epoch)` has no dependency term. The existing `web_overlay` "fetched error text is fresher" hack is exactly this problem today.
- **Storms.** ST-6 costs a mirror write per event, plus re-normalisation per upsert, which is cheap. Un-costed are patch re-evaluation for descendant selectors and a full re-patch of a 100k-node mirror when an add-on is enabled, disabled or swapped (ADR-0019 staged update). The ADR should say which lane does that work and how it is bounded.
- **Memory.** 88 B as sketched (measured). A web node has two or three carriers (`SrcNode`, `NodeMeta`, `NodeSnapshot`), so the real 100k-node increase can exceed the M-9 10 MB gate. M-9 must count every carrier.
- **Exhaustive enums vs extensibility.** A closed `SemRole` in the core and open codes at WIT is the right split (V-3). Two notes: `#[repr(u8)]` has headroom, but adopting AccessKit's DPUB and input roles approaches about 230 values, so reserve `u16` at the WIT side. And `Field` in `PresIr` needs the same open-code treatment, because `present` hooks receive it.

---

## 3. Provenance and conflicts

| Conflict | Rule in ADR | Assessment |
|---|---|---|
| Exclusive states (expanded+collapsed, checked+mixed) | SM-2: Mixed > On > Off; Expanded wins | Defined, deterministic, testable |
| ARIA token vs native raw role (FF menuitem as role 33; `role=button` on `<a href>`; fallback lists `"switch checkbox"`; invalid tokens) | "table-driven `(raw_role, token)`" | No rule for multi-token fallback, invalid tokens, or when the engine-computed role wins. Must be stated and table-tested (math P9). |
| Engine quirk vs add-on patch vs page | — | **Undefined** |
| Two add-ons patching one field | — | **Undefined**. ADR-0019 orders *hooks* (phase + before/after), not patches. |
| User or site override vs add-on | — | **Undefined** |
| App snapshot vs doc mirror for the same web node | implicit in `web_overlay` | **Undefined in the ADR** (S4) |
| `present` hook adds a second Role field | "fail-open to core IR" | PR-1 must be re-checked *after* hooks. The §4.1 diagram puts hooks beside dedupe, not before it. |
| `roledescription` hostile | cap 64, ignore empty | The audit §3.5 safeguard ("keep the native role in the info/spell key") is not adopted |

On provenance, the ADR has one inline `Provenance` per node: one value, debug-only, and inside the struct. It cannot answer "why did gorca say 'switch'?" when a quirk set the role and a patch renamed the node. It also pollutes `SemDiff` equality, so a provenance-only change could look like a semantic change, and it costs 24 B (measured). Hook budget skips make output depend on load (math P10) and are not recorded anywhere.

---

## 4. Migration risk

- M22b ("migrate focus/browse/quick-nav/say-all/change, delete §4.6") is one slice, which makes it a big-bang. The guards are 24 aria/browse-present unit tests plus 72 exact-string asserts in `browse/tests.rs`, 14 in `utterance.rs`, and the M8/M8b e2e suites (live browsers, not per-commit). String tests get rewritten together with the code, so they guard nothing during the switch.
- What exists and helps: math tier 1 (fake-tree 3-wise covering array, properties P0–P10) and P2-ARIA-0's M-1 cross-surface test. Neither is wired to the migration as a *gate* yet. Tier 1 needs `Segment` to carry a `MsgId` (the math report §3), and that hook is not in the ADR.
- Incremental path that works: dual-carry `Semantics` next to `AttrMap` with a debug assertion that legacy reads agree. Then run `gorca-present` in **shadow mode** beside each legacy presenter and diff token multisets over the M8 fixtures and the tier-1 array, with a `deviations.toml` as the only allowed difference. Switch one surface at a time (change → focus → browse/quick-nav → review → window/notification/info) and delete the legacy path only when its shadow diff is zero.

## 5. Order with M18a, M21, M15, M10

- **M18a.** The owner's order (decisions 2026-10-17) is "Phase 3 starts with M18a". ADR-0022 makes M22a a hard predecessor and does not acknowledge the conflict. The constraint is also self-inflicted: ADR-0019 **V-4** says growing inputs go through accessors and records are frozen snapshots, and `Semantics` is the textbook growing input. Carry it through accessors and an open code registry (`sem-role: u16`, `prop(code) -> option<prop-value>`). Then only the **code registry** has to exist before 0.9, not the Rust struct, and M18a is unblocked. Resolve C-1 (record vs resource) first, because it changes the answer.
- **M21 review.** M21 promises "feature records" and stable identity for graph seams (phase-3-features §H). Those records should be `Semantics`. Review today uses `NAV_MASK` (no attrs), which hits the partial-semantics problem above. `review/cluster.rs` and `review/commands.rs` must be on the migration list.
- **M15 elements list.** `ListEntry` over 10k nodes at 3 µs/node is 30 ms, which breaks W4's 15 ms gorca-side target if composed eagerly. Visible rows must be composed lazily.
- **M10 braille.** `PresIr` fields carry no source span (node plus flat-text range), so routing keys (B2) cannot map cells back to objects or offsets. Add an origin span per field before M10 consumes the IR.

---

## 6. Must-change list (before acceptance)

1. **MC-1 One explicit fusion stage.** Define `fuse() → EffectiveSemantics` with a written layer order: connector-normalised → engine quirk (connector) → cross-carrier merge (doc mirror authoritative for web nodes; app snapshot contributes only fresher fields under a revision rule) → add-on patches (deterministic order: declared priority, before/after, then add-on id; last writer per field) → user/site overrides → derived facts (Cat, anchors, positions). Replace and delete `web_overlay`/`subtree_label` (move the list-marker quirk out of core). Add the five missing presenters plus `live.rs` to the migration list.
2. **MC-2 Partial semantics.** Add a `known` mask (or `Option` groups) tied to `PropMask`. Compose omits unknown fields and tags `may_refine`. Say what the focus fetch adds (ATTRS cost) and add an end-to-end cold-focus metric next to M-6.
3. **MC-3 Cross-node derived fields.** Either compute them at compose time from the mirror (pure, not cached), or define dependency edges and invalidation. Fix the cache key accordingly.
4. **MC-4 Provenance.** Per field, out of line: a ring or side table keyed by `(node, sem_rev)`. Each `PresIr` field gets an origin (rule row, quirk id, patch id, hook id, budget-skip). Add a "why was this said" log or command. Remove `provenance` from `Semantics` so it fits 64 B.
5. **MC-5 Extension side channels.** Map every WIT path that emits text: `present`, `policy` value/say/silence, `on-event` rewrite, `host.speak`, `command` speak, site `describe`. Each is either an IR edit (pipeline order compose → hooks → dedupe/validate PR-1 → render) or a declared *content* utterance outside the PR rules.
6. **MC-6 ADR-0019 alignment.** Handle the non-`Semantics` patches (hide, flatten, claim, sleep). Resolve the `category` patch vs SM-1 conflict. Carry `Semantics` through V-4 accessors and open codes so the M22a→M18a hard dependency drops to "code registry before 0.9". Reconcile this with the owner's M18a-first order in `decisions.md`.
7. **MC-7 Migration gate.** Strangler with shadow-mode token diff, per-surface switch, `MsgId` on `Segment` in M22a, and math tier-1 properties P0–P10 as the acceptance gate. Split M22b per surface.
8. **MC-8 Housekeeping and correctness.**
   - Record the QA-1..3 owner answers (still listed as open).
   - Reconcile the position-info policy: the audit says not in browse lines and once per group; the rule table says always on focus.
   - MI-1 must *collapse* the icon child's U+FFFC into the control's single anchor, not add one more, and must cover the audit's top-ranked inline-run case (several markers on one line, word/char moves).
   - P2-ARIA-0 implementation waits on the BT1 repro.
   - Add the missing roles: comment, suggestion, details/summary.
   - Correct "Newton 1:1".

## 7. Should-change

- Field origin spans for braille routing (M10).
- Lazy `ListEntry` compose (M15).
- Native `ContextTracker` when ancestors are not cached: omit, then refine; never IPC.
- Interim `ReportFlags` type until ADR-0016.
- Make M-9 count every carrier.
- Log hook budget skips (P10 nondeterminism).
- Precedence rule for one app visible through both connectors (N2).
- Adopt the audit §3.5 rule that `roledescription` never hides the native role from the info key.

## 8. Not verified

- No benchmark run. The M-4/M-5 numbers are design targets; the A-vs-C micro-bench is still to do.
- The S9 inventory comes from `grep`. There may be more text-producing paths in `ext-vte`'s host actions.
- `SrcNode` lifetime (transient or retained) was not checked, so "two or three carriers" is an upper bound.
