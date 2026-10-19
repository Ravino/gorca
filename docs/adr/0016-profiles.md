# ADR-0016: Configuration profiles (typed layer stack, triggers, origin-layer edits)

Status: **Proposed** (r2, 2026-10-19: review `docs/reports/adr-0016-review.md` r1 ACCEPT-WITH-CHANGES,
H-1..H-3, M-1..M-7 and L-1..L-8 integrated, mapping in §8). Owner: architect.
Relates to: ADR-0005 (settings as data, explicit save, `ArcSwap<Settings>`, keymaps), ADR-0003 §21 and
`docs/design/browse-mode.md` (browse/focus machine), ADR-0019 (extension platform, data-first add-ons, I-8,
`request-mode`), ADR-0021 (`browse.selection_model`), ADR-0022 (presentation, `ReportFlags`), ADR-0024 §2.13
(profiles vs semantic model, PS-1..PS-6). Design source: `docs/plan/phase-3-features.md` §3 (this ADR supersedes
it where they differ). Milestone: **M14a** (`docs/plan/phase-3.md`), core slices in §6.

## 1. Context

gorca has one settings file (`gorca.toml`) folded over the `SETTINGS` table, plus a runtime `Overlay` of eight
fixed keys (`crates/gorca-config/src/settings.rs`: `Overlay`, `SettingDef`), published by the Reader as one
`Arc<Settings>` (`gorca-core/src/commands/state.rs` `republish`). There is no per-application variation except
code: the CHR-0 branch adds an interim per-application web-mode table
(`crates/gorca-config/src/app_modes.rs` in worktree `agent-a508eca24e2626204`: `AppModeRow { id, apps, host,
browse: BrowseDefault::{Auto, Off}, why }`, one built-in row `vscode` → `Off`, `resolve(layers, app, host)` with
"later layer wins, host row beats app row", names compared with `eq_ignore_ascii_case`, consumed once in
`doc_registry.rs` at `HubMsg::Appeared`, compiled in, so it applies in every mode).

Requirements:

- Owner DP-3 (decisions 2026-10-19): NVDA-style default (browse mode, automatic focus mode on widgets), an
  Orca-style sticky focus mode the user can toggle at any time (`browse.toggle`: gorca+Space in the NVDA
  presets, gorca+A in the Orca presets), per-app presets as **data**, add-ons contribute profile rows as
  data only.
- Owner QA-2 (2026-10-18): verbosity comes as presets in profiles.
- ADR-0024 PS-2: mode defaults are profile keys; the Reader's mode machine decides
  `mode = policy(settings.browse.*, document semantics, user toggle)`.
- Product goals: nothing on the key → speech path may cost IPC or an unbounded computation; a misbehaving
  add-on cannot silence the reader or weaken its privacy guards.

### 1.1 Prior art

| System | Model (cited) | Take | Avoid |
|---|---|---|---|
| NVDA | Base config at the bottom, triggered profiles pushed on a stack, a manual profile always on top (`refs/nvda/source/config/__init__.py:883-905` `_triggerProfileEnter`); "changed settings are written to the most recently activated profile" (l.478-480); `BASE_ONLY_SECTIONS` (l.482-496); `atomicProfileSwitch` batches trigger enter/exit (l.933-970); triggers suspended while NVDA's own dialogs are focused (`appModuleHandler.py:425-434`); triggers `app:` and `sayAll` only (`speech/sayAll.py:483`). Browse mode per app is **code**: `disableBrowseModeByDefault` (`treeInterceptorHandler.py:59-67`; `appModules/skype.py:12`, `whatsapp_root.py:13`); VS Code blocks the tree interceptor (`appModules/code.py:19-25`). Pass-through state lives per tree interceptor (`browseMode.py:772-784`). | stack, atomic switch, base-only sections, suspension during own modes, per-document toggle state | edits always into the top profile; per-app browse policy in code; no site/window/context triggers |
| Orca | Layered lookup app → current profile → default profile → schema default (`refs/orca/src/orca/gsettings_registry.py:923-985`); active app set on script change (`script_manager.py:343-347`); profiles manual only. Web mode: per-app session state `focus_mode_is_sticky`, `browse_mode_is_sticky`, `user_has_overridden_auto_sticky_focus_mode` (`document_presenter.py:97-99, 223-294`); Insert+A ×1 toggles, ×2 sticky focus, ×3 sticky browse (`document_presenter_command_definitions.py:37-62`); auto sticky focus for "likely Electron" (l.175-190) and top-level `http` web apps (l.192-208, 711-727). | per-app layer without creating a "profile"; sticky focus/browse as first-class session states; user override remembered per app | dconf storage; heuristics that flip whole apps (DP-3 rejected this as a default) |
| VoiceOver | Activities: named bundles of settings bound to apps/websites. | site and app binding | opaque storage, no layering |
| AccessKit / Newton | No settings concept; the consumer owns presentation. | `AppMatcher` reads identity facts only, never toolkit names | — |

## 2. Decision

**Typed layer stack with automatic triggers and origin-layer edits** (option A3 of `phase-3-features.md` §3.2).
All resolvers read **one published snapshot**; effective settings are precomputed per match set and swapped as one
`Arc`.

### 2.1 Layers and precedence (lowest first)

