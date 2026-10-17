# ARIA semantics: one semantic model and one presentation pipeline

Date: 2026-10-18. Author role: architect. Status: design consultation (read-only), feeds the draft
[ADR-0022](../adr/0022-semantic-model-and-presentation-pipeline.md).

Inputs: `crates/gorca-core/src/aria.rs` (M8b), `crates/gorca-core/src/browse/present.rs`,
`crates/gorca-core/src/present/utterance.rs`, `crates/gorca-core/src/present/flow.rs`,
`crates/gorca-core/src/browse/commands.rs`, `crates/gorca-model/src/{role,node,doc}.rs`,
`crates/gorca-doc-source-api/src/web.rs`, `crates/gorca-doc/src/web/{cats,doc}.rs`, ADR-0003 §13,
ADR-0004, ADR-0008 §3.2, ADR-0016 plan (`docs/plan/phase-3-features.md` §3.8), ADR-0019.
Parallel work this report must be reconciled with: `docs/reports/aria-coverage-audit.md` (web-a11y
engineer, coverage per role/state) and the math researcher's test-coverage design.

---

## 1. What exists today, and why the role=button bug is structural

### 1.1 Four presenters, one helper module

| Path | Code | Input | Notes |
|---|---|---|---|
| Focus, web | `present/utterance.rs::present_web` | `NodeSnapshot` + web attrs overlaid from the mirror (`browse/commands.rs::web_overlay`) | name, role, states, current, value, description, level, error |
| Focus, native | `present/utterance.rs::present_with` | `NodeSnapshot` | its own `STATE_WORDS` table and `mixed_word`; ignores posinset/setsize/level that GTK4 exports |
| State change | `present/utterance.rs::present_change` | old/new snapshot diff (`changed`) | third rule set; only the focused node (`app_actor.rs` ~l.950, `flow.rs::on_state_changed`) |
| Browse line / quick nav / say all | `browse/present.rs::{describe_from, describe_node_from, enter_items, marker_items, container_items, leaving_items}` | `WebDoc` `NodeView` | its own `announced()` role list, own name-from-author rule (`takes_authored_name`), own container rules |
| Focus context in browse | `browse/commands.rs` dialog lead, list-box lead, `pending_tail` | `WebDoc` | ad-hoc per container kind |

`aria.rs` centralises *words* (role word overrides, `state_words`, `current_word`, `popup_word`,
`level_phrase`, `error_text`), but each presenter still decides *which* fields to say, in *which*
order, for *which* roles, how names relate to content, and how containers are entered. Three
lists of "announced roles" exist (`browse/present.rs::announced`, `announced_node`,
`is_named_control`, plus `marker_items::named`), and two "unavailable" rules (one in
`push_states`, one in `present_web` via `never_disabled`).

The semantic facts are also *not modelled*: ARIA arrives as interned attribute strings
(`web_attrs::XML_ROLES`, `CURRENT`, `HASPOPUP`, `EXPLICIT_NAME`, ...) and is re-parsed at
presentation time (`Node::kind()` splits `xml-roles` on every call). Mutually exclusive states are
bit pairs (`EXPANDED|COLLAPSED`, `CHECKED|MIXED`, `PRESSED|CHECKED` for toggles) that every
presenter disambiguates on its own. Native apps get none of this: GTK4 exports `level`,
`posinset`, `setsize`, `placeholder-text`, `keyshortcuts`, `valuetext`
(`refs/gtk/gtk/a11y/gtkatspicontext.c:540-650`), and gorca drops them.

### 1.2 The known bug (role=button + aria-label + icon content)

Symptoms: skipped by browse arrows; role spoken twice on Tab. Two independent causes are likely,
both consequences of §1.1 (to be confirmed by a failing test first, see §7.1):

- **H1, no flat-text anchor.** A node is reachable by line/character moves only if it contributes
  at least one scalar to the flat text. Controls with no text get one only when the engine puts a
  U+FFFC for them in the parent's hypertext *and* they are leaves (`marker_items` keys on
  `v.text == Some("\u{FFFC}")`). A `<button aria-label="Close"><svg aria-hidden="true">` has an
  empty subtree text (Firefox) or an image child (Chromium), so either nothing lands on the button
  (`cursor::is_blank` sees an empty line and skips) or the line describes the child graphic and
  `authored_name_items` never runs on the button.
- **H2, two owners of the role word.** On Tab, the focus utterance (`present_web`) says
  "Close button", and a browse-side addition (`pending_tail`/lead, or `web_focus_fallback`'s
  `describe_node` which runs `enter_items` + `authored_name_items`) contributes "button Close" again;
  nothing dedupes across the two producers, because dedupe (`dedupe_names`) works on one item list.

