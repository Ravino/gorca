# ADR-0022: Semantic model (roles, states, properties), one fusion stage and one presentation pipeline

Status: **Proposed** (draft r2, 2026-10-18; r3 note 2026-10-19; r4 note 2026-10-24). Owner: architect.

**r4 (2026-10-24, owner rule: no legacy tails):** new section 8, "Cut-over and no-legacy rule".
Shadow windows have expiry dates and exit gates; the legacy path of a surface is deleted in the
slice that switches it, with no flag; pre-fusion fallbacks become S5 provisional facts in the
model; xtask rules 12 and 13 enforce a shrink-only dated allowlist; extensions never see legacy
data. Section 2.11 is read together with section 8.

**r3 (2026-10-19, after the ADR-0024 review):** the source contract is opened to future fact
sources (browser add-on bus, extensions, local inference) by ADR-0024
(`docs/adr/0024-fact-sources-contract.md`). Amendments to this ADR:
(1) §2.2: the source-slot list is open (descriptor + core-computed grant, scope pinned by the
transport); a new stratum **S4 inferred** sits below S3 derived (user > patch > observed > derived >
inferred > default); a secondary source is an S0 candidate only for groups a static per-source-profile
row declares inexpressible for the primary, never because a primary value is not fetched yet; values
derived from untrusted or inferred inputs are tainted and hedged; confidence gates admission, sets a
spoken hedge and orders candidates only inside S4 by class (alternative G stands for cross-source
ranking). (2) §2.1, layout x32 (owner 2026-10-19: extensibility beats per-entry memory): 32 group slots, 18
registered (the 16 of r2 plus `orientation` and `sort`; `existence` and `text` are not fused),
`description` moves into `Semantics`' `Arc<SemText>`, `u32` masks, `ProvTag` `[u64; 2]`,
`hedge: u64`, `taint: u64`; `SemEntry` measures 128 B (r2 was 120 B); gates `SemEntry` ≤ 160 B,
`Semantics` ≤ 80 B, and M-9's 100k-node RSS ≤ 25 MB as the real ceiling (the 112 B entry gate is
retired). Name from content uses untainted children first. (3) `PI[app profile]` is `PI[source profile]` (engine/toolkit class, not an
ADR-0016 user profile). (4) The AT-SPI normaliser runs inside the AT-SPI crates; `SrcNode` carries a
normalised `SrcRecord`. Everything else in r2 is unchanged. M22a plan: `docs/plan/m22a-spec.md`.

- Analysis, prior art, interface sketches: `docs/reports/aria-semantics-architecture.md`.
- Coverage input: `docs/reports/aria-coverage-audit.md` (web-a11y engineer).
- Review: `docs/reports/adr-0022-review.md` (MC-1..MC-8 are integrated here, §7 maps them).
- Fusion model and proofs: `docs/reports/semantic-fusion-math.md` (amendments A1–A8, properties
  F1–F10, bench B-FUSE). Test plan: `docs/reports/aria-test-coverage-math.md` (P0–P10).
- Relates to: ADR-0001 §4.2 (core role superset), ADR-0003 §13 (live regions) and §21 (browse
  mode), ADR-0004 (coalescer), ADR-0008 §3.2 (engine quirk tables), ADR-0010 (catalog), ADR-0016
  (profiles, `ReportFlags`, planned), ADR-0019 (patches, hooks, versioning V-3/V-4, spike C-1),
  ADR-0021 (copy).
- Milestones: P2-ARIA-0 (Phase 2 polish), M22a, M22b1–b4, M22c, M22d (Phase 3,
  `docs/plan/phase-3.md` §M22).

r2 changes: an explicit fusion stage (§2.2) replaces "normalise in the connector and merge
wherever"; provenance leaves the struct (§2.6); partial data is three-valued (§2.3); cross-node
values have invalidation rules (§2.4); every extension text path is routed (§2.9); order fixed by
the owner as M22a → M18a, with add-ons reading semantics through accessors (§2.10); migration
is a strangler with a shadow diff gate (§2.11); QA-1..3 answered.

## 1. Context

The owner wants WAI-ARIA roles, states and properties presented at NVDA parity or better on the web,
and the same semantics for native GTK/Qt/LibreOffice where the platform exposes them. The core
stays backend-agnostic (AT-SPI2 and Newton/AccessKit connectors). The owner's model: *all sources
are fused at one point into one representation, and one pipeline presents it.*

Today (M8b) ARIA is carried as interned attribute strings (`web_attrs::XML_ROLES`, `CURRENT`,
`HASPOPUP`, `EXPLICIT_NAME`, ...) and re-parsed at presentation time. Mutually exclusive states are
bit pairs (`EXPANDED|COLLAPSED`, `CHECKED|MIXED`). Two AT-SPI normalisers disagree
(`gorca-connector-atspi/src/decode.rs:77` maps INDETERMINATE to MIXED,
`gorca-doc-source-atspi/src/roles.rs::map_states` does not). Engine quirks are `is_gecko`/`is_blink`
booleans (`gorca-doc-source-atspi/src/factory.rs:80-84`), not the ADR-0008 table, and one quirk
row lives in core code.

### 1.1 Inventory of semantic merge points and text producers (verified 2026-10-18, `main` at `32a5b29d`)

Merge points (where semantics from several sources are combined today):

| # | Site | What it merges |
|---|---|---|
| X1 | `gorca-core/src/browse/commands.rs:1157 web_overlay` (called from `present/flow.rs:798 spoken_snapshot`) | **The real fusion point today.** App-connector focus snapshot + doc-mirror `NodeMeta`: name filled from mirror subtree text (`subtree_label`, `commands.rs:613`, glyph/U+FFFC filtered); list/tree-item name replaced by the mirror's (Firefox list-marker quirk, engine knowledge in core); fetched `error-text` wins over the mirror's; mirror description as fallback; `READ_ONLY|HAS_POPUP` OR-merged |
| X2 | `gorca-core/src/doc_actor/translate.rs` (`:193-272`, `markers` `:1031`) | Raw `web_attrs` → mirror role refinements, layout-table guess, live, level, description, U+FFFC markers |
| X3 | `gorca-core/src/doc_actor/live.rs:90-125` | Raw `xml-roles`/`live`/`relevant`/`atomic`/`busy` strings → live-region config |
| X4 | `gorca-connector-atspi/src/decode.rs`, `gorca-doc-source-atspi/src/roles.rs` | Two independent AT-SPI state/role normalisers |

Role-word and utterance producers (each decides fields and order on its own):

| # | Site | Surface |
|---|---|---|
| P1 | `present/utterance.rs` `present_focus`/`present_selected_item` → `present_web` (`:84`), `present_with` (`:147`), `present_change` (`:334`), `STATE_WORDS`, `mixed_word` | focus, selection, changes |
| P2 | `browse/present.rs` `describe`, `describe_from`, `describe_node(_from)`, `announced`, `role_phrase`, `push_states`, `container_items`, `enter_items`, `marker_items`, `leaving_items`, `takes_authored_name`, `authored_name_items`, `is_named_control`, `dedupe_names` | browse line, quick nav, say-all |
| P3 | `browse/commands.rs` `web_document_lead` (`:1368`), `web_dialog_lead` (`:1392`), `web_listbox_lead` (`:1451`), `pending_tail` | browse leads appended to focus |
| P4 | `present/flow.rs:145 apply_web_extra` | splices P2/P3 items into a P1 utterance (second role producer in one utterance, bug H2) |
| P5 | `present/notification.rs:115` | notifications |
| P6 | `present/window.rs:160` | window/dialog titles |
| P7 | `commands/info.rs:130-231` | where-am-I, title, object info |
| P8 | `review/commands.rs:696` | review / object nav "no name" |
| P9 | `review/cluster.rs:132` | review clusters |
| P10 | `aria.rs` (`role_word`, `:171-434`) | shared word helpers (NVDA tests) |
| P11 | Extension paths: `present/flow.rs:405-420` (`PolicyVerdict::Value/Say`), `present/notification.rs:350`, `app_actor.rs:1067` (`ExtVerdict::Rewrite`), WIT `host.speak`, `command-result::speak`, `present` hook; `gorca-daemon/src/shell_policy.rs:75` maps ext-gnome-shell `Role` segments | extension text |

