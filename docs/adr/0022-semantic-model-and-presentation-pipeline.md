# ADR-0022: Semantic model (roles, states, properties) and one presentation pipeline

Status: **Proposed** (draft, 2026-10-18). Owner: architect. Analysis, prior art, interface sketches:
`docs/reports/aria-semantics-architecture.md`. Coverage input: `docs/reports/aria-coverage-audit.md`
(web-a11y engineer). Relates to: ADR-0001 §4.2 (core role superset), ADR-0003 §13 (live regions) and
§21 (browse mode), ADR-0004 (coalescer), ADR-0008 §3.2 (engine quirk tables), ADR-0010 (catalog),
ADR-0016 (profiles, `ReportFlags`, planned), ADR-0019 (patches, `present` hooks, versioning V-3/V-4,
spike C-1). Milestones: P2-ARIA-0 (Phase 2 polish), M22a–M22d (Phase 3).

## 1. Context

The owner wants WAI-ARIA roles, states and properties presented at NVDA parity or better on the web,
and the same semantics for native GTK/Qt/LibreOffice where the platform exposes them, with the core
backend-agnostic (AT-SPI2 and Newton/AccessKit connectors).

Today (M8b) ARIA is carried as interned attribute strings (`web_attrs::XML_ROLES`, `CURRENT`,
`HASPOPUP`, `EXPLICIT_NAME`, ...) and re-parsed at presentation time; mutually exclusive states are bit
pairs (`EXPANDED|COLLAPSED`, `CHECKED|MIXED`). `aria.rs` shares the *words*, but four presenters
(`present_web`, `present_with`, `present_change`, `browse::present::{describe_from,
describe_node_from}`) plus ad-hoc leads in `browse/commands.rs` each decide which fields to say, for
which roles, in which order, and how names relate to content. Native presentation ignores the
`level`/`posinset`/`setsize`/`placeholder-text`/`keyshortcuts` GTK4 exports
(`refs/gtk/gtk/a11y/gtkatspicontext.c:540-650`). The open bug (role=button with `aria-label` and icon
content is skipped by browse arrows and its role is spoken twice on Tab) is a symptom: no flat-text
anchor for a content-less control, and two producers owning the role word in one utterance.

Prior art: NVDA has one property composer keyed by output reason (`speech/speech.py:2013
getPropertiesSpeech`, `:2288 getControlFieldSpeech`, `controlTypes/processAndLabelStates.py`) and
announces state changes for the focus and its ancestors from a per-object spoken-properties cache
(`NVDAObjects/__init__.py:1341`, `speech.py:770-790`). Orca duplicates generator logic between its
default and web scripts (`scripts/default.py:396,572` vs `scripts/web/script.py:1030,1247`) and calls
AT-SPI synchronously inside generators. AccessKit models tri-states as enums (`Toggled`, `Expanded`,
`Selected`, `Invalid`, `AriaCurrent`, `HasPopup`; `refs/accesskit/accesskit/src/lib.rs:465-605`).

## 2. Decision

### 2.1 Semantic model in `gorca-model::sem`

- `Semantics` per node: fine-grained `SemRole` (ARIA 1.2 vocabulary + native-only roles, append-only),
  tri-state enums `Check`, `Press`, `Expand`, `Select`, plus `Current`, `Invalid`, `Popup`, `Avail`,
  `NameFrom`, `SemFlags` (required, read-only, busy, modal, multiselectable, visited, has-details,
  active-descendant host, ...), `level`, `SetPos{pos,size,src}`, `GridPos`, `Live`, and one shared
  `Arc<SemText>` for rare strings (role description, error text, placeholder, shortcuts, value text,
  details target). Sketch: report §3.1.
- Carried by `NodeSnapshot`, `SrcNode` and the document mirror's `NodeMeta`. `AttrMap` remains for
  raw/unknown attributes, logs and extension selectors.
- Invariants **SM-1..SM-7** (report §3.2): coarse `Role` derived from `SemRole`; exclusive states
  unrepresentable; presentation never parses strings; one normaliser per connector family;
  `Disabled` only on disableable roles; positions never computed by IPC on the presentation path;
  `Semantics` immutable per revision (`sem_rev`).

### 2.2 Normalisation in connectors

- AT-SPI: new pure crate `gorca-sem-atspi` (depends on `gorca-model` only), used by both
  `gorca-connector-atspi` and `gorca-doc-source-atspi`; table-driven per Core-AAM/HTML-AAM and
  toolkit rows (GTK4 now; Qt, LibreOffice as data rows later).
- Newton/AccessKit (M19): 1:1 mapping from `accesskit::Node`; a shared fixture set runs both
  normalisers.
- Engine quirks stay in ADR-0008 tables, applied before normalisation as new closed `QuirkAction`
  variants. Consumers of `Semantics` never learn the engine.

### 2.3 One presentation pipeline in a new pure crate `gorca-present`

`Facts → compose(Reason, Ctx, ReportFlags) → PresIr (typed fields) → dedupe → render (speech now,
braille M10, log/test transcript)`. `Reason` = Focus, FocusAncestor, BrowseLine{entered}, QuickNav,
SayAll, Review, Change(SemDiff), ListEntry, WhereAmI. Field selection and order come from one rule
table indexed by (role class, reason); per-language order overrides live in the catalog. Words are
catalog ids; render is the only producer of text. Invariants:

- **PR-1** At most one `Role` and one `Name` field per node per utterance, across all producers of
  one output event.
- **PR-2** Container enter/leave fields come only from the context tracker (§2.5).
- **PR-3** Same facts and reason give the same fields on every surface; allowed differences are
  rows of the rule table.
- **MI-1** Every non-hidden atomic presentable node owns at least one flat-text scalar; the
  `gorca-doc` flat builder synthesises a U+FFFC anchor when its subtree has no visible text.

### 2.4 State changes (ST-1..ST-6)

Spoken for: the focus (ST-1); focus ancestors up to 8 levels for expand/select/check/press/sort/busy
(ST-2, NVDA #10890); the active descendant (ST-3); a node gorca activated in the last 2 s (ST-4);
optionally the browse cursor node (ST-5, `report.state_changes_at_cursor`, default off); never other
nodes (ST-6). Diff against the last *spoken* `Semantics` per channel; user-caused changes are spoken
at the first reply and refined (not queued) for 250 ms; `Prio::Normal`, cancel key `(node, Change)`;
per-app limit 8 change utterances/s with per-node coalescing. `aria-busy` suppresses change and
live-region speech inside the subtree until it clears (bounded at 10 s); live regions keep ADR-0003 §13.

### 2.5 Container context

One `ContextTracker` per channel (focus, browse cursor, review) diffs the chain of context-bearing
ancestors and emits `ContainerLeave` (browse only) and `ContainerEnter` with name, kind, item count or
table dimensions, from mirror data only. It replaces NVDA-style focus-difference logic, the browse
`leaving_items`/`container_items`, and the dialog/list-box leads.

### 2.6 Verbosity

`ReportFlags` (ADR-0016) gate fields in `compose`; new bits: position info, levels, landmarks,
container kinds, role description, details, current, state changes (+ at cursor), negative states.
`verbosity.level` = brief/normal/verbose presets (report §5.5). Vendor profiles may set them.

### 2.7 Extensions (ADR-0019)

Node patches are typed edits of `Semantics` + name, applied by the core at mirror/snapshot commit
with provenance. Inline `present` hooks receive `PresIr`, may add/drop/reorder fields within the
3 ms aggregate cap, fail-open to the core IR. The WIT 0.9 node record carries `Semantics` as open
codes (V-3); hence **M22a precedes the M18a WIT freeze**.

### 2.8 Phasing

- **P2-ARIA-0 (Phase 2 polish):** failing Firefox/Chromium tests for icon-only buttons; MI-1;
  PR-1 dedupe across focus utterance and browse tail/leads; cross-surface consistency test (M-1)
  over the M8 fixtures. No new crates.
- **M22a** model + normalisers (before M18a). **M22b** pipeline + migration, deleting the duplicate
  presenters (before M10, M15, M21 consume it). **M22c** ST rules + context tracker. **M22d**
  verbosity integration (after M14a) + native parity rows.

## 3. Alternatives considered

- **B. Typed accessors over the existing attribute map, shared helper functions.** Cheapest; keeps
  string parsing on the hot path, the bit-pair ambiguity and per-presenter field ownership (the bug
  class remains). Rejected.
- **C. Generic property bag (`PropId → PropValue`).** Most extensible; loses exhaustive matching,
  more memory and slower compose. Kept as the WIT-side representation (open codes), not the core one.
- **D. Orca-style per-role generator classes.** Leads to duplicated per-script logic; rejected.
- **E. Fully declarative presentation grammar.** Adopted only for field order (localisation, vendor
  profiles); logic stays in Rust.

A vs C for the core representation is settled by a 2-day micro-benchmark (compose cost, memory on the
100k-node synthetic page), judged by the reviewer on M-4/M-5/M-9; no full bake-off.

## 4. Consequences

- One place to change wording, order and dedupe; braille, elements list and review reuse it.
- Native apps gain position, level, placeholder and shortcut reporting where toolkits export them.
- Cross-crate migration: `gorca-model`, both AT-SPI crates, `gorca-doc`, `gorca-core` presenters;
  NVDA-parity tests in `aria.rs` move to `gorca-present`.
- New ordering constraint between M22a and M18a.
- The app actor routes property changes for focus ancestors and the active descendant (bounded set).
- Slight memory increase (≤ 64 B per node inline).

## 5. Metrics to validate

M-1 cross-surface consistency: 0 unexplained differences. M-2 NVDA parity over ≈30 APG patterns:
≥ 95 % token agreement on P0. M-3 GTK4 native parity fixture. M-4 compose p99 ≤ 3 µs/node.
M-5 browse line p99 ≤ 60 µs on 100k nodes, no regression. M-6 focus keypress → speech Δp99 ≤ 0.1 ms.
M-7 10k non-focus state events/s → 0 utterances, no reader lag regression. M-8 check box toggle →
speech ≤ 10 ms gorca-side, ≤ 1 utterance per 250 ms flip-flop. M-9 ≤ 64 B/node inline, ≤ 10 MB on
100k nodes. M-10 every ARIA 1.2 role/state in the coverage audit mapped or listed as unmapped.
Details: report §8.

## 6. Open questions for the owner

- **QA-1** Announce state changes after a browse-mode activation (ST-4, proposed on) and at the
  browse cursor (ST-5, proposed off)?
- **QA-2** Verbosity presets as in report §5.5?
- **QA-3** Honour `aria-roledescription` by default (NVDA does)?