| Layer | Source | Writable | On in safe mode / under `profiles.suspend` | Who may set what |
|---|---|---|---|---|
| L0 Defaults | `SETTINGS` table | no | yes / yes | every key |
| L1 Preset | keyboard preset × layout (`presets/*.toml`) | no | yes / yes | keymap, keyboard-implied defaults |
| L2 Extension defaults | extension schema defaults | no | no / yes | only `ext.<id>.*` of that extension (ADR-0019 I-8) |
| L3 Product rows | first-party rows compiled into `gorca-config` (`include_str!("product/*.toml")`) | no (shadowed) | **yes / yes** | `vendor` keys (§2.3), each row names its apps (PF-4) |
| L4 Add-on rows | `[[profile]]` rows of installed extensions | no (shadowed) | no / **no** | `vendor` keys, non-`privacy`, confined to the extension's `app_name` list (PF-4); third-party rows **off** until enabled |
| L5 User base | `~/.config/gorca/gorca.toml` | yes | yes / yes | every key; see UB-1 |
| L6 User profiles | `~/.config/gorca/profiles/*.toml`, incl. **shadows** of L3/L4 rows | yes | no / no | every non-`Base` key, subject to the matrix (§2.3) |
| (L7 Manual profile) | a user profile activated by command | — | — | **type-only in M14a** (owner Q-P5): the `LayerRef::Manual` variant and the snapshot field exist, nothing activates it until the M15a chooser |
| Session deltas | unsaved runtime edits | — | — | **not a layer above the others**: each writable layer (L5, each L6 file) carries its own delta, folded directly on top of that layer's file values |
| Safety clamp | — | — | always | PC-1 |

Fold order: `L0, L1, L2, L3 (by specificity), L4 (by specificity), L5 (file ⊕ Δ, UB-1), L6 (by specificity,
file ⊕ Δ each), [L7], clamp`. Names, not numbers, are normative; ADR-0024 PS-5 "L3/L5/L6" means the user layers
(here L5, L6, L7) and "vendor (L4)" means L3 and L4.

- **UB-1 User choices beat add-on and product rows; generated defaults do not.** An L5 entry whose value equals the
  L0/L1 default is not an override (a full dump of the defaults, as `gorca --defaults` writes, must not cancel
  every row); it is skipped in the fold. Any L5 entry that differs from the default, and every L5 entry of a
  `privacy` key, sits above L3/L4. To keep the default value against a row, the user disables the row (PF-2) or
  overrides it in its shadow. `settings.explain_current` says which applies.
- **PM-1 Specificity** inside L3, L4 and L6: for each key the most specific profile that sets it wins. A profile's
  specificity is that of its most specific *matching* trigger: `Site > Window > App > Context`; ties: longer
  literal prefix of the best matching glob, then profile id order, with one load diagnostic naming both.

### 2.2 Triggers and matching

```rust
// gorca-config::profiles (pure: no tokio, no I/O)
pub enum Trigger {
    App     { app: AppMatcher },
    Window  { app: AppMatcher, title: Glob, kind: Option<WindowKind> }, // Normal | Dialog | Popup
    Context { ctx: FocusContext },
    Site    { app: AppMatcher, host: Glob, path: Option<Glob> },          // documents only
}
/// Identity facts only (never toolkit heuristics).
pub struct AppMatcher { pub name: Vec<Glob>, pub exe: Vec<Glob>, pub desktop_id: Vec<Glob> }
pub enum FocusContext { Terminal, Editable, Document, BrowseMode, FocusMode, Menu, Table, Dialog }
```