All of X1–X4 and P1–P11 are migrated or deleted by M22 (§2.11). The review's count of "five extra
role-word producers" (P5–P9) is confirmed; P4 and the `pending_tail` coupling are added.

Prior art: NVDA has one property composer keyed by output reason (`speech/speech.py:2013
getPropertiesSpeech`, `:2288 getControlFieldSpeech`, `controlTypes/processAndLabelStates.py`) and
announces state changes for the focus and its ancestors from a per-object spoken-properties cache
(`NVDAObjects/__init__.py:1341`, `speech.py:770-790`). Orca duplicates generator logic between its
default and web scripts (`scripts/default.py:396,572` vs `scripts/web/script.py:1030,1247`) and calls
AT-SPI synchronously inside generators. AccessKit models tri-states as enums
(`refs/accesskit/accesskit/src/lib.rs:465-605`) but computes names consumer-side
(`accesskit_consumer/src/node.rs:744-760`) and has no name-source field.

## 2. Decision

### 2.1 Semantic model in `gorca-model::sem`

- `Semantics` per node: fine-grained `SemRole` (ARIA 1.2 vocabulary + DPUB subset + native-only
  roles, append-only, `#[repr(u16)]`-compatible codes), tri-state enums `Check`, `Press`, `Expand`,
  `Select`, plus `Current`, `Invalid`, `Popup`, `Avail`, `NameFrom`, `SemFlags`, `level`,
  `SetPos{pos,size,src}`, `GridPos`, `Live`, `Orientation`, `Sort`, and one shared `Arc<SemText>`
  for rare strings. Sketch: report §3.1, **without** the `provenance` field (§2.6).
- Roles added in r2 (audit G3/G11/G13): `Comment`, `Suggestion`, `Details`, `Summary` (disclosure
  trigger), `DescriptionList`, `Time`, `Iframe`. M-10 gates completeness.