The structural fix is the same for both: (a) a mirror invariant that every *atomic presentable*
node owns exactly one anchor scalar, and (b) one composer that owns every field of a node in one
utterance, with a single dedupe pass.

---

## 2. Prior art and its failures

| System | What it does | Lesson |
|---|---|---|
| NVDA | One property-to-speech function for every output reason: `speech.getPropertiesSpeech` (`refs/nvda/source/speech/speech.py:2013`), `getControlFieldSpeech` (:2288) for container enter/exit in documents; states filtered per role by `controlTypes/processAndLabelStates.py` (`_processPositiveStates` :14, `_processNegativeStates` :97); `OutputReason` (FOCUS, CHANGE, QUICKNAV, CARET, ...) selects variants | **Copy:** one composer keyed by reason. **Avoid:** Python dict of properties, re-fetched per call; reasons leak into many `if` branches |
| NVDA state change | `NVDAObjects/__init__.py:1341 event_stateChange`: speaks for the focus *and focus ancestors* (#10890); `speakObjectProperties` keeps a per-object property cache and speaks only the diff, negative states included (`speech.py:770-790`) | **Copy:** ancestors + diff against last *spoken* values |
| NVDA context | `api.getFocusDifferenceLevel` (`api.py:197`) speaks only new ancestors on focus change; `getControlFieldSpeech` in browse mode; `roleText` honours `aria-roledescription` (`NVDAObjects/IAccessible/ia2Web.py:194`); `script_reportDetailsSummary` (`globalCommands.py:2891`) | One ancestry-diff algorithm, but separate implementations for focus and virtual buffer |
| Orca | Generators per role with formatting lists (`refs/orca/src/orca/generator.py:1073-1289` `_generate_state_*`, `speech_generator.py:1122 _generate_ancestors`, `:984 _generate_leaving`); state changes gated by last-known-state cache and locus-of-focus-or-ancestor (`ax_utilities_event.py:1152 is_presentable_expanded_change`) | **Avoid:** synchronous AT-SPI calls inside generators (every word can block); duplicated web/default script logic (`scripts/web/script.py:1030,1247` re-implement `default.py:396,572`) — the exact split gorca has between focus and browse |
| AccessKit | Typed properties: `Toggled{False,True,Mixed}`, `Expanded: Option<bool>`, `Selected: Option<bool>`, `Invalid{True,Grammar,Spelling}`, `AriaCurrent`, `HasPopup`, `Live`, `Level`, `PositionInSet`, `SizeOfSet`, `RoleDescription`, `StateDescription`, `Details`, `ErrorMessage` (`refs/accesskit/accesskit/src/lib.rs:465-605, 1924-2499`) | **Copy:** tri-states as enums, not bit pairs. The Newton connector then maps 1:1 |
| VoiceOver | Container "interaction" model (VO+Shift+Down) instead of ancestry announcements | Not adopted (NVDA-parity goal), but the "container context" must be cheap to repeat on demand (gorca "where am I") |

---

## 3. Decision part 1: the semantic model (`gorca-model::sem`)

### 3.1 Shape

A normalised, owned, mostly-`Copy` value per node, produced by connectors, stored in the
snapshot and in the document mirror, read by every presenter and by extensions. ARIA 1.2 is the
vocabulary (it is a superset target for GTK4 `GtkAccessibleRole`, AccessKit `Role` and Qt), so
native and web share it.

```rust
// crates/gorca-model/src/sem.rs  (sketch)

/// Fine-grained role, ARIA 1.2 + graphics-aria + the DPUB subset NVDA speaks, plus native-only
/// roles (Terminal, Canvas, ...). Closed enum in Rust; an open numeric code at the WIT boundary
/// (ADR-0019 V-3).
#[repr(u8)]
pub enum SemRole { Unknown, Generic, Alert, AlertDialog, Application, Article, Banner, Blockquote,
    Button, Caption, Cell, Checkbox, Code, ColumnHeader, Combobox, Complementary, ContentInfo,
    Definition, Deletion, Dialog, Document, DocumentWeb, Feed, Figure, Form, Grid, GridCell, Group,
    Heading, Img, Insertion, Link, List, Listbox, ListItem, Log, Main, Mark, Marquee, Math, Menu,
    Menubar, MenuItem, MenuItemCheckbox, MenuItemRadio, Meter, Navigation, Note, Option,
    Paragraph, ProgressBar, Radio, RadioGroup, Region, Row, RowGroup, RowHeader, Scrollbar, Search,
    SearchBox, Separator, Slider, SpinButton, Status, Switch, Tab, Table, TabList, TabPanel, Term,
    TextBox, PasswordBox, Timer, Toolbar, Tooltip, Tree, TreeGrid, TreeItem, Terminal, Window,
    Popover, StaticText /* ... append-only */ }

impl SemRole {
    /// Total function: the coarse `Role` the rest of the core already uses (SM-1).
    pub const fn coarse(self) -> Role { /* exhaustive match */ }
    pub const fn class(self) -> RoleClass { /* exhaustive match */ }
}

/// What the presentation rules key on; one row per class in the rule table (§4.3).
pub enum RoleClass { Landmark, Container(ContainerKind), Widget(WidgetKind), Structure, Text, Atomic }

pub enum Check   { NotCheckable, Off, On, Mixed }          // checkbox, radio, menuitemcheckbox, switch, option[aria-checked]
pub enum Press   { NotToggle, Off, On, Mixed }             // aria-pressed
pub enum Expand  { NotExpandable, Collapsed, Expanded }
pub enum Select  { NotSelectable, Off, On }
pub enum Current { No, True, Page, Step, Location, Date, Time }
pub enum Invalid { No, True, Grammar, Spelling }
pub enum Popup   { No, Menu, Listbox, Tree, Grid, Dialog }
pub enum Avail   { Enabled, Disabled, NotApplicable }      // static content: NotApplicable, never "unavailable"
pub enum NameFrom { Content, Author, Placeholder, None }

bitflags! { pub struct SemFlags: u32 {
    const REQUIRED; const READ_ONLY; const BUSY; const MODAL; const MULTISELECTABLE; const MULTILINE;
    const VISITED; const FOCUSABLE; const EDITABLE; const HAS_DETAILS; const HAS_ERROR_MESSAGE;
    const ACTIVE_DESCENDANT_HOST; const ATOMIC_PRESENTATION; const LAYOUT_TABLE; const SELECTION_FOLLOWS_FOCUS;
} }

pub struct SetPos  { pub pos: u32, pub size: Option<u32>, pub src: PosSrc }   // size None = unknown (aria-setsize=-1)
pub enum   PosSrc  { Author, Engine, Computed }
pub struct GridPos { pub row: u32, pub col: u32, pub row_span: u16, pub col_span: u16 }
pub struct Live    { pub politeness: Politeness, pub atomic: bool, pub relevant: Relevant, pub root: bool }

/// Rare strings, shared (one Arc per node that has any).
pub struct SemText {
    pub role_description: Option<Arc<str>>,   // aria-roledescription, capped 64 scalars
    pub error_text: Option<Arc<str>>,         // aria-errormessage target text
    pub placeholder: Option<Arc<str>>,
    pub key_shortcuts: Option<Arc<str>>,
    pub value_text: Option<Arc<str>>,         // aria-valuetext / GTK valuetext
    pub details_target: Option<NodeRef>,      // aria-details (first target), read on demand
}

pub struct Semantics {
    pub role: SemRole,
    pub check: Check, pub press: Press, pub expand: Expand, pub select: Select,
    pub current: Current, pub invalid: Invalid, pub popup: Popup, pub avail: Avail,
    pub name_from: NameFrom,
    pub flags: SemFlags,
    pub level: u8,                       // 0 = none
    pub pos: Option<SetPos>,
    pub grid: Option<GridPos>,
    pub orientation: Orientation, pub sort: Sort,
    pub live: Option<Live>,
    pub text: Option<Arc<SemText>>,
    pub provenance: Provenance,          // Connector | Quirk(id) | Patch(addon id) — debug/log only
}
```

Size target: at most 64 bytes inline (one `Arc` for rare strings). 100k-node page: +6.4 MB, within
the "speed beats frugality" budget.

### 3.2 Invariants

- **SM-1** `snapshot.role == sem.role.coarse()`; the coarse `Role` is derived, never set
  independently. `Cat` (navigation categories) is a pure function of `Semantics` (extends DM-4).
- **SM-2** Mutually exclusive states are unrepresentable: no code path ever sees
  expanded-and-collapsed or checked-and-mixed. Connectors resolve conflicts once
  (rule: `Mixed` > `On` > `Off`; `Expanded` wins over `Collapsed`, logged as a quirk hit).
- **SM-3** Presentation never parses strings: no `xml-roles` split, no attribute lookup by key in
  `gorca-present`. `AttrMap` stays for raw/unknown attributes, logs and extension selectors.
- **SM-4** Semantics are produced by exactly one normaliser per connector family, shared by every
  consumer of that connector (AT-SPI app connector *and* AT-SPI doc source).
- **SM-5** `Avail::Disabled` is only set on roles that can be disabled; static content is
  `NotApplicable` (removes the two divergent "unavailable" heuristics).
- **SM-6** Positions are never computed by IPC on the presentation path: `PosSrc::Computed` comes
  from the mirror (web) or from children already cached (native); otherwise `pos = None`.
- **SM-7** `Semantics` is immutable per snapshot; any change is a new snapshot/mirror revision with
  a bumped `sem_rev` (cache key, §5.3).

### 3.3 Normalisers (connector side; the core never imports their types)

| Connector | Where | Input | Notes |
|---|---|---|---|
| AT-SPI (app + doc source) | new pure crate `gorca-sem-atspi` (no tokio; depends on `gorca-model` only), used by `gorca-connector-atspi` and `gorca-doc-source-atspi` | raw AT-SPI role, state bits, `{ss}` attributes (`xml-roles`, `level`, `posinset`, `setsize`, `current`, `haspopup`, `roledescription`, `placeholder-text`, `valuetext`, `keyshortcuts`, `container-*`, `live`, `atomic`, `busy`), relations (`details`, `error-message`, `labelled-by`), toolkit id | Table-driven: `(raw_role, xml-role token) -> SemRole` per Core-AAM/HTML-AAM; GTK4 attribute names come from `gtkatspicontext.c`; Qt and LibreOffice rows added later (data rows, no core change) |
| Newton / AccessKit (M19) | the connector crate | `accesskit::Node` | 1:1 for tri-states (`Toggled`, `Expanded`, `Selected`, `Invalid`, `AriaCurrent`, `HasPopup`, `Live`, `Level`, `PositionInSet`, `SizeOfSet`, `RoleDescription`, `Details`, `ErrorMessage`); a mapping-table test runs both normalisers against the same fixture set |

**WebQuirks (ADR-0008 §3.2) plug in before the normaliser**, as new closed `QuirkAction` variants
over the raw record (e.g. `FixXmlRole`, `DropStaleSelected`, `SynthesizeSetPos`), still evaluated
once per app profile, still `disabled_quirks`-switchable. Engine knowledge never reaches
`Semantics` consumers (keeps the M8b promise in `aria.rs` header).

### 3.4 Extension patches (ADR-0019 PA-1)

Node patches (`rename`, `re-role`, `label-by`, `hide`, `flatten`, `live`, `category`) become
**typed edits of `Semantics` + name**, not attribute writes:

```text
patch "github-icon-buttons" {
  select  role=button name="" has-descendant(img[name])        # selector over Semantics + attrs
  set     name = descendant(img).name
  set     name_from = author
}
```

- Applied by the core at mirror-apply time (doc actor) and at snapshot-commit time (app mirror),
  producing the *effective* `Semantics`; provenance records the add-on id. Cost: selector match is
  incremental (only upserted nodes), bounded per batch (ADR-0019 I-6 budgets).
- Inline `present` hooks (ADR-0019 PA-4) receive the composed **presentation IR** (§4.2), not
  strings, and may insert/drop/reorder fields under the 3 ms aggregate cap; fail-open to the core IR.
- Consequence for ADR-0019 C-1 (node records vs resource): the WIT `node` record must carry the
  `Semantics` fields as open codes (V-3) *before* 0.9 freezes; otherwise adding them later is a
  major bump. **This is the main ordering constraint of this design (§7.2).**

---

## 4. Decision part 2: one presentation pipeline (`gorca-present`)

### 4.1 Stages

```text
 facts ──► compose(reason, ctx, report) ──► PresIR (typed fields) ──► dedupe ──► render
 (Semantics, name,                                                  │           ├─ speech (Segments, catalog words)
  value, content ref)                                               │           ├─ braille (M10, abbreviations)
                                                                    │           └─ log / test transcript
                                                       ext present hooks (ADR-0019)
```

New pure crate `gorca-present` (no tokio; depends on `gorca-model`, `gorca-i18n`): compose and
render are synchronous, allocation-light, unit-testable without a reader. `gorca-core` keeps the
*when* (focus lane, browse commands, change detection); `gorca-present` owns the *what*.

### 4.2 Interfaces (sketch)

```rust
pub enum Reason {
    Focus,                          // focus landed here
    FocusAncestor,                  // newly entered ancestor of the focus (NVDA FOCUSENTERED)
    BrowseLine { entered: bool },   // node met inside a line; entered = its content starts in the range
    QuickNav,                       // target of H, B, F, ... (LP-4)
    SayAll,
    Review,                         // object/flat review (M21)
    Change(SemDiff),                // state/name/value change (§5)
    ListEntry,                      // elements list / chooser row (M15)
    WhereAmI,                       // context on demand
}

pub struct Facts<'a> {
    pub sem: &'a Semantics,
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub value: Option<&'a Value>,
    pub content: Content<'a>,       // None | Text(&str) | Range(u32..u32) in the flat text
}

pub enum Field<'a> {
    Name(&'a str), Role(RoleWord), State(StateWord), Value(Cow<'a, str>),
    Position { pos: u32, size: Option<u32> }, Level(u8), Current(Current),
    Description(&'a str), Error(&'a str), HasDetails, Shortcut(&'a str),
    ContainerEnter { kind: ContainerKind, name: Option<&'a str>, size: Option<u32>, dims: Option<(u32, u32)> },
    ContainerLeave(ContainerKind),
    Content(Content<'a>),
}

pub struct PresIr<'a> { pub node: NodeRef, pub fields: SmallVec<[Field<'a>; 8]> }

pub fn compose<'a>(f: &Facts<'a>, r: Reason, ctx: &Ctx, report: ReportFlags) -> PresIr<'a>;
pub fn render_speech(ir: &[PresIr<'_>], lang: &Catalog, o: &VerbalizeOpts) -> SmallVec<[Segment; 8]>;
```

`RoleWord`/`StateWord` are catalog ids (`gorca_i18n::MsgId`), so render is the only place that
produces text (keeps the M12a `cargo xtask strings` gate meaningful).

### 4.3 Rule table, not branches

One table `RULES: [[FieldOrder; Reason::N]; RoleClass::N]` (data, generated from a reviewed TOML
or a `const` array) says, per role class and reason, which fields appear and in which order, and
the name/content relation:

| Class | Focus | BrowseLine entered | QuickNav | Change |
|---|---|---|---|---|
| Atomic widget (button, link, img, switch, tab) | name, role, states, current, value, pos, desc, error | role, states, name-or-content | name-or-content, role, states | diff only |
| Text field | name, role, states, value/"blank", placeholder, desc, error | role, states, content | name, role, states, value | diff, value |
| Container (list, tree, grid, tablist, menu, toolbar, radiogroup, named group) | via FocusAncestor: name, role, size | ContainerEnter/Leave | ContainerEnter + first line | expanded/selected diff |
| Landmark / region | FocusAncestor: name, landmark | ContainerEnter/Leave | name, landmark | — |
| Heading | name, "heading level n" | role+level, content | role+level, content | — |

- **Name vs content (one rule).** `name_from == Author` on a role whose ARIA "name from content"
  is allowed (NVDA `nameIsContent`: link, button, toggle, menu items, heading, tab) replaces the
  content; otherwise content is spoken and the name is spoken only if not contained in it (the
  existing `dedupe_names` behaviour, now in one place).
- **Per-language order** (e.g. "кнопка Закрыть" vs "Close button") is a catalog-level override of
  the field order of a class, not code.
- **roledescription** replaces the role word but never removes state/position words (NVDA
  `roleText`); empty or >64 scalars is ignored.

### 4.4 Dedupe invariants (fix class for the bug)

- **PR-1** One utterance contains at most one `Role` field per node and at most one `Name` field
  per node, whatever number of producers contributed (focus utterance + browse tail + leads). The
  composer receives *all* contributions for one output event as one `Vec<PresIr>` and dedupes by
  `(node, field kind)`.
- **PR-2** A `ContainerEnter` for node N is emitted at most once per cursor/focus move, by the
  context tracker (§5.4), never by the node composer.
- **PR-3** Same facts + same reason ⇒ same fields on every surface. Cross-surface differences are
  allowed only where the rule table says so (golden test §8, M-1).

### 4.5 Mirror anchor invariant (fix for H1)

- **MI-1** Every node whose class is Atomic widget (button, link, image, switch, check box, radio,
  slider, progress bar, menu item, tab, tree item) and that is not hidden owns **at least one**
  flat-text scalar. If its subtree contributes no visible text, the mirror synthesises one U+FFFC
  anchor for it (descendants that are presentational, `aria-hidden`, or decorative images are not
  counted). A node with an author name and an icon-only subtree is therefore *one* marker: it is
  landed on by arrows, spoken by `compose(BrowseLine)` with its author name, and its icon child is
  not spoken separately.
- Implementation home: `gorca-doc` flat builder (engine-neutral), not the AT-SPI source; the source
  only reports `name_from` and children.

### 4.6 What gets deleted

`browse/present.rs::{announced, announced_node, role_phrase, push_states, enter_items, marker_items,
container_items, takes_authored_name, is_named_control}`, `present/utterance.rs::{present_web,
present_with, present_change, STATE_WORDS, mixed_word}`, the dialog and list-box leads in
`browse/commands.rs`, and most of `aria.rs` (its tables move into `gorca-present` rule data; the
NVDA tests move with them). Net effect: one place to fix wording, one place to test.

---

## 5. Decision part 3: changes, context and verbosity

### 5.1 Which changes are spoken (ST rules)

| Rule | Nodes | Fields | Source of truth |
|---|---|---|---|
| **ST-1** | the focus | any `SemDiff` field except those the reason implies (selection-follows-focus `select`) | NVDA `event_stateChange`, Orca `is_presentable_*_change` |
| **ST-2** | focus ancestors (≤ 8 levels) | `expand`, `select`, `check`, `press`, `sort`, `busy` | NVDA #10890 (`NVDAObjects/__init__.py:1341`) |
| **ST-3** | the active descendant of an `ACTIVE_DESCENDANT_HOST` focus (tree, grid, listbox, combobox popup) | as ST-1 | ARIA activedescendant = virtual focus |
| **ST-4** | the node gorca itself activated (Enter/Space in browse mode, `DocAction::Click`) during 2 s after the action | `expand`, `check`, `press`, `select`, `current` | new (NVDA gets it only because activation focuses); fixes silent disclosure buttons in browse mode |
| **ST-5** | the browse cursor node (browse mode, focus elsewhere) | `expand`, `check` only, `report.state_changes_at_cursor` (default off) | gorca extension of NVDA |
| **ST-6** | any other node | nothing spoken; mirror updated | Orca / NVDA |

Today only ST-1 exists (`app_actor.rs` filter `self.focus == Some(*node)`). ST-2/3 need the app
actor to route `PropertiesChanged` for focus ancestors and the active descendant; ST-4/5 need the
doc actor to tag nodes. The routing check is a bounded set lookup (≤ 10 node keys per app).

### 5.2 Diff, coalescing, priority

- **Diff against last spoken, not last seen.** Each surface keeps `LastSpoken { node, sem_rev,
  sem }` (one per channel, as `Presented` already is). `SemDiff` is computed field-by-field on the
  typed `Semantics` (cheap, exhaustive). A flip-flop between two reads (collapsed→expanded→collapsed
  within the settle) produces an empty diff and no speech (Orca's last-known cache, generalised).
- **No settle delay on the user path.** A change caused by the user's key (ST-1/ST-4 within
  300 ms of a key) is spoken at the first reply, with `may_refine`; later diffs of the same node
  within 250 ms *replace* (`refines`) instead of queueing. Keeps the 15 ms gorca-side budget.
- **Priority.** Change of the focus: `Prio::Normal`, cancels only earlier change utterances of the
  same node (cancel key = `(node, Change)`); never cancels the focus utterance it follows. Focus
  change cancels pending changes (epoch rule, unchanged).
- **Rate limit.** Per app: 8 change utterances/s, excess coalesced per node to the latest
  (token bucket, same shape as ADR-0019 §2.4). A page toggling `aria-pressed` in a loop on the
  focused node cannot flood speech.
- **Ingress.** Unchanged ADR-0004 coalescing: `state-changed:*` for the same `(node, state)` keep
  the latest; the doc source already owns the dirty set (browse-mode D1).

### 5.3 aria-busy and live regions

- `busy` subtree: mirror applies mutations (navigable), but ST-2..5 and live-region announcements
  inside are suppressed; on `busy → false` the live-region path (ADR-0003 §13, LR-1..7) presents the
  region once (atomic or additions per `relevant`). Bound: a region busy for > 10 s is treated as
  not busy (hostile/forgotten flag), logged.
- Live region text uses `compose(Reason::Change)` only for role words of *focusable* inserted
  content (NVDA says just text); politeness → `Prio` mapping stays ADR-0003.
- `role=alert`/`alertdialog` appearance: `ContainerEnter` with `Prio::Interrupt` within LR rules.

### 5.4 Container context (one tracker for focus, browse, review)

`ContextTracker` per channel holds the chain of **context-bearing ancestors** (classes Container,
Landmark, Dialog, Article, Figure, Form, Table/Grid) of the last presented position. On each move:
`diff(old_chain, new_chain)` → `ContainerLeave` (innermost first, browse only — NVDA does not say
"out of" on focus) then `ContainerEnter` (outermost first). Fields:

- lists/listboxes/trees/menus: name, kind, item count (`setsize` of the first item, else children
  count from the mirror; never IPC);
- tables/grids: name, "table with C columns, R rows" (`aria-colcount/rowcount` win);
- tree items: "level n", "k of m" (from `SetPos`), spoken on the item, and a level change spoken
  as "level n" only when it changed (NVDA);
- tab list / tab: "tab control", "tab k of m";
- landmarks: name + landmark word (existing catalog ids).

Focus uses it with `Reason::FocusAncestor` (replaces dialog/list-box leads and NVDA's
`getFocusDifferenceLevel`); browse uses it for line/quick-nav moves (replaces `leaving_items` +
`container_items`); review (M21) and braille (M10) get it for free.

### 5.5 Verbosity

Report flags from ADR-0016 §3.8 (`ReportFlags: u64`, compiled once per effective profile) gate
fields in `compose`; new bits proposed: `position_info`, `levels`, `landmarks`, `containers.lists`,
`containers.tables`, `containers.groups`, `role_description`, `details`, `current`,
`state_changes`, `state_changes_at_cursor`, `negative_states` ("not checked", "not selected").
`verbosity.level` presets:

| Field | brief | normal (NVDA default) | verbose |
|---|---|---|---|
| role words | abbreviated catalog form | full | full |
| position "k of m" | off | on | on |
| containers entered | landmarks, tables | + lists, tab controls, trees, named groups | + unnamed groups, row groups |
| descriptions | off | on | on |
| negative states | checkable only | as NVDA | all |
| has details / shortcuts | off | details only | both |

Vendor profiles (data-only extensions) may set these keys per app/site (PF rules unchanged).

---

## 6. Performance

- **Never block.** `compose` and `render` are pure functions over in-memory data. Everything that
  needs IPC (details text, `error-message` target, native sibling counts) is fetched at focus-fetch
  time under the existing deadline (new `PropMask::SEM` = role+states+attrs+relations subset) or
  omitted (`SM-6`); `HasDetails` is spoken as a flag and the text read by a command
  (NVDA `script_reportDetailsSummary`).
- **Normalise once.** String parsing moves from every presentation to one normalisation per
  upsert/fetch. Browse lines over 100k-node pages stop splitting `xml-roles` per node per line.
- **Storms.** ST-6 nodes cost a mirror write and nothing else. The ST routing set check is O(1).
  10k `state-changed` events/s on non-focus nodes must produce 0 utterances and stay within the
  existing ingress budget (ADR-0004 gates).
- **Cache.** Two levels, both keyed by `(node, sem_rev, name_rev, report_epoch, lang_epoch)`:
  (1) per-node composed field list for `BrowseLine`/`QuickNav` in the doc actor's published
  snapshot side table (invalidated by the mirror's existing `sig`), (2) none for focus (one node per
  event, cheaper to recompute). Build cache (1) only if bench B-SEM-2 misses its gate without it.
- **Memory.** ≤ 64 B inline `Semantics` + rare `Arc<SemText>`.

---

## 7. Phasing

### 7.1 Phase 2 polish (now): P2-ARIA-0, small, no new crates

1. Failing tests first: fixture page with icon-only buttons (`svg aria-hidden`, `img alt=""`,
   icon font `<i>`, CSS background, `<svg><title>`), authored `aria-label`, in Firefox and Chromium;
   assert arrows land on each button and Tab speaks "Close button" exactly once.
2. MI-1 in `gorca-doc` (synthetic anchor for content-less atomic nodes; `aria-hidden`/decorative
   children not counted).
3. PR-1 dedupe across the focus utterance and `pending_tail`/leads (one item list merged before
   render); keep the existing presenters.
4. A cross-surface consistency test (M-1 below) over the existing M8 fixture pages, run in CI: it
   catches the next instance of this bug class before the refactor.

Cost: ~2–3 days rust-engineer + web-a11y-engineer; reviewer pass.

### 7.2 Phase 3: new milestone M22 "Semantic model and unified presentation" (ADR-0022)

| Slice | Content | Depends on | Must land before |
|---|---|---|---|
| **M22a** model + normalisers | `gorca-model::sem`, `gorca-sem-atspi` (web + GTK4 rows), mirror/snapshot carry `Semantics`, SM-1..7, mapping golden tests; AccessKit table spec for M19 | — | **M18a WIT 0.9 node record** (ADR-0019 C-1) |
| **M22b** pipeline | `gorca-present` compose/render, rule table, migrate focus/browse/quick-nav/say-all/change, delete §4.6 | M22a | M10 braille, M15 elements list, M21 review |
| **M22c** changes + context | ST-1..6, ContextTracker for focus/browse, busy suppression, rate limits | M22b | — |
| **M22d** verbosity + native parity | ReportFlags gating, presets, GTK4 posinset/setsize/level/placeholder/shortcuts in native focus; Qt/LibreOffice rows later | M22b, M14a | — |

Folding: not into M8 (closed in Phase 2) or M21 (review completeness is a consumer, not the owner);
M15 and M10 consume M22b. ADR number: **0022** (0009–0017 planned, 0018/0019/0021 written, 0020
reserved for the store).

---

## 8. Metrics to validate

| Id | Metric | Gate |
|---|---|---|
| M-1 | Cross-surface consistency: for every node of the fixture corpus, the role word, state words and name emitted by Focus, BrowseLine(entered), QuickNav, Review differ only where the rule table allows | 0 unexplained differences |
| M-2 | NVDA parity corpus: WAI-ARIA APG patterns (≈30: accordion, checkbox mixed, combobox, dialog, disclosure, grid, listbox, menu/menubar, menu button, radio group, slider, spin button, switch, tabs, toolbar, tree, treegrid, feed, landmarks, breadcrumb, link, table, alert, carousel) with expected transcripts derived from NVDA rules (`speech.py`, `processAndLabelStates.py`) | ≥ 95 % token agreement on P0 patterns, every disagreement listed as intended or bug |
| M-3 | Native parity: GTK4 fixture (list view with posinset/setsize, tree expander, switch, check button mixed, level) | role/state/position words equal to the web equivalent |
| M-4 | `compose` cost | p99 ≤ 3 µs per node (B-SEM-1, release, quiet host) |
| M-5 | Browse line presentation | p99 ≤ 60 µs per line on a 100k-node page (B-SEM-2); no regression vs today's LP-3 numbers |
| M-6 | Focus keypress → speech gorca-side | Δp99 ≤ 0.1 ms vs before M22b |
| M-7 | State-change storm: 10k `state-changed` events/s on non-focus nodes | 0 utterances, reader lag p99 unchanged |
| M-8 | User-path change latency: Space on a check box → "checked" | ≤ 10 ms gorca-side p99; flip-flops within 250 ms produce ≤ 1 utterance |
| M-9 | Memory | ≤ 64 B inline per node; 100k-node page RSS increase ≤ 10 MB |
| M-10 | Mapping coverage | every ARIA 1.2 role and state in the coverage audit maps to a `SemRole`/field or is listed as intentionally unmapped |

---

## 9. Alternatives considered

| Option | Trade-off |
|---|---|
| **A. Typed `Semantics` + one composer with a rule table (recommended)** | Highest consistency and testability; a cross-crate migration (M22a/b) and a WIT impact that must be timed before M18a |
| B. Keep attribute strings; add typed accessors (façade over `AttrMap`) and share helpers | Cheapest; keeps string parsing on the hot path, keeps bit-pair ambiguity, does not give native parity or a clean WIT record; the bug class stays (presenters still own field choice) |
| C. Generic property bag (`PropId → PropValue`, AccessKit-style) | Most extensible (new ARIA property = new id, no struct change); weaker exhaustiveness (`match` over a sparse map), ~2× memory, slower compose. Viable as the *WIT* representation (open codes) with A as the core representation |
| D. Orca-style per-role generator classes / formatting strings | Familiar; leads to per-script duplication (Orca's web vs default) and is hard to keep allocation-free |
| E. Template grammar per role in data (fully declarative order + wording) | Great for localisation and vendor profiles; risks a mini-language; adopted only for field *order* (§4.3), not for logic |

Contested point: A vs C for the *core* representation. Not worth a full bake-off; a 2-day
micro-benchmark (B-SEM-1 compose cost + memory on the 100k synthetic page, both representations)
decides it, judged by the reviewer on M-4/M-5/M-9.

## 10. Risks and open questions

- The causes H1/H2 of the button bug are inferred from code reading, not reproduced; P2-ARIA-0 step 1
  confirms them.
- ADR-0019 C-1 timing: if M18a freezes the 0.9 node record before M22a, `Semantics` enters WIT only
  at 1.0 or via additive accessors (V-4), which is acceptable but uglier.
- ST-4/ST-5 (announcing changes on nodes that are not focused) are beyond NVDA; owner should confirm
  they are wanted (QA-1).
- GTK4 does not export `aria-current`, `invalid` kinds or `details` today; native parity is bounded
  by toolkits (upstream track U).
- Chromium vs Firefox differences in `name_from` exposure (`explicit-name` exists on both today?
  verify in the coverage audit) decide whether MI-1 can rely on `name_from` alone.
- Owner questions: QA-1 announce state changes at the browse cursor / after browse activation
  (ST-4 default on, ST-5 default off)? QA-2 verbosity presets as in §5.5? QA-3 honour
  `aria-roledescription` by default (NVDA does)?