- **GL-1 One glob** (`gorca-config::glob`, shared by ext-host and profiles; ext-host's `glob.rs` re-exports it in
  P2, or keeps a copy if that crate is busy): `*` any run, `?` one character, `\` escapes `*`, `?`, `\`; ASCII
  case-insensitive (non-ASCII compared exactly, as CHR-0's `eq_ignore_ascii_case`); pattern ≤ 256 bytes;
  iterative O(n·m) without recursion. Moving ext-host to case-insensitive only widens its matches (all shipped
  patterns are literal lowercase or `org.gnome.*` forms); its tests gain mixed-case and non-ASCII cases.
- **TR-1 Inputs are cached, never computed per key press.** App: per `AppId` at `Appeared` from
  `AppInfo.app_name`; `exe` from an async `/proc/<pid>/exe` read (deadline 50 ms, Background class; absent = no
  match on `exe`); `desktop_id` when ShellHelper supplies it (absent = no match). Window: per (frame node, title
  generation) from the Reader's active-window record; title generations debounced 100 ms. Context: per focus
  from the focus snapshot's role class (M14a; switches to the published `SemEntry` role class, ADR-0024 PS-4,
  when M22a wave C lands) and the current `WebMode`. Site: per `DocId` from `DocMeta.url` (new; AT-SPI
  `Document.GetAttributeValue("DocURL")` once per load, Background class, 250 ms deadline; host and path only;
  the URL is never logged, PR-8).
- **TR-2 Compiled index.** `ProfileSet::compile` builds per-kind lookup tables: app matchers → profile ids
  (evaluated once per app), a `[Vec<ProfileId>; N_CONTEXT]` table, window and site glob lists. A focus change
  unions at most four cached id lists and interns the result.
- **TR-3 ProfileKey** = `(rows: MatchSetId, user: MatchSetId, manual: Option<ProfileId>)`, interned `u32`s.
- **TR-4 Suspension** (NVDA): triggers are not re-evaluated while the Reader is in its own modes (`Chooser`,
  `Prompt`, `InputHelp`); the latest focus is remembered and evaluated once on leaving.
- **TR-5 Deferred:** a `SayAll` trigger (Phase 4); add-on context tags as `ctx.tag` facts matched by `Context`
  (ADR-0024 PS-4, Phase 4).

### 2.3 Key namespace and the trigger/scope matrix

Every `SettingDef` gains metadata (schema rows, not a file-format change):

```rust
pub struct SettingDef { /* existing: section, key, ty, default, since, apply */
    pub scope: KeyScope,
    pub vendor: bool,    // L3/L4 rows may set it (presentation, mode defaults)
    pub privacy: bool,   // never settable by L3/L4 rows; an L5 value always beats rows (UB-1)
}
pub enum KeyScope {
    Base,  // L0-L2 and L5 only: never in any profile
    App,   // resolved per AppId: App triggers
    Doc,   // resolved per (AppId, DocId): App and Site triggers
    Focus, // resolved per ProfileKey: all trigger kinds
}
// SettingType gains Map(bool) for `profiles.rows` (L-8).
```

| Section | Scope | Vendor | Privacy | Notes |
|---|---|---|---|---|
| `keyboard.*`, `keymap.*`, `speech.backend`, `ui.language`, `profiles.*`, every `Apply::Restart` key | Base | no | — | NVDA `BASE_ONLY_SECTIONS`. Per-app keymaps stay in `[keymap."app:<x>".<ctx>]` (ADR-0005 §5.2). |
| `speech.rate/pitch/volume/voice/language/output_module` | Focus | **no** | no | SW-3 ordering; PC-1/PC-4 |
| `speech.punctuation`, `speech.caps*` | Focus | yes | no | |
| `echo.*` | Focus | **no** | **yes** | typing echo is a privacy choice (shoulder-surfing, shared rooms) |
| `editing.announce_clipboard` | Focus | **no** | **yes** | clipboard contents spoken |
| other `editing.*`, `text.*` timings, `verbosity.*`, `report.*` (new, compiled to `ReportFlags`, M22d) | Focus | yes | no | `editing.word` per app (decision 2026-10-17) |
| `browse.enabled`, `browse.mode_default`, `browse.auto_focus_mode`, `browse.sticky_focus`, `browse.selection_model` (ADR-0021), `browse.say_all_on_load` | **Doc** | yes | no | mode policy inputs (§2.6) |
| `browse.description_pause`, other `browse.*` presentation | Focus | yes | no | |
| `terminal.settle_ms`, `burst_lines`, `prompt_markers`, `prompt_regex` | App | yes | no | read by the terminal arm for that app, focused or not |
| `terminal.secret_regex` | App | **no** | **yes** | **additive** (TS-1) |
| `general.sleep` | App | no | yes | sleep mode persistence (`phase-3-features.md` §8 row 5) |
| `sources.*` (ADR-0024 PS-5) | App/Doc | **no** | **yes** | opt-ins never come from rows |
| `ext.<id>.*` | Focus | own namespace only | per schema | defaults at L2; see NS-3 |

- **NS-1 Matrix.** A `Context` or `Window` trigger may set only `Focus` keys; a `Site` trigger `Doc` and `Focus`
  keys; an `App` trigger any non-`Base` key. A row that breaks the matrix is a load diagnostic (key skipped,
  file kept).
- **NS-2** Unknown keys get `did_you_mean` diagnostics (`diag.rs`); a profile never fails as a whole for one key.
- **NS-3 The mode loop is closed (INV-P3).** `WebMode` depends only on `Doc` keys, document semantics, the user
  toggle and extension `request-mode` effects (M18a). `Context{BrowseMode|FocusMode}` triggers may set only core
  `Focus` keys, **never `ext.*`** (diagnostic), so a mode switch cannot change anything an extension reads. In
  addition the host honours `request-mode` only from hooks run on a document/focus event, never from a hook run
  because settings changed, at most once per document per 2 s, and never against a user toggle (MP-3). The
  dependency graph `Doc keys → WebMode → Context → Focus keys` therefore has no back edge.
- **TS-1 Secret patterns only grow.** `terminal.secret_regex` is unioned with the extension's built-in pattern
  instead of replacing it (today `extensions/ext-vte/src/prompt.rs:68-72` replaces it); the core's fail-closed
  keyword check (`gorca-core/src/terminal.rs:217`) stays. No layer can remove a default secret pattern.

### 2.4 One published snapshot, effective settings and switching latency

```rust
/// Published by the Reader (single writer) in `Arc<ArcSwap<ProfileSnapshot>>`; every resolver reads it.
pub struct ProfileSnapshot {
    pub gen: u64,                      // bumps on any field change
    pub set: Arc<ProfileSet>,          // compiled files + rows (from the config task)
    pub deltas: Arc<Deltas>,           // unsaved edits per writable layer
    pub manual: Option<ProfileId>,     // always None in M14a (Q-P5)
    pub mode: ProfileMode,             // Active | Suspended | Disabled (safe mode)
}
pub struct Effective {
    pub settings: Settings,
    pub origin: Box<[LayerRef]>,       // per KeyId: which layer supplied the value (PE-1, explain)
    pub report: ReportFlags,           // u64, compiled once (M22d consumes)
}
pub enum LayerRef { Default, Preset, ExtDefault(ExtId), Product(RowId), AddOn(RowId), UserBase, User(ProfileId), Manual(ProfileId) }

