# ADR-0022: Semantic model (roles, states, properties), one fusion stage and one presentation pipeline

Status: **Proposed** (draft r2, 2026-10-18; r3 note 2026-10-19). Owner: architect.

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
  deleted in the same milestone. Order: b1 change + focus (P1, P4) → b2 browse/quick-nav/say-all
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