- Name and description are fused groups too (§2.2), stored beside `Semantics` in the fused entry.
- **Fused entry** (one per node, owned by the node's writer, §2.2):

```rust
pub struct SemEntry {
    pub sem: Semantics,               // Eq compares values only; ≤ 64 B (measured 64 B, review §0)
    pub name: Option<Arc<str>>,
    pub description: Option<Arc<str>>,
    pub known: GroupMask,             // u32: bit set = group known (value or Absent), clear = Unknown
    pub prov: ProvTag,                // u64: 4 bits per group (winning layer, source slot), §2.6
    pub stale: GroupMask, pub fallback: GroupMask,
    pub sem_rev: u32,                 // bumps iff (sem, name, description, known) changed (A5)
}
```

  `NodeSnapshot`, the doc mirror's `NodeMeta` and review/object-nav views hold `Arc<SemEntry>`
  (8 B), not copies. `AttrMap` stays for raw/unknown attributes, logs and extension selectors.
- Invariants **SM-1..SM-8** (report §3.2, amended):
  - SM-1 coarse `Role == sem.role.coarse()`; built-in `Cat` is a pure function of the effective
    `Semantics`. Add-on categories are a separate set (§2.9).
  - SM-2 exclusive states unrepresentable; canonicalised by the normaliser for its own record
    **and** re-established by repair `R` after patches and overrides (A3).
  - SM-3 presentation never parses strings.
  - SM-4 one normaliser per connector family, shared by every consumer of that family.
  - SM-5 `Avail::Disabled` only on disableable roles (in `R`).
  - SM-6 no IPC on the presentation path for positions; `PosSrc::Computed` comes from S3 (§2.2).
  - SM-7 an entry is immutable per `sem_rev`; `sem_rev` bumps iff the effective value changes.
  - **SM-8 (new)** Effective semantics are produced only by `FUSE`, run by the node's single
    writer. Nothing else (presenter, extension host, review) merges sources.

### 2.2 One fusion stage: registers + `FUSE` in fixed stratum order (MC-1, A2, A3)

**Do not merge events; merge state.** Connectors and the doc source deliver normalised *source
records* (`SrcRecord`: per-group value or `Unknown`, per-group source sequence), never effective
semantics.

- **Registers.** One register per (node, source slot, group). A fact replaces the register value iff
  its per-source sequence is newer (the actor timeline: ingress order for events, issue order for
  fetches, i.e. the mirror's `issued_at`, LR-1..LR-3). Across sources there is no time order, ever.
  A no-change join stops processing (free dedupe).
- **Source slots.** `atspi` (all AT-SPI reads of the node, whichever path issued them: doc-source
  walk, focus fetch, review fetch; same object, so issue order is meaningful), `accesskit`
  (M19), `user`, `patch`. A focus fetch of a web node is forwarded to the owning doc actor as an
  `atspi` fact; this replaces X1's "fetched error text is fresher" rule.
- **Writers.** Each node has exactly one writer: the doc actor for nodes of a web document it
  mirrors, the app actor for every other node. `FUSE` runs only there (SM-8).
- **`FUSE`** is a pure function of the registers and static tables, evaluated per node in this
  fixed stratum order (math §1.3, Lemma 2: unique result, terminates):

| Stratum | Reads | Computes |
|---|---|---|
| S-1 quirk | one source record | closed `QuirkAction`s per (app profile, source) from the ADR-0008 table, with hit ids. The table is **created in M22a** (moves `is_gecko`/`is_blink` branches and X1's list-marker row out of core) |
| S0 observed | quirked records of all observed sources | per group: first source in the trust table `PI[app profile][group]` whose value is known |
| S1 patch | **S0** of the node and of bounded neighbours (selector radius ≤ r, ADR-0019 fixes r) | per group: highest-ranked matching patch, rank (specificity desc, `ext_id`, `patch_id`); ties give a `patch-tie` diagnostic. Selectors never see patched values (cascade rule, A4) |
| S2 user | user and site overrides | override value unless `Clear` |
| S3 derived | final values of children and of relation targets in the mirror | name from content (replaces `subtree_label`), `aria-describedby`/`errormessage`/`labelledby` target text, `PosSrc::Computed`, heading level from tag, inherited live; **only if the winning layer for that group is observed-empty or default** |
| R repair | the merged record | SM-1, SM-2, SM-5, role-inapplicable groups masked; every mask recorded in the trace |

  Precedence per group, highest first: **user > add-on patch > observed (AT-SPI or AccessKit by
  `PI`) > derived from the mirror > default**. Per-group layer availability: math §1.4 (e.g. role
  is never derived; text, value and caret are observed-only from the primary source).
- **Unknown vs Absent** (A6). Registers are three-valued: `Unknown` (not fetched, `PropMask` bit
  clear), `Absent` (fetched, no value), or a value. An explicit empty value from a patch or the user
  is final: S3 never fills it (the prototype found this bug, math §2.5).
- **Incremental maintenance.** Dirty set per coalescer batch; recompute S0 for dirty nodes;
  propagate through the reverse selector index (only for properties a selector reads), relation
  reverse index and content-name edges (§2.4); recompute S1..R depth-descending with early cutoff;
  publish only changed entries. Per-batch work budget (2 ms) with focus chain and active
  descendant first; carry-over changes latency only, never the result. Gated by **B-FUSE** (§5).
- **Two connectors on one app** (A8). Default: per-app connector dedupe (the native tree wins: Newton
  for Newton apps, AT-SPI otherwise). Node-level fusion only on exact keys (AccessKit node id in the
  AT-SPI object path, `refs/accesskit/adapters/unix/src/atspi/object_id.rs:14-42`). Structural
  matching is diagnostics only (68–88 % recall, math §3.2).
- **Not fused** (A7): verbosity, `ReportFlags`, language. They gate `compose` and stay in the
  presentation cache key.

### 2.3 Partial data (MC-2)

- `known` travels with the entry. `compose` omits fields of `Unknown` groups and tags the utterance
  `may_refine` when a missing group is one the rule row would speak. A late fact that fills it goes
  through the existing refine path (one late tail, never a re-read).
- Because review, object nav, focus and browse all read the same `SemEntry` (SM-8), "toggle button
  in review, switch in browse" cannot happen; the only cross-surface difference left is
  `Unknown` vs known, which P3 allows and M-6c bounds.
- Focus fetch: `ATTRS` (and the `labelled-by`/`details`/`error-message` relations for web) join the
  focus fetch **as a concurrent call in the same batch**, not a serial round trip. Native review and
  object-nav masks gain `ATTRS` likewise. Fallback if M-6b fails: lazy `ATTRS` after first speech
  with `may_refine`.

### 2.4 Cross-node derived values (MC-3)

Two classes, each with one rule:

1. **Inside `Semantics`** (affect `sem_rev`): name from content, relation target text, computed
   positions, selector matches. Each has an explicit dependency edge maintained by the writer:
   child → parent (content name, stops at the first ancestor not named from content, typically
   ≤ 3), relation target → sources (reverse index, fan-out cap 64; beyond it dependents are
   recomputed in background chunks with `stale` set and ST rules ignore stale groups), parent's
   children list → siblings (positions, dirtied on `children-changed`), selector read set → matched
   nodes. A change at the far end of an edge dirties the dependent and bumps its `sem_rev` if the
   effective value changes. The presentation cache key `(node, sem_rev, name_rev, report_epoch,
   lang_epoch)` is therefore complete.
2. **Context values** (container item counts, table dimensions, ancestor chain, "N items"): never
   stored in `Semantics`. `ContextTracker` and `compose` read them from the mirror at compose time
   (pure, no IPC), uncached per node.

### 2.5 One presentation pipeline in a new pure crate `gorca-present`

`SemEntry → compose(Reason, Ctx, ReportFlags) → PresIr → ext hooks → dedupe/validate → render`
(speech now, braille M10, log/test transcript). `Reason` = Focus, FocusAncestor,
BrowseLine{entered}, QuickNav, SayAll, Review, Change(SemDiff), ListEntry, WhereAmI. Field selection
and order come from one rule table indexed by (role class, reason); per-language order overrides
live in the catalog. Words are catalog ids; render is the only producer of text.

- Every `PresIr` field carries an `Origin` (rule row id, node, group; plus patch/hook/quirk id when
  one contributed) and an optional flat-text span (node + range) for braille routing (M10).
- `Segment` gains `MsgId` (catalog id or literal marker) and an origin id, behind a test/diagnostic
  feature in M22a (needed by P7 and by the "why" trace).
- `ListEntry` composes lazily, visible rows only (M15).
- Until ADR-0016 lands, `gorca-present` defines an interim `ReportFlags` newtype that ADR-0016
  adopts unchanged.

Invariants:

- **PR-1** At most one `Role` and one `Name` field per node per utterance across all producers of
  one output event, **checked after extension hooks**.
- **PR-2** Container enter/leave fields come only from the context tracker (§2.8).
- **PR-3** Same entry and reason give the same fields on every surface; allowed differences are rows
  of the rule table or `Unknown` groups.
- **MI-1** Every non-hidden atomic presentable node owns exactly one flat-text anchor. The `gorca-doc`
  flat builder **collapses** the U+FFFC markers of an icon-only subtree (presentational,
  `aria-hidden`, decorative or unnamed images) into the control's single anchor; it does not add a
  second one. Inline runs (`[Close][Menu][Search]` on one line, audit §BT1 cause 1): each anchor is a
  word and a character unit, and word/char moves onto an anchor speak the control via
  `compose(BrowseLine)`.
- **PR-4 (new)** Position info ("2 of 5") per the audit G2 rule: spoken for Focus and WhereAmI,
  never in BrowseLine/QuickNav/SayAll by default; `setsize ≤ 0`, `posinset > setsize` and
  container-level `setsize` are ignored.
- **PR-5 (new)** `aria-roledescription` is honoured (QA-3) but never hides the native role from the
  object-info/where-am-I key; ignored when empty, equal to the name, or longer than 64 scalars.

**`PresIr` frozen at version 1 (M22b1 slice 2, 2026-10-24).** The records live in
`crates/gorca-present-ir` (no runtime, so the plugin API can depend on it), `PRES_IR_VERSION = 1`.
The slice 1 draft was blocked by review (`docs/reports/m22b1-review.md` P-1..P-5) and corrected
before the freeze: `Content` is `Text | Msg { id, args } | Ext { ns, key, args } | Opaque { code }`
(numbers stay numbers; add-on text is an extension catalog reference, never a raw U+001E string);
`Field { kind, flags, lang, content, origin, span }` (56 B) with `FieldFlags::{SECRET, VERBOSE_ONLY}`;
`lang` is a host-interned BCP-47 content-language tag (`LangTags`; on the wire the tag string, never a
catalog index; an unknown tag is absent); `ArgVal` has `Opaque { code }`;
`PresIr { reason, node, sem_rev, may_refine, flags, fields }`; `Reason` has `Unknown(u16)`.
Wire rules, each with a test: kinds are codes and an unknown kind (reason, content, field kind)
passes through unedited and never traps; words travel as catalog key strings, never as the
build-order `MsgId` number; add-on `FieldKind` codes are allocated by the host per
(namespace, name) (`ExtKinds`: the namespace is an `ExtNs` token bound by the host from the manifest id,
256 kinds per namespace), a guest never picks a raw code; secret fields are redacted before a
plugin, a log or braille sees the IR (`PresIr::redacted`). `FieldKind` codes 1..13 are
append-only. A change that is not an append of a code bumps the version and amends this ADR; the
`shape_of_the_records_is_frozen` test destructures every record exhaustively. Evidence: the native
corpus replay shows 0 differences outside `deviations.toml` (was 169 of 375). Plan:
`docs/reports/m22b1-plan.md`.

### 2.6 Provenance and "why was this said" (MC-4, A1)

- `Semantics` carries no provenance; `ProvTag` (u64) and the `stale`/`fallback` masks live beside it
  in `SemEntry`. A provenance-only change bumps nothing and speaks nothing (F6).
- `explain(node, group)` re-runs `FUSE` in trace mode over the retained registers (same generic code,
  no-op tracer on the hot path) and returns every candidate with its status (won, shadowed,
  unknown, stale, no-value, masked-by-repair) and rule ids.
- End-to-end chain: utterance segment (`MsgId`, origin id) → `PresIr` field (rule row, hook ids,
  budget-skip marks) → (node, group) → `explain` → fact seq → ingress ring buffer (ADR-0013 M11
  diagnostics). Exposed as a diagnostics command "explain last utterance" (Control API, M14; a debug
  key binding before that). Hook budget skips are recorded in the trace (P10 nondeterminism made
  visible).

### 2.7 State changes (ST-1..ST-6)

Spoken for: the focus (ST-1); focus ancestors up to 8 levels for expand/select/check/press/sort/busy
(ST-2, NVDA #10890); the active descendant (ST-3); a node gorca activated in the last 2 s, including
browse-mode activation (ST-4, **on**, QA-1); the browse cursor node (ST-5, **off**, QA-1; setting
`report.state_changes_at_cursor` stays); never other nodes (ST-6). Diff the effective entry against
the last *spoken* entry per channel; stale or provenance-only transitions are not changes.
User-caused changes are spoken at the first reply and refined (not queued) for 250 ms;
`Prio::Normal`, cancel key `(node, Change)`; per-app limit 8 change utterances/s with per-node
coalescing. `aria-busy` suppresses change and live-region speech inside the subtree until it clears
(bounded at 10 s); live regions keep ADR-0003 §13.

### 2.8 Container context

One `ContextTracker` per channel (focus, browse cursor, review) diffs the chain of context-bearing
ancestors and emits `ContainerLeave` (browse only) and `ContainerEnter` with name, kind, item count or
table dimensions, from mirror data only (§2.4 class 2). Native ancestors not yet cached are omitted,
then refined; never fetched by IPC on the presentation path. Replaces `leaving_items`,
`container_items` and the P3 leads.

### 2.9 Extensions (MC-5, MC-6; ADR-0019)

**Patches** (ADR-0019 PA-1) map to fusion layers or are explicitly outside it:

| Patch | Where |
|---|---|
| `rename`, `label-by` | S1, name group |
| `re-role` | S1, role group (then `R` repairs dependent groups) |
| `live` | S1, live group |
| `hide` | S1, existence group (removes the node from browse/quick-nav/review sets) |
| `flatten` | mirror structure (flat builder), not `Semantics`; recorded in the trace as a structure patch |
| `claim`, `sleep` | hook routing and lifecycle (ADR-0019), outside `FUSE` |
| `category` | adds membership in an **add-on-defined** category set stored beside the entry (`ExtCats`); built-in `Cat` stays a pure function of `Semantics` (SM-1). To join a built-in category an add-on re-roles the node |

Patch-set changes (enable, disable, staged update) are an epoch: full re-fuse in background chunks
on the writer's lane under the batch budget, focus chain first.

**Every extension text path:**

| WIT path | Treatment |
|---|---|
| `present` hook | 0.9: an IR edit hook (add/drop/reorder fields, open `Field` kind codes) in fixed ADR-0019 phase/before/after order; then dedupe/PR-1; then render. The 0.2 `option<list<segment>>` replace form is a legacy adapter: its segments become IR fields (`Role`-kind segments count for PR-1) |
| `policy` `Value(text)` / `Say(segs)` / silence | IR edits (replace value field / replace fields / drop all), same dedupe and render. Covers `ext-gnome-shell` (`shell_policy.rs`) |
| `on-event` `rewrite` | IR replacement for that event's utterance, same dedupe and render |
| site `describe` (ADR-0019) | IR edit hook for BrowseLine/QuickNav reasons |
| `host.speak`, `command-result::speak` | declared **content utterances**: outside PR-1..3, but rendered by the same render stage (braille, log, catalog tagging) and rate-limited by the ADR-0019 effects buffer |

Content streams (terminal ADR-0018, editor echo, live-region text) are also outside PR rules and go
through render. ADR-0021 "copy what the user heard" copies flat text, never render output.

**Model across the WIT boundary (V-4):** add-ons read semantics through accessors over the
effective entry (`sem-role: func(n) -> u16`, `prop: func(n, code: u16) -> option<prop-value>`,
name/description accessors), with open code registries for roles, properties and `Field` kinds
(V-3). No frozen node record carries `Semantics` in WIT 0.9.

### 2.10 Ordering with M18a (owner decision 2026-10-18)

The owner decided that **M22a lands before M18a**, so the add-on WIT 0.9 is designed directly on the
final model (simpler contracts, no shim, easier maintenance). This supersedes the 2026-10-17
"Phase 3 starts with M18a" order for the M18a slot only. Consequences:

1. **M22a → M18a is a hard dependency.** M18a starts when M22a has merged: `Semantics`, `SemEntry`,
   `FUSE`, the quirk table and the code registries exist in the core.
2. **Code registries** (`SemRole`, property and `Field` kind codes, V-3) are produced by M22a as
   data in `gorca-ext-api` and frozen append-only by M18a with WIT 0.9.
3. **Representation across the boundary.** Accessors over the effective entry (§2.9) remain the
   recommended WIT design on their own merits (V-4: growing inputs through accessors; no copy of
   the entry per hook call). ADR-0019 C-1 may still choose a record or a resource; with the model
   final, either is safe. Not a sequencing mechanism any more.
4. **`present` hook** ships in 0.9 in the IR-edit form (§2.9), which needs the `Field` registry
   from M22a and the `PresIr` shape from M22b1. If M22b1 has not merged when M18a freezes, the IR
   record is frozen from the M22a design sketch and M22b1 implements it unchanged.
5. **Selectors and patches** in M18a are written against `FUSE` layers S1 (patch) and the registry
   names; M18a owns the selector engine, M22a owns the layer it plugs into.
6. M22b4 (extension text paths through the pipeline) follows M18a.

### 2.11 Migration (MC-7): strangler, shadow diff, per-surface switch

- **M22a** dual-carries `SemEntry` beside `AttrMap`/`StateSet`, with a debug assertion that legacy
  reads agree with the entry (disagreements logged with the node and group, a test failure in CI).
  Adds `MsgId` + origin to `Segment`. Builds `FUSE`, the quirk table, `gorca-sem-atspi`, and
  replaces X1/X4 (`web_overlay`, `subtree_label`, the second normaliser).
- **M22b1..b4** run `gorca-present` in **shadow mode** next to each legacy presenter and diff token
  multisets (kind, `MsgId`, literal) over the M8 fixtures and the tier-1 covering array; the only
  allowed differences are listed in `crates/gorca-present/deviations.toml`, each with a reason.
  A surface switches when its shadow diff is zero outside the deviations, and its legacy code is
  deleted in the same slice, with no flag (section 8: dated windows, exit gates, provisional
  facts instead of reader fallbacks, xtask rule 12). Order: b1 change + focus (P1, P4) → b2 browse/quick-nav/say-all
  (P2, P3) → b3 review/object nav (P8, P9) → b4 window/notification/info/where-am-I and every
  extension path (P5–P7, P11). X2/X3 move to `Semantics` reads in b2.
- **Acceptance gate for every slice:** math tier-1 properties P0–P10
  (`aria-test-coverage-math.md` §2) and fusion properties F1–F10 (`semantic-fusion-math.md` §5)
  pass in `cargo test`; F1–F10 run against a fake two-source connector until M19 exists.
- P2-ARIA-0 (Phase 2) implementation waits on the BT1 repro (audit §BT1); its tests stay as
  regression guards through M22.

### 2.12 Verbosity

`ReportFlags` (ADR-0016; interim type §2.5) gate fields in `compose`; new bits: position info,
levels, landmarks, container kinds, role description, details, current, state changes (+ at
cursor), negative states. Verbosity ships as presets in profiles (M14a, QA-2) per report §5.5.
Vendor profiles may set them.

## 3. Alternatives considered

- **B. Typed accessors over the existing attribute map.** Cheapest; keeps string parsing, the
  bit-pair ambiguity and per-presenter field ownership (the bug class remains). Rejected.
- **C. Generic property bag (`PropId → PropValue`).** Most extensible; loses exhaustive matching,
  more memory, slower compose. Kept as the WIT-side representation (open codes + accessors).
- **D. Orca-style per-role generator classes.** Duplicated per-script logic. Rejected.
- **E. Fully declarative presentation grammar.** Adopted only for field order.
- **F. Fusion by arrival-order last-writer-wins across sources** (what X1 does implicitly). Not a
  function of the fact set: diverged in 660/660 shuffled deliveries vs 0/720 for registers
  (math §0). Rejected.
- **G. Numeric confidence per source.** Uncalibrated across engines, unexplainable. Rejected for a
  static trust table `PI`.
- **H. Provenance inside `Semantics`.** 88 B per node (measured) and provenance-only changes look
  like semantic changes. Rejected for `ProvTag` beside the struct.

A vs C for the core representation is settled by a 2-day micro-benchmark (compose cost, memory on
the 100k-node synthetic page), judged by the reviewer on M-4/M-5/M-9; no full bake-off.

## 4. Consequences

- One fusion point and one presentation point; "why was this said" becomes answerable.
- Native apps gain position, level, placeholder and shortcut reporting where toolkits export them.
- The focus fetch grows by one concurrent `GetAttributes` (M-6b guards it).
- Cross-crate migration: `gorca-model`, both AT-SPI crates, `gorca-doc`, `gorca-core` (presenters,
  doc actor, browse, review, info, window, notification), `gorca-ext-host`, `gorca-daemon`
  (shell policy). NVDA-parity tests in `aria.rs` move to `gorca-present`.
- The ADR-0008 quirk table finally exists (M22a); engine knowledge leaves the core.
- Writers carry registers and reverse indices: more memory than r1 (M-9 revised).
- M18a waits for M22a (owner order, §2.10); WIT 0.9 freezes the final model's codes.
- Newton/AccessKit (M19) is **not** a 1:1 mapping: `Toggled` maps to `Check` or `Press` by role;
  the connector must implement accname consumer-side and derive `NameFrom`; AccessKit's ~180 roles
  need a mapping table with an "unmapped" list (M-10); dual exposure follows §2.2 A8.
- The app actor routes property changes for focus ancestors and the active descendant (bounded).

## 5. Metrics to validate

| Id | Metric | Gate |
|---|---|---|
| M-1 | Cross-surface consistency (P3) over fixtures and tier-1 array | 0 unexplained differences |
| M-2 | NVDA parity over ≈30 APG patterns | ≥ 95 % token agreement on P0 patterns |
| M-3 | GTK4 native parity fixture | role/state/position words equal to the web equivalent |
| M-4 | `compose` cost (B-SEM-1) | p99 ≤ 3 µs/node |
| M-5 | Browse line (B-SEM-2, 100k nodes) | p99 ≤ 60 µs, no regression vs LP-3 |
| M-6 | Focus keypress → speech, gorca-side | Δp99 ≤ 0.1 ms vs before M22b1 |
| **M-6b** | **Cold focus** (uncached node) keypress → speech start, end-to-end incl. IPC, Firefox + GTK4 fixtures | Δp99 ≤ 2 ms vs before M22a; else lazy `ATTRS` fallback (§2.3) |
| **M-6c** | Focus utterances with an `Unknown` group the rule row wanted | ≤ 5 % on the fixture corpus; each refined within 100 ms |
| M-7 | 10k non-focus state events/s | 0 utterances, reader lag p99 unchanged |
| M-8 | Check box toggle → speech | ≤ 10 ms gorca-side p99; ≤ 1 utterance per 250 ms flip-flop |
| **M-9** (revised) | Memory | `Semantics` ≤ 64 B inline; `SemEntry` ≤ 112 B; 100k-node page RSS increase ≤ 25 MB counting **every** carrier, registers and reverse indices |
| M-10 | Mapping coverage (ARIA 1.2, audit, AccessKit roles) | every role/state mapped or listed as unmapped |
| **M-11** | **B-FUSE**: 10k–50k mixed events/s, 100k-node mirror, 100 patches | fusion + publish p99 ≤ 2 µs/event; no ingress lag regression |
| **M-12** | Fusion properties F1–F10 + P0–P10 | all pass in `cargo test` (tier 1) |
| **M-13** | Shadow diff per surface (§2.11) | 0 differences outside `deviations.toml` before each switch |

Details: report §8, math reports.

## 6. Open questions

- **QO-1 (answered 2026-10-24: M22b1 freezes `PresIr` itself, at the end of slice 2, section 2.5)** If M22b1 lags M18a, is freezing the `PresIr` record from the M22a sketch acceptable,
  or should M18a wait for M22b1 too (§2.10 item 4)? Recommendation: freeze from the sketch.
- **QO-2** ADR-0019 selector radius r and the relation fan-out cap (64) are engineering guesses;
  B-FUSE and XP-8 settle them.
- **QO-3** Trust table `PI` defaults are heuristic (math §1.4); first rows from the M8 fixtures.
- **QO-4** Newton per-tree update ordering and client-PID mapping are unverified (math §9); only
  M19 needs them.
- **QO-5** Structural events (subtree replacement) with derived names and selector indices are not
  simulated; F7 in Rust covers them.

Answered: QA-1 yes (ST-4 on, ST-5 off), QA-2 yes (presets in profiles, M14a), QA-3 yes
(`aria-roledescription` honoured by default, with PR-5) — `docs/decisions.md` 2026-10-18.

## 7. Review traceability

MC-1 → §1.1, §2.2. MC-2 → §2.3, M-6b/M-6c. MC-3 → §2.4. MC-4 → §2.1 (`SemEntry`), §2.6. MC-5 →
§2.9. MC-6 → §2.9, §2.10 (order M22a → M18a by owner decision). MC-7 → §2.11, M-12/M-13. MC-8 → §2.1 (roles), §2.5 (MI-1, PR-4, PR-5),
§2.11 (BT1), §4 (Newton), §6 (QA answers). Should-change items: braille spans, lazy `ListEntry`,
interim `ReportFlags` (§2.5); M-9 all carriers (§5); budget-skip logging (§2.6); dual-connector
precedence (§2.2); roledescription info key (PR-5).

## 8. Cut-over and no-legacy rule (r4, 2026-10-24)

Owner rule (2026-10-24): when gorca moves from one epoch to the next, here from the legacy
presenters and mirror attribute reads to the fused semantic model and `PresIr` v1, it leaves no
tails and no paths back. Every consumer reads only the new model; legacy code is deleted, not kept
alongside. The failure it prevents: "Firefox says this because the focus presenter uses that
branch, browse uses `SemEntry`, an extension fixes the role, but review reads the old `AttrMap`."
This section replaces the "strangler" wording of section 2.11 where the two differ. Inventory and
the per-slice deletion list: `docs/reports/2026-10-24-architect-m22b-legacy-inventory.md`.

### 8.1 What is legacy, what is not

Legacy is any read of a *presentation fact* (role, name, description, value, spoken states,
attributes, word tables) from anything but the node's fused `SemEntry` (through `Ivm::entry_view`)
or from `PresIr`. That includes `NodeMeta` fields, `AttrMap`, `NodeSnapshot.attrs/description`
read by a consumer, `Role::spoken`, the `aria` tables, `present_focus`/`present_web`/
`present_change`, `web_overlay`, `subtree_label`, and `PolicyFacts` built from a snapshot.
Not legacy: identity, tree links, bounds, text content, and *structural* state (`SHOWING`,
`VISIBLE`, `DEFUNCT`, `FOCUSABLE`, `FOCUSED`). Structural state is the typed `StructState`
(`gorca-model`, M22b3): `NodeSnapshot::struct_state()` projects the structural bits of `StateSet`
(`SHOWING`, `VISIBLE`, `DEFUNCT`, `FOCUSABLE`, `FOCUSED`, `MULTILINE`) and says whether the states were
read at all. Review and object navigation read nothing else of the state set (rule 12 ban
`snapshot-states`, scope `review/`); every spoken state is a group of the fused entry. The `StateSet`
field itself leaves the node record in b4 (inventory gap 1, closed for review).

### 8.2 Shadow windows are temporary

Each surface has a shadow window (section 2.11) with an **expiry date** and exit gates. The dates
are those of `xtask/legacy.allow`; they move only by a `docs/decisions.md` row.

| Surface | Slice | Window ends (expiry) |
|---|---|---|
| Focus, change (P1, P4) | M22b1 slice 5 | 2026-11-07 |
| Browse, quick-nav, say-all (P2, P3) | M22b2 | 2026-11-21 |
| Review, object nav (P8, P9) | M22b3 | 2026-11-28 |
| Window, notification, info, explain, extensions, carriers (P5-P7, P11) | M22b4 | 2026-12-12 |

Exit gates, all required before the cut-over commit:
1. **Diff**: M-13 holds, 0 differences outside dated `deviations.toml` rows on the fixtures and
   the tier-1 array; web token diff <= 0.5 %; a deviation row never outlives its surface.
2. **Latency**: M-6 (and M-6b for cold focus) measured on the real bench, interleaved, load and
   governor recorded; M-7 and M-8 where the surface has them.
3. **Soak**: at least 8 hours of real use over at least 3 days with shadow on, `different` ratio
   < 1 %, `dropped_full` = 0, no new deviation id; the counters go in the slice report.

If a gate fails at the expiry date the answer is not an extension flag: the owner either accepts
the measured deviation (a decision row; the path is deleted anyway) or declares the slice failed
and moves the date by a decision row.

### 8.3 Cut-over deletes, in the same slice; no flag keeps the old path

* The slice that switches a surface deletes that surface's legacy path in the same merge. No
  environment variable, setting, cargo feature or `cfg` selects the old presenter, and there is no
  "fall back to legacy when the entry is missing" branch. Rollback is `git revert` of the slice.
* The shadow hook for the surface (`GORCA_PRESENT_SHADOW`, `CoreConfig::present_shadow`, its diff
  classifier) is deleted with it. The replay corpus stays as golden tests of `compose`.
* The slice lowers or removes its rows in `xtask/legacy.allow` in the same commit (8.5).

**Pre-fusion fallbacks move into the model as lowest-trust facts.** The C3 overlay exists because
the document actor publishes a node batch before fusion has produced its entry (a 40-50 ms window
on a big page), and the Reader then reads `NodeMeta` instead. That branch is the leak, so the
window is closed in the writer, not worked around in the reader:

* **S5 provisional** is a new stratum below S4 inferred: user > patch > observed > derived >
  inferred > **provisional** > default, with `ProvCode::Provisional` as its provenance code.
* When the writer (document actor for web nodes, app actor otherwise) publishes a node, it writes
  in the *same* publish a provisional entry made only from total, local functions of the source
  record: role map, state map, the name and description as the engine reported them, and, for a
  control the engine left nameless, the one-line subtree text label capped at 160 bytes
  (`gorca_fuse::content_label`, the function S3 uses: one implementation, the fallback is not a
  copy). Content-derived values are tainted; provisional values are spoken plainly (no hedge) and
  never beat an S0..S4 value of the same group.
* Fusion then supersedes: the first register write or derivation for that group replaces the
  provisional value by the ordinary strata order. A change after an utterance started follows the
  existing refinement rules (M-6c, `may_refine`).
* **PV-1 totality**: from the moment a node is visible to a reader surface, `entry_view` returns an
  entry for it. The Reader has no `Option<SemEntry>` branch; its input type is a fused node
  (record + entry), so a missing entry cannot be written. The cold-focus path (fetch miss) builds
  the provisional entry from the fetched snapshot before publish.
* **PV-2** provisional never wins over a fused value. **PV-3** `ProvTag` shows it, so explain and
  the shadow can say "provisional". **PV-4** a provisional value is never saved to a profile and
  never visible to patch selectors (ADR-0019 selectors read S0 and above).
* Cost: the provisional label runs the existing S3 function once per nameless control, bounded by
  the 160-byte cap; B-FUSE measures it (M-14b). If it exceeds M-11 the label is dropped (the
  control speaks its role until fusion lands) and that degradation is recorded; the answer is
  never to put the branch back in the Reader.
* The C3b inferred form label (slice 4) is an S4 inferred fact in the entry, with taint and the
  layout revision it was measured at, not a Reader overlay; the Reader overlay is deleted in
  slice 5.

### 8.4 One source of truth for every consumer

After b4 the readers of presentation facts are `gorca-fuse` (writer), `gorca-present` (compose,
render, word tables) and the ADR-0026 accessors for extensions. Browse, quick-nav, review,
explain, the braille plan, window, notification and policies call `compose` or an accessor over
the same entry, so "Firefox says this in focus but that in browse" can only be a difference in
`compose` rules, and an extension role fix (an S1 patch) shows up in every surface at once.
Structure predicates (group, modal, category) read the entry's structure groups (section 2.11,
X2/X3), not attributes.

### 8.5 Enforcement: xtask rules 12 and 13

`cargo run -p xtask -- legacy` (also part of `cargo xtask deps`, so CI runs it):

* `BANS` in `xtask/src/legacyrule.rs` lists the banned identifiers (`subtree_label`, `web_overlay`,
  `overlay_from_meta`, `legacy_name`, `Legacy`, `present_focus`, `present_web`,
  `present_selected_item`, `present_with`, `present_change`, `aria::`, `mod aria`, `state_words`,
  `.spoken()`, `legacy_family`, `NodeMeta`, `.meta`, `AttrMap`, `.attrs`, `web_attrs::`,
  `PolicyFacts`, `mod agree`, and `NodeSnapshot` in the extension crates), each with a path scope
  and the replacement printed on failure. The braille crates, the extension host, the plugin API
  and `extensions/` are in scope of the bans that matter to them.
* `xtask/legacy.allow` rows are `ban | file | uses | expires | slice`. It is a ratchet: a use with
  no row fails; more uses than the row allows fails; fewer uses than the row says fails (the slice
  must lower it, so the list only shrinks); a row past its expiry **fails CI** (delete the path,
  do not extend); no expiry may be later than 2027-03-31; every row names its deleting slice.
* A slice that introduces a replacement API adds the ban for what it replaces in the same change,
  and a slice that cuts a surface over removes its rows. At the end of b4 the allowlist file is
  empty and is deleted; the bans stay as a permanent guard.
* Test code, comments and `tests/`, `benches/`, `examples/` are not scanned: test code of a legacy
  path dies with it. The rule is a text check, so a renamed wrapper around a banned read escapes
  it. Hence the review checklist of every M22b slice includes "no new reader of a presentation
  fact outside `compose` or the accessors", and b4 makes the fields private (visible to
  `gorca-fuse` only), which the compiler enforces.
* **Rule 13**: the `host` cargo feature of `gorca-present-ir` (`ExtKinds`, the `ExtNs` binding,
  `LangTags`) is enabled by `gorca-present` only. No other manifest, above all no plugin-API crate
  or extension, may enable or forward it, and the IR crate's `default` features may not include it.

### 8.6 Extensions and plugins get no legacy data, ever

Extensions and plugins (ADR-0026) read facts only through accessors over `SemEntry` and write only
through ADR-0019 patches and `PresIr` edit hooks. They never see `NodeSnapshot`, `AttrMap`,
`NodeMeta`, the legacy `Role` enum or `StateSet`: WIT 0.9 role codes are `SemRole`. The
`ext-snapshot`, `attr-map`, `attrs-field`, `web-attrs` and `policy-facts` bans enforce this for
`gorca-ext-api`, `gorca-ext-host` and `extensions/` (today `convert.rs` and `host.rs` still convert
from `NodeSnapshot`: allowlisted until b4). An extension-side "role fix" is an S1 patch that fusion
applies before any surface reads, so an extension cannot make one surface differ from another. A
plugin that needs a fact the accessors lack gets an accessor (a WIT version bump), never a back
door to the old carriers.

### 8.7 Slice plan changes

`docs/reports/m22b1-plan.md` is amended: slice 4 builds the provisional stratum and moves the C3
and C3b fallbacks into the entry; slice 5 deletes the focus and change legacy and the Reader
overlay; b2, b3 and b4 each end with their deletions and their allowlist rows removed. The
structural/presentation split of `StateSet` (8.1) lands in b4 at the latest.

### 8.8 Consequences and metrics

* More work per slice (deleting, not only adding) and no rollback by flag; accepted by the owner
  for the sake of one behaviour per fact.
* The provisional stratum adds a write per published node, inside the `SemEntry` size already
  budgeted (M-9); no new per-node storage.
* **M-14**: `xtask/legacy.allow` holds zero rows of a slice's surface at the end of that slice and
  is empty at the end of b4. **M-14b**: provisional-label cost in B-FUSE within M-11. **M-14c**:
  PV-1 test: no published node without an entry under the F1-F10 shuffles.
* Open: QO-6, the `StructState` shape and entries for lazily cached ancestors (inventory gaps 1, 2).

### 8.9 The cut-over gate (owner rule, 2026-10-24)

Before a surface moves to `gorca-present` (PresIr v1) the difference between old and new output on
every recorded corpus is zero, and whatever remains is an explained epoch change. The gate is one
command per surface (`focus`, `change`, `browse`, `quicknav`, `review`, `window`, `notification`,
`explain`): `cargo run -p xtask -- cutover <surface>` (`xtask/src/cutover.rs`).

* **Replay.** The test `cutover_gate_tests::cutover_emit` (`gorca-connector-atspi`) feeds every
  family of `tests/fixtures/sem` (`gtk3`, `gtk4`, `firefox`, `chromium`, `electron`, `telegram`;
  the list and its dated waivers are `tests/fixtures/sem/cutover-corpora.toml`) through the real
  fusion writer, the legacy presenter and `gorca-present`, and writes one canonical record per
  node. `change` has no recorded state-change corpus, so its replay flips one state (or the name)
  of every focusable node, one case at a time, and compares `legacy change` with `compose_change`.
  `xtask` runs the test binary inside `tests/harness/run-isolated.sh` (private buses, no display),
  never in the live session.
* **Classification.** EQUAL (identical tokens, in order; "silent" when both say nothing), INTENDED
  (a `deviations.toml` row for exactly this surface, not expired, with a reason and an `epoch`
  tag) or UNCLASSIFIED (everything else, including order-only differences and a row that is
  expired, untagged or reasonless). The gate passes only with 0 UNCLASSIFIED, a non-empty replay,
  every required family covered (a family with no fixtures needs a dated, reasoned waiver, which
  the report prints) and determinism.
* **Determinism.** The replay runs twice plainly and once with a seeded shuffle of the arrival
  order and batching (siblings keep their relative order, because the child index is data); the
  three record files must be byte-identical. Fused values are a function of the final state
  (section 2.2), so any difference is a finding: a nondeterministic fusion or presenter fails the
  gate.
* **Report.** The terminal table and `bench/results/cutover/<surface>-<date>.md` (first line
  `result: PASS|FAIL`, rev, input hash, load, governor, per-family table, intended rows,
  grouped unclassified differences, plus a `.unclassified.tsv`). A later report never overwrites an
  earlier one; failing reports stay. A surface whose path does not exist yet reports FAIL by design.
* **CI hook.** `cargo xtask legacy` (rule 12, part of `cargo xtask deps`) also fails when no
  `xtask/legacy.allow` row names the slice that switches a surface (that surface is cut over) and
  `bench/results/cutover/` has no git-tracked PASS report for it whose `rev` is an ancestor of
  `HEAD`. A cut-over commit therefore ships its passing report, and the legacy deletion that
  empties the slice's rows cannot land without one. `cargo xtask cutover --check` runs only this.
* **Order of events for a surface:** gate PASS, commit the report, tag main `legacy/<surface>-last`
  (annotated), delete the legacy path and its rows (this section, 8.3), run the tests.

### 8.7 Status after M22b4 part 4 (2026-10-25)

Done: the mirror node carries the fused entry only (`NodeData`: source id, transport key, field
value, entry); presentation, extension words (`Reason::Ext`), explain and the document carriers
read the fused model; rule 12 has no allowlist.

Not done, moved to the named follow-up slice **M22c**: the promises of 8.1 and 8.5 that
`NodeSnapshot` fields become private and that legacy `Role` and `StateSet` leave the node record
(about 1330 `Role::` uses in 91 files and 395 `StateSet` uses in 48 files of production code).
M22c makes the snapshot fields private, moves each consumer to the fused entry and deletes the
legacy `Role`/`StateSet` from the record. The semantic-model epoch is closed by M22c, not before.

Known difference to record: label inference in the document actor skips a control whose role the
author set by `ROLE_AUTHORED` (a valid role token). The retired check skipped on any non-empty
role attribute, so a control with an unknown role token, or a focusable presentation node, now gets
an inferred label where Orca (`has_role_from_aria`) would not infer one. Accepted until M22c; the
fix is a `ROLE_AUTHORED`-like bit for "any role token present".

#### M22c part 1 (2026-10-25): review and object navigation off the legacy record

`crates/gorca-core/src/review/` reads the fused entry, `struct_state()` and the new
`NodeSnapshot::caps()` only; rule 12 bans legacy `Role`, `StateSet`, `Ifaces`, `.interfaces`,
`.states` and `.role` there (`PORTED` in `xtask/src/legacyrule.rs`, a ratchet: a module is added
when ported and never removed). Plan and inventory: `docs/reports/m22c-plan.md`. Oracles: the review
goldens (35270 rows, none changed), the new `walk-build` rows (`wants_lines`, surface role) and the
review tests. Dated differences, all in which nodes the build asks the app for text lines
(`wants_lines`, an extra bounded request answered "no" by a node without Text) and in the tree root:

* 2026-10-25: nodes whose legacy role was `Unknown` (date and time editors exported as groups,
  `<ins>`, `<del>`, `<sub>`, `<sup>`, `<mark>`, definitions) now have a text-bearing model role
  (`TextBox`, `Insertion`, `Deletion`, `Subscript`, `Superscript`, `Mark`, `Definition`), and an
  ARIA `option` exported as a filler is `Option`: all are asked for lines. 35 nodes of the recorded
  corpus; none changes a spoken word.
* 2026-10-25: the application root is `Application` without an author token and without a parent
  (fetched), as a node of the same model role with a parent is an embedded object or an ARIA
  widget. The legacy check was the raw role, which the entry no longer carries; the golden caught
  the HTML `<object>` case (raw `embedded` folds into `Application`). A root whose parent was never
  fetched takes one ancestry fetch before the walk stops, where the legacy check stopped at once.
* 2026-10-25: `MatchRule::showing_status_bar` hides the connector's role and state types from the
  review build; the rule sent is the same.

#### M22c part 2: text layer (2026-10-25)

`crates/gorca-core/src/text/` reads the fused entry (`SemRole`, `SemFlags::EDITABLE`) and
`struct_state()` (defunct, focused, multi-line). Decision (coordinator, 2026-10-25): `Ifaces` stays
private structure on the snapshot behind `NodeCaps`. The golden case `text-track` (one row per
recorded node) guards `trackable`.

* 2026-10-25: Chromium date, time, month and datetime-local inputs are exported with a role the old
  mapping had no word for (`Unknown`, not editable, so never tracked); the fused role is `TextBox`,
  which the text layer always follows. 7 recorded nodes. The caret machine now sees their text
  object; nothing else changes.

#### M22c part 2: commands (2026-10-25)

`crates/gorca-core/src/commands/` asks the fused role. New shared module `nodekind` (`role_of`,
`is_app_root`, `is_app_node`, `is_window_role`). The `Words` fields of the where-am-i answer are
`role_text` and `state_text` (they were `role` and `states`, names the ratchet reserves for the
legacy fields). Golden case `info-shape` (collection, container, window, application per node).

* 2026-10-25: the container chain of "where am I" names a form, a radio group, a figure, a
  presentational node, a log, a timer, a row group, a feed and a comment as containers, and an
  `alertdialog` is a window as a dialog is; a container the catalog has no role word for
  (`roles::UNMAPPED`) is skipped unless it is named (review fix: the web row would say "text").
  The `whereami` goldens change in 40 records per language against the legacy baseline (80 lines
  of 28296 in `whereami.en.tsv`, same in `.ru`): a control in a Chromium alert dialog now starts
  with the dialog (as in Firefox, where the dialog already was one), a named log or timer
  (`Chat log`, `Countdown`) is spoken, an ARIA option exported as a filler no longer says
  `list item`, a layout table no longer says `panel`, a GTK label inside a named label container
  says that container; 9 records lose an unnamed container that the old chain spoke as `text`
  (a figure, comment or generic filler).
* 2026-10-25: the application of the chain is an application node without an author token whose
  parent is absent or was never fetched; an embedded object inside the tree is not one.

#### M22c part 2: present (2026-10-25)

`crates/gorca-core/src/present/`, `focus.rs` and `nodekind.rs` read the fused entry,
`struct_state()` (now with `ACTIVE`) and `NodeCaps` (now with `has_value`). The presentation log
and the mirror probes keep the connector's names for roles and states as a diagnostic
(`NodeSnapshot::role_log_name`, `state_names`, `with_selected_for_log`; no decision reads them).
The label inference of the document actor and the gorca-doc label planner take the control's
`SemRole`; the document tree's own node roles stay for the doc port. Golden case `present-shape`
(needs-name, description request, flap, label, label inference, dialog-read part, reads-content).

* 2026-10-25: a check or radio *menu item* no longer gets a label inferred (it was a check box or
  radio button to the old mapping; Orca infers for check boxes and radio buttons only): 7 nodes.
* 2026-10-25: date and time editors (old role `Unknown`, now `TextBox`) are asked for a
  description, may get an inferred label and are skipped by a dialog read (2 nodes); `<ins>`,
  `<del>`, `<sub>`, `<sup>`, `<mark>` and definitions (old `Unknown`) are read as text by a
  dialog read (6 nodes); one image (old `Unknown`) is skipped; a layout table (`Presentation`) is
  looked into, not skipped (1 node); an item the engine exports with a menu role and an ARIA option
  exported as a filler need a name like menu items (2 nodes).

#### M22c part 2: document actor and browse mode (2026-10-25)

The mirror's document nodes carry the fused model role: `gorca_doc::web::NodeSpec::role`,
`NodeView::role` and `PropsPatch::role` are `SemRole` (a node without an entry or record takes
`SrcNode::model_role`, the closest model role of the source role; `NodeSpec::new` accepts a
legacy `Role` through `From<Role> for SemRole` for tests and fixtures). The categories
(`cats_of`), the table and label planners, `doc_actor` (`flags_sem`, `translate`, `live`, `mod`,
`repair`) and all of `browse/` read `SemRole`, the fused flags and the toggle states of the entry;
none names the legacy `Role`, `StateSet` or `Ifaces`. `forms::passes_through` takes `PassFacts`
(from the fused semantics; a disabled web control still passes keys, as before). The focus
snapshot of a document node (`web_focus_snapshot`) is built by `NodeSnapshot::from_fused` in the
model, which derives the legacy fields there for the consumers that remain (`impl From<SemRole>
for Role`). `doc_actor/fuse.rs` is not in the ratchet yet: its `row_of` hands raw state bits to the
plugin watch rows (`plugin_port::Row`), a bridge another slice owns. `SrcNode` keeps
`is_visible`, `is_focusable` and `reports_invalid` for the structural facts the source reports even
where the fused flags group was not fetched.

Goldens: `docflags.tsv` 6 records, `quicknav.{en,ru}.tsv` +18 nodes each, `browse.*` unchanged.

* 2026-10-25: a node the engine exports as an alert is flagged `LIVE` like any live region (it was
  excluded because its category already says live): 5 nodes, the categories are unchanged.
* 2026-10-25: a date or time editor (old role `Unknown`, now `TextBox`) is a form field and no
  longer carries the `ERROR` flag of a non-field container (1 node), and 18 nodes become
  quick-navigation targets: the segments of Chromium date, time, month and week inputs
  (edit, spin button), ARIA options exported as fillers (list item) and a canvas fallback (graphic).
* 2026-10-25: `meter` stays without an object marker (it never had one); the model role `Meter`
  is not in the marker roles.

#### M22c part 2: the secret mark (2026-10-25, security review)

A toolkit password entry stays secret when the fusion settles on another role (an author token or a
quirk turning it into a text box). `SemFlags::SECRET` (1<<21, code 2097152, appended to the codes
snapshot) is set by the normaliser for the engine's password role and travels in the fused entry;
`gorca_present::is_secret_entry` and `gorca_model::is_secret_node` ask the role *or* the mark, and
`NodeSnapshot::from_fused` turns a secret entry into a password-entry snapshot, so
`is_sensitive()` holds. Every browse sink asks `selmirror::node_is_secret` (selection mirror,
focus snapshot); the mirror's text of a secret field is the source's bullets, which browse speaks
as before ("7 bullet"), and the value never enters a snapshot. Test: `a_toolkit_password_entry_with_a_fused_text_box_role_is_secret_in_every_browse_sink`.

The mark is sticky and only a source can set it (second security finding): each source's register
set (`SrcRegs`) keeps a monotone `secret` bit set by any write whose flags carry `SECRET`, never
cleared by a later write, an `Absent` flags group or an older write (the join stays a function of
the set of writes). `fuse_node` removes `SECRET` from the flags the strata made and sets it again
exactly when a source said so, after the patch, user-override, fact and repair steps; so a higher
stratum, a patch that takes the role away, a plugin or extension fact with flags 0 cannot clear it,
and a fact that sets it on a node no source marked is ignored. Tests:
`the_secret_mark_survives_every_overwrite_and_only_a_source_sets_it` and
`the_secret_mark_join_does_not_depend_on_the_order_of_writes` (`gorca-fuse`). No Newton connector
exists in this tree; when one is added its record must set `SECRET` for a password role.

* 2026-10-25 (security review): quick navigation to a password field no longer speaks the
  mirror's text under it (Chromium "7 bullet", Firefox seven bullet glyphs): a secret node's
  description is its name and role only, whatever text the source left under it, and line reading
  and say-all skip the text under a secret field the same way (`browse.*.tsv`: 32 lines per
  language; `quicknav.*.tsv`: 2 records per language). The browse test uses plain text ("hunter2") under the field, so
  it checks the redaction itself and not the source's masking.

Audit of the part-2 diff (8c615c5c to the tip) for removed secret checks: the three
`is_secret_node` calls in `browse/` became `node_is_secret` (role, entry mark, or role without an
entry); `NodeSnapshot::is_sensitive`, `is_secret_node` and `is_secret_entry` gained the mark; the
text layer still asks `is_sensitive`; every `Role::PasswordEntry` that left a role list is
`SemRole::PasswordBox` in its replacement (text role, dialog-read skip, object marker, category
`EDIT`, pointer class, description request, label planner), and no test fixture lost its password
case.

Review fixes (reviewer verdict MERGE-WITH-FIXES, 2026-10-25): line reading skips the text under a
secret field; `compose` decides the IR's secrecy with `is_secret_entry` (the xtask secret-rule
exemption for `compose.rs` now covers the definition of `is_secret_role` only); the plugin watch
rows of both fuse bridges ask the record's mark too (`doc_actor::fuse::rec_is_secret`); where-am-I
skips an unnamed container the catalog has no role word for. Audit tests un-ignored:
`audit_line_reading_never_speaks_the_text_under_a_secret_field`,
`audit_a_marked_text_box_value_is_secret_and_redacted`.

Secret text on every speech path (follow-up, 2026-10-25): `speakable::text_in` is the one reader of
document text for speech; it masks (with bullets, same length) the text of a secret node or of a
node under one. Character, word, line, say-all, selection, find and the live-region snapshot
read through it, and rule 12 bans `.text_in(` in `browse/` and `doc_actor/live.rs` (`raw-text-in`).
Test: `reading_a_character_of_a_secret_field_never_speaks_a_plaintext_char`.