impl ProfileSnapshot {
    pub fn resolve_focus(&self, key: ProfileKey) -> Effective;
    pub fn resolve_app(&self, app: &AppFacts) -> AppScoped;              // App keys
    pub fn resolve_doc(&self, app: &AppFacts, doc: &DocFacts) -> DocScoped; // Doc keys
}
```

- **SS-1 One input (H-1).** All three resolvers are pure functions of one snapshot. `Suspended` folds only
  L0, L1, L3, L5 (+ L5 delta); `Disabled` folds L0, L1, L3, L5 (safe mode keeps product rows, M-5). Unsaved edits,
  suspend and (later) the manual profile therefore reach App and Doc keys in the doc registry and the terminal
  arm exactly as they reach the Reader. Consumers cache `AppScoped`/`DocScoped` per `AppId`/`DocId` tagged with
  `gen` and recompute lazily on a `gen` change; when they apply is §2.6 DL-1.
- **INV-P1** Effective values are a pure function of (snapshot `gen`, key or facts). **INV-P4** The Reader is the
  single writer of the snapshot and of the focus cell. **INV-P6** For the same facts, an App/Doc key from
  `resolve_app/resolve_doc` equals the value in `resolve_focus` of a key built from those facts (property test).
- **SW-1 Focus changes.** The Reader keeps `LruMap<ProfileKey, Arc<Effective>>` (64 entries, hand-rolled on
  existing dependencies; no new crate). A focus change computes the key from cached inputs (O(matched
  profiles)) and swaps the `Arc` into the existing `ArcSwap<Settings>` cell.
- **SW-1b Key presses that change context (M-1).** The claim "switching never touches the key path" is wrong for
  mode changes: `browse.toggle` (gorca+Space / gorca+A) and automatic focus mode on Tab or quick navigation change
  `WebMode`, which is a `Context` input, so the key press computes a new `ProfileKey` and may emit `SetParams`
  before the mode word. Typing and caret movement never change the key. Bound: on a prewarmed hit the key
  computation is two interned lookups (≤ 20 µs p99 incl. the swap); misses are prevented by SW-2.
- **SW-2 Prewarm.** On `Appeared`, on first focus of a document, on every focus change into a document and on every
  snapshot `gen` change, the config task folds, off the Reader, the keys reachable by one step: the focused app ×
  {no context, each context with a matching profile} and, inside a document, **both** `BrowseMode` and `FocusMode`
  keys of the focused document. It sends `SettingsMsg::Prewarm { gen, entries }`; the Reader drops entries of an
  older `gen`. A cold miss on the Reader folds inline (≤ 200 µs p99; target 5–20 µs).
- **SW-3 Voice ordering.** If the new `Effective` changes `speech.{rate,pitch,volume,voice,language}`, the Reader
  emits `SpeechCtl::SetParams` on the ordered speech channel **before** the utterance (ADR-0005 §6.2).

State machine (Reader, `gorca-core/src/commands/profiles.rs`); `mode` below is published in the snapshot:

```rust
enum ProfileState {
    Active    { key: ProfileKey },
    Suspended { key: ProfileKey, why: Suspend, pending: Option<FocusInputs> }, // Suspend::{OwnMode, User, Both}
    Disabled,                                                                    // safe mode
}
```

| From | Event | To | Effect |
|---|---|---|---|
| Active | focus / window title / doc / `WebMode` change, new key ≠ key | Active | swap `Arc`; SW-3; optional cue `profile.switch` (off) |
| Active | own mode entered | Suspended{OwnMode} | none |
| Active | `profiles.suspend` | Suspended{User} | spoken "profiles off"; publish `mode = Suspended` |
| Suspended{OwnMode} | focus change | same | `pending` = latest inputs |
| Suspended{OwnMode} | own mode left | Active | evaluate `pending` or current inputs once |
| Suspended{OwnMode} | `profiles.suspend` | Suspended{Both} | spoken |
| Suspended{User} | focus change | same | key kept current for explain; folds use the suspended layer set |
| Suspended{User} | own mode entered / left | Suspended{Both} / — | none |
| Suspended{Both} | own mode left | Suspended{User} | none |
| Suspended{Both} | `profiles.suspend` | Suspended{OwnMode} | spoken "profiles on" (takes effect when the mode is left) |
| Suspended{User} | `profiles.suspend` | Active | spoken "profiles on"; evaluate |
| Active / Suspended | snapshot `set` reload | same | clear LRU, drop deltas whose file value changed (HR-4 per layer), new `gen`, re-evaluate |
| Active / Suspended | user profile file of an active profile deleted | same | reload path; the profile drops out of the match set |
| Disabled | reload | Disabled | only L5 reloads |
| any | safe mode | Disabled | — |

### 2.5 Edits: where a change is written

- **PE-1 Origin-layer rule.** A command that changes a setting writes the delta of the layer in `origin[key]`:
  `Default/Preset/ExtDefault/UserBase` → L5; `User(p)` → p; `Product(r)`/`AddOn(r)` → the **shadow** of r (an L6
  file with the same triggers and `shadows = "<owner>/<row-id>"`, created on save, holding only the user's
  keys). A change of a `privacy` key from any origin may also be written to L5 (it beats rows by UB-1).
- **PE-2 Explicit scope.** `profiles.edit_scope_cycle` (provisional chord gorca+Ctrl+Shift+P) cycles the target of
  the next edits for this focus: automatic (PE-1) → this application (App profile, created if absent) → this site
  (only in a document; Site profile on the host) → global (L5). It falls back to automatic after 30 s without an
  edit or on app change. A key whose scope forbids the target (NS-1) uses the nearest allowed one and says so.
- **PE-3 Every edit is audible or explained (INV-P5).** After the edit the Reader publishes a new snapshot and
  re-resolves; if the effective value did not change, the confirmation names the layer that holds it
  ("rate 60 globally; Terminal profile keeps 45").
- **PE-4 Explain.** `settings.explain_current` speaks the active profiles and rows in precedence order and, for a
  key in review, its origin; UB-1 skips are named. The settings app (M14) shows the origin per value.
- **PE-5 Save** (`settings.save`, ADR-0005 explicit save) writes every dirty delta to its own file with
  `toml_edit` (comments kept), atomic rename, content hash checked against the loaded text (a file changed on disk
  is not overwritten: "profile <name> changed on disk, reload first"). **Revert** clears all deltas. The daemon's
  config task is the only writer (ADR-0005 §7.6).
- **PE-6 Creating profiles from untrusted names (M-3).** App names come from the application, hosts from the page.
  File ids are slugs `[a-z0-9-]{1,48}` plus `-<8 hex of FNV-1a(name)>`; created with `openat(O_CREAT|O_EXCL|
  O_NOFOLLOW)` inside the profiles directory (never a path join of the raw name). The name is written into the
  trigger as an **escaped literal** (GL-1 `\*`, `\?`, `\\`), so an application named `*` or `code*` matches only
  itself. Display names are sanitised (control characters dropped, ≤ 128 chars). Tests use hostile names
  (`..`, `/`, NUL, `*`, 4 KiB, RTL marks).
- **PE-7 Session mode state is not a setting.** The browse/focus toggle stays reader state, sticky
  (`FocusCause::Manual`, only the toggle leaves). `browse.save_mode_default` (no default chord in M14a, owner
  Q-P4; reachable from the keymap and later the M15a command list) writes the current mode as
  `browse.mode_default` through PE-1/PE-2 with the default target "this site" in a browser and "this
  application" elsewhere, and applies to the current document at once (DL-1).

### 2.6 Browse-mode keys (ADR-0024 PS-2, owner DP-3)

| Key | Values | Default | Meaning |
|---|---|---|---|
| `browse.enabled` | bool | true | false = no virtual buffer; the reader follows focus (CHR-0 `Off`). Resolved at App level it also means "do not mirror documents of this app" (doc registry); at Site level the document is mirrored but stays in focus mode. |
| `browse.mode_default` | `browse` \| `focus` | `browse` | mode when a document first gets focus. `focus` enters `FocusCause::Manual` (sticky). |
| `browse.auto_focus_mode` | `widgets` \| `never` | `widgets` | NVDA `autoPassThroughOnFocusChange` (`configSpec.py:220`) with gorca's D-1 role sets. |
| `browse.sticky_focus` | bool | false | false: a user toggle is remembered per document lineage (MS-1). true: per application for the session (Orca `focus_mode_is_sticky`). |

- **MS-1 Document lineage (owner Q-P3 refinement).** A toggle survives a document replacement inside the same
  app, same frame and same host (Chromium document swaps, SPA reloads that create a new document): the new
  `DocId` inherits the mode state of the document it replaced when it gets focus within 10 s. A different host
  starts from `browse.mode_default`.
- **DL-1 When a changed default reaches an open document (M-6).** Doc keys are **latched per document at its
  first focus**; App-level `browse.enabled` is latched per app at `Appeared`. A new snapshot `gen` (reload, row
  change, suspend, scope edit) applies to documents first focused afterwards. Exceptions: (1)
  `browse.save_mode_default` updates the current document's latch (its mode already equals the saved value);
  (2) `browse.refresh` (gorca+F5) re-latches the focused document and, for App-level `browse.enabled`, the app
  (false → stop mirroring and drop to focus-following; true → start warm-up). Test per case in P6.
- `role=application` and `aria-busy` remain model inputs (ADR-0024 PS-2), never settings. The policy is the pure
  `WebPolicy::initial(doc: &DocScoped, sem: &DocSemantics, session: &SessionMode) -> WebMode` in `browse/`; the
  user toggle is always above settings and extension requests.
- **Orca preset multi-press (follow-up, not M14a-core).** Mirroring Orca, the Orca presets may bind gorca+A ×2
  to `browse.focus_sticky` and ×3 to `browse.browse_sticky` (session states; sticky browse ignores automatic focus
  mode for the document). This needs per-preset `max_press = 3` for `browse.toggle` (today `max_press: 1`,
  `gorca-core/src/commands/table.rs:1469`) and costs the multi-press window on the single toggle in that preset
  only; scheduled with M15a key work, measured against the mode-switch budget.

### 2.7 Rows from the product and from add-ons (owner: data only)

```toml
# crates/gorca-config/product/web-apps.toml (L3, compiled in); add-ons: `[[profile]]` in ext.toml from M18a
schema = 1
[[profile]]
id = "vscode"                         # stable row id; "gorca/vscode" in profiles.rows and shadows
name = "Visual Studio Code"
why = "editor built from text widgets with its own key bindings (NVDA appModules/code.py)"
triggers = [{ app = { name = ["code", "visual studio code", "code - oss", "vscodium", "cursor"] } }]
[profile.values]
browse.enabled = false
```

- **PF-1 Keys.** Only `vendor = true`, `privacy = false` keys are accepted; a rejected key is a diagnostic.
- **PF-2 User control.** `profiles.rows = { "gorca/vscode" = false, "ext-foo/bar" = true }` (L5) disables or
  enables one row; shadows override single keys (PE-1); disabling an extension removes its rows. A key in
  `profiles.rows` that names no row is a diagnostic, never silent (a renamed row is visible). Row ids are required
  and must stay stable across extension versions (manifest validation).
- **PF-3 No auto-activation.** Add-ons cannot activate, deactivate or rank rows at run time and cannot declare a
  manual profile. Triggers are declarative and evaluated by the core.
- **PF-4 Confinement (H-2).** Every row trigger must name apps: a non-empty `name`, `exe` or `desktop_id` list,
  each pattern with at least 3 literal (non-wildcard) characters; `Context`-only rows are invalid for add-ons. For
  L4 the trigger's app patterns must each be covered by a pattern of the extension manifest's `app_name` list
  (literal equality after normalisation, or a literal that the manifest glob matches); an extension with an empty
  `app_name` (toolkit/connector matchers only) has **no** rows. `browse.*` keys are accepted only from an extension
  whose manifest claims the app by `app_name`.
- **PF-5 Default state.** Product rows (L3) and rows of first-party extensions shipped in this repository are on by
  default (owner Q-P2 (a), first-party only). Third-party rows start **off**; enabling is an explicit L5 entry, and
  the install flow (M18) announces the extension's rows once.
- **PF-6 Limits.** 8 rows × 64 keys per extension, 16 triggers per row, 16 globs per matcher, 256 bytes per glob;
  over-limit rows are dropped with a diagnostic.

### 2.8 Persistence format

- `~/.config/gorca/profiles/<id>.toml`, one profile per file. The id is the file stem fixed at creation (PE-6); the
  display name is a field and may change freely. Shadows live in `profiles/shadow/<owner>--<row-id>.toml` (both
  parts slugged).

```toml
schema = 1
[profile]
name = "Telegram web"
shadows = ""                          # "<owner>/<row-id>" for a shadow
[[profile.trigger]]
site = { app = { name = ["firefox*", "chromium*", "google-chrome*"] }, host = "web.telegram.org" }
[speech]
punctuation = "some"
[browse]
mode_default = "focus"
```

- Sections and keys are those of `gorca.toml` (only overridden keys). Hot reload by the config task's inotify watch
  on the directory (debounce 200 ms, at most one compile per 200 ms); migrations per PR-9 / CF-8.
- **PC-0 Last good parse (L-5).** A file that fails to parse keeps its last good parse for the session (editors
  that write non-atomically would otherwise flip voices mid-edit); the user hears one diagnostic. A file that
  never parsed is skipped.
- **PC-1 Volume floor (M-7).** No profile or row may set `speech.volume < 20`; the lower layer's value is kept and
  the user hears once "profile <name>: volume ignored". Rate and pitch keep their full range in user profiles
  (rows cannot set them, PF-1). A command that would write a volume below the floor into a profile refuses and
  offers the global target. Silencing an app is sleep mode (`general.sleep`), which is explicit.
- **PC-4 Unavailable voice (M-7).** When a profile's `speech.voice`, `output_module` or `language` is not in the
  speech backend's inventory, or `SetParams` is rejected, the Reader uses the next lower layer's value, says once
  "profile <name>: voice <x> not available", and caches the rejection until the inventory changes. The base value
  is never clamped (it is the user's own last word; the speech client's existing fallback applies).
- **PC-2 Safe mode** (M11) folds L0, L1, L3, L5. **PC-3** At most 256 profile files and 1 MiB per file
  (`MAX_FILE_BYTES`); more is a diagnostic, never a failure.
- **PC-5 Privacy.** Site profiles store host and path globs the user wrote; no URL history is kept.

### 2.9 Migration of the CHR-0 interim table (M-5)

| CHR-0 (`app_modes.rs`) | ADR-0016 |
|---|---|
| `AppModeRow.apps` (exact, `eq_ignore_ascii_case`) | `AppMatcher.name`, escaped literals under GL-1 (same semantics) |
| `host: Option<String>` | `Trigger::Site { host }` (escaped literal) |
| `BrowseDefault::Auto` | no row (L0: `browse.enabled = true`, `auto_focus_mode = "widgets"`) |
| `BrowseDefault::Off` | `browse.enabled = false` |
| `layers` "later layer wins" | L3 < L4 < L5 (UB-1) < L6 |
| "host row beats app row" | PM-1 `Site > App` |
| `id`, `why` | row `id`, `why` (spoken by `settings.explain_current`) |
| `builtin_rows()` (`vscode`) | **L3 product row** in `crates/gorca-config/product/web-apps.toml`, loaded with `include_str!` — no extension, no ext-host change, and like CHR-0 it applies in safe mode and under `profiles.suspend` |
| `browse_default(&info.app_name)` in `doc_registry.rs` | `snapshot.resolve_app(&facts).browse_enabled()` |

P6 keeps a parity test: every CHR-0 test case, the shipped row, safe mode and suspend give the same decision as
before. `app_modes.rs` is then deleted. CHR-0 needs no change before it merges.

### 2.10 Failure modes

| Failure | Behaviour |
|---|---|
| bad profile file / bad key | key or row skipped with diagnostic; last good parse kept (PC-0) |
| profile directory unreadable | L0, L1, L3, L5 only, one spoken notice |
| `/proc` exe read or `DocURL` read times out | that input is absent: no match (never blocks the switch) |
| profile thrash (popups, title churn) | an `Arc` swap; Shell popups get no profile by default; title generations debounced |
| add-on row silences, misroutes or weakens privacy | PF-1 key filter (no voice/rate/volume/privacy keys), PF-4 confinement, PF-5 third-party off, UB-1, `profiles.suspend` (L4 off) |
| extension ↔ mode oscillation | impossible by NS-3; `request-mode` rate limit as a second guard |
| hostile app name or host creates a profile | PE-6 slug + `O_EXCL|O_NOFOLLOW` + escaped literal |
| file edited on disk while deltas are dirty | reload wins per key (HR-4); save refuses on hash mismatch |

## 3. Alternatives

| Option | Why not |
|---|---|
| A1 NVDA stack (edits into the top profile) | surprising edits; no add-on rows; no site triggers |
| A2 Orca layers (app → profile → default, manual profiles) | no window/site/context triggers; no add-on rows; dconf storage |
| CSS-like cascade (per-key selectors over roles/nodes) | per-node cost on presentation paths; per-node variability belongs to the semantic model (ADR-0024 PS-6) |
| Each actor folds its own settings from files | several writers and inputs; this is exactly the H-1 divergence the single snapshot removes |
| Add-on rows above the user base (r1) | an add-on could override a user's explicit global choice; replaced by UB-1 |
| Bake-off | not contested: semantics and UX; P1/P2 are met by construction (`Arc` swap, prewarm) |

## 4. Consequences

- `gorca-config` gains `glob`, `profiles/` (pure), `product/*.toml` and `SettingDef` metadata; `Overlay` becomes a
  sparse `SettingsPatch` delta per layer (façade kept until callers move).
- The Reader gains `ProfileState`, the LRU and the published `ProfileSnapshot`; the doc registry and terminal arm
  read App/Doc keys through the snapshot instead of static tables.
- ext-host glob becomes ASCII case-insensitive with escapes (GL-1); ext-vte's secret pattern becomes additive (TS-1).
- Per-app behaviour that needs code today (VS Code without a buffer) becomes data; NVDA's
  `disableBrowseModeByDefault` list can be ported as product rows after measurement.
- Users learn "where does this setting come from" — mitigated by PE-3 and PE-4.

## 5. Metrics to validate

| Id | Stimulus → end | Target |
|---|---|---|
| **P1** | focus change into an app with a matching profile (voice change included) → first SPEAK | Δp99 ≤ 0.2 ms vs no profiles; key resolution ≤ 20 µs p99 on a hit, fold ≤ 200 µs p99 on a miss |
| **P1b** | gorca+Space in a document with a `FocusMode` context profile that changes rate → mode word SPEAK (and the reverse) | Δp99 ≤ 0.2 ms vs no profiles; 0 inline folds after prewarm (counter) |
| P2 | 200 profiles × 4 triggers loaded | compile ≤ 50 ms on the config task; P1, P1b and K2 unchanged |
| P3 | edit command → confirmation SPEAK | ≤ 5 ms p99 gorca-side; INV-P5 property test (no silent edit) |
| P4 | 100 rewrites/s of a profile file (inotify storm), incl. half-written files | K2 p99 unchanged; ≤ 1 compile per 200 ms; no voice flip (PC-0) |
| P5 | migration parity | 100 % of CHR-0 cases, the shipped row, safe mode and suspend decide identically |
| P6 | property tests | fold = specification oracle (random sets of ≤ 32 profiles and rows, all trigger kinds, deltas, suspend); INV-P6; NS-1 matrix exhaustive; PE-1 targets exhaustive; PF-4 confinement against random manifests; GL-1 vs CHR-0 `eq_ignore_ascii_case` incl. non-ASCII; PE-6 hostile names |
| P7 | DL-1 cases | each latch rule has one e2e step (reload, save-mode, refresh, lineage swap) |

## 6. Implementation slices (M14a-core; ≤ 2 days each, no GUI)

None touches M22a files (`gorca-model/src/{sem,fact.rs}`, `gorca-fuse`, `gorca-fact-api`, `gorca-sem-atspi`,
`gorca-ext-api/src/codes.rs`, `gorca-testkit/src/{fake_source,fact_conformance}.rs`, M22a benches) or CHR-0-owned
files (`doc_registry.rs`, `browse/commands.rs`, `doc_actor/`, `app_actor/doc_route.rs`, doc-source crates). One
trivial conflict with CHR-0 in `gorca-config/src/lib.rs` (module list). No new crates (Cargo.lock stays out of the
M22a waves' way); tests are proptest round-trips, no cargo-fuzz.

| Id | Slice | Owner | Days | Depends on | Files | Free slot now? |
|---|---|---|---|---|---|---|
| **P1** | `KeyId` registry over `SETTINGS`; `SettingDef.scope/vendor/privacy`; `SettingType::Map(bool)`; typed `Value`; `SettingsPatch` with `apply`/`drop_changed` (generalises `Overlay`, façade kept); `load.rs` `apply_key` through `Value`; UB-1 default-equality helper; tests: every row round-trips | rust-engineer | 2 | — | `gorca-config/src/{settings.rs,load.rs,patch.rs}` | **yes** |
| **P2** | GL-1 glob (escapes, ASCII case-insensitive, limits) + ext-host re-export or copy; profile, product-row and add-on-row parsers; `Trigger`, `AppMatcher`; NS-1, NS-3, PF-1, PF-4, PF-6, PC-1, PC-3 diagnostics; `ProfileSet::compile` (TR-2); proptest round-trips | rust-engineer | 2 | P1 | `gorca-config/src/{glob.rs,profiles/{mod,trigger,file,compile}.rs}`, `gorca-ext-host/src/glob.rs` | **yes** |
| **P3** | `ProfileSnapshot` and the three resolvers (SS-1, UB-1, suspend/safe-mode layer sets), interning, fold with `origin`, LRU type; PE-1/PE-2 `plan_edit`; delta save into profile and shadow files with PE-6 naming (`save.rs`); property tests P6 against a Python oracle | rust-engineer + math-researcher (oracle) | 2 | P2 | `gorca-config/src/profiles/{snapshot,resolve,edit,naming}.rs`, `save.rs`, `tools/research/profiles_oracle.py` | **yes** |
| P4 | config task: profiles dir load, inotify, PC-0, prewarm (SW-2) by `gen`; Reader `ProfileState` machine (full table), snapshot publication, key computation from focus/window/`WebMode`, SW-3, `profiles.suspend`, `settings.explain_current`, `profiles.edit_scope_cycle`, PC-4; catalog strings | rust-engineer | 2 | P3 | `gorca-daemon/src/config_task.rs`, `gorca-core/src/commands/{profiles.rs,state.rs,handlers.rs}`, focus hook in `reader.rs`, `gorca-i18n` catalogs (append) | yes, if no other branch holds `commands/state.rs` |
| P5 | gates P1, P1b, P2–P4 bench (`gorca-bench/benches/profiles.rs`) + e2e: terminal App profile `punctuation=most`; voice-change ordering; gorca+Space with a `FocusMode` profile | perf-engineer, test-engineer | 1.5 | P4 | `crates/gorca-bench/benches/profiles.rs`, `bench/e2e` scenario | yes |
| P6 | browse keys (§2.6), `WebPolicy::initial`, MS-1 lineage, DL-1 latches, `browse.save_mode_default`, CHR-0 migration to the L3 product row + parity test P5; terminal arm reads App keys via the snapshot; TS-1 in ext-vte; delete `app_modes.rs` | rust-engineer (+ web-a11y-engineer review) | 2 | P4, **CHR-0 merged** | `doc_registry.rs`, `browse/{mod,commands,policy}.rs`, `gorca-core/src/terminal.rs`, `extensions/ext-vte/src/prompt.rs`, `gorca-config/product/` | after CHR-0; before M22a wave C or rebased on it |
| P7 | inputs: `AppInfo.exe` (async `/proc`), `DocMeta.url` (Site triggers), window title generations; `desktop_id` when ShellHelper exposes it | linux-platform-engineer | 2 | P4 | connector `AppInfo`, doc source (`DocURL`), `present/window.rs` | after M22a wave C |
| P8 | adversarial review + fix round, decision row | reviewer, then rust-engineer | 2 | P5, P6 | — | — |

Critical path P1 → P2 → P3 → P4 → P6 → P8 ≈ 12 days; P1–P3 fit the free cargo slot of M22a wave A once this ADR
is accepted (one cargo build per agent). Site triggers work end to end only after P7; until then `Site` rows load
and `settings.explain_current` reports them as "waiting for the document address". The manual profile (L7) stays
type-only until M15a.

## 7. Decisions pending (owner)

| # | Question | Options | Recommended (architect + reviewer) |
|---|---|---|---|
| Q-P1 (= QF-1) | A setting changed with a key while an app profile is active: where does it go? | (a) origin layer (global unless the profile sets it), plus a scope key; (b) always the active profile (NVDA); (c) always global | **(a)** |
| Q-P2 (= QF-2) | Rows shipped with gorca and its own extensions (VS Code without browse mode, terminal punctuation) | (a) on by default, presentation and mode keys only, one switch each; (b) off until enabled | **(a) for first-party rows only**; third-party add-on rows start off (PF-5) |
| Q-P3 | Scope of the browse/focus toggle by default | (a) per document (NVDA); (b) per application for the session (Orca) | **(a)**, kept across document replacement on the same host (MS-1), with `browse.sticky_focus = true` per app for messengers |
| Q-P4 | Chord for "save the current mode as the default here" | (a) gorca+Space ×3 (would run toggle, toggle, save today); (b) no chord now: keymap and the M15a command list | **(b)**; Orca presets later mirror Orca's ×2 sticky focus / ×3 sticky browse (§2.6), NVDA presets may get ×3 = save with `max_press = 3` |
| Q-P5 | Manual profile switching before the chooser (M15a) | (a) `profiles.manual_cycle` now; (b) wait for M15a | **(b)**, and the manual layer is type-only until then |
| Q-P6 (new, UB-1) | Your global file sets a value different from the default (e.g. punctuation "all"); an extension row sets it for one app | (a) your global choice wins; keep the row's value by its shadow or a profile; (b) the row wins in its app | **(a)** |

## 8. Review r1 mapping

| Finding | Where |
|---|---|
| H-1 one input for all resolvers | §2.4 SS-1, INV-P6, `ProfileSnapshot` |
| H-2 confinement, third-party off | §2.7 PF-4, PF-5 |
| H-3 privacy keys, rows below user choices | §2.3 `privacy`, TS-1; §2.1 L4/L5 order, UB-1 |
| M-1 toggle computes a key | §2.4 SW-1b, SW-2; §5 P1b |
| M-2 extension/mode loop | §2.3 NS-3 |
| M-3 untrusted names | §2.5 PE-6 |
| M-4 glob semantics | §2.2 GL-1 |
| M-5 CHR-0 migration | §2.1 L3 product rows; §2.9 |
| M-6 latch timing | §2.6 DL-1, MS-1; §5 P7 |
| M-7 clamp | §2.8 PC-1, PC-4 |
| L-1 state table | §2.4 table |
| L-2 specificity | §2.1 PM-1 |
| L-3 limits | §2.7 PF-6 |
| L-4 ids, shadows, row ids | §2.8; PF-2 |
| L-5 half-written files | §2.8 PC-0 |
| L-6 context source | §2.2 TR-1 |
| L-7 fuzz, no new crate | §6 preamble; SW-1 |
| L-8 map type | §2.3 `SettingType::Map(bool)` |
