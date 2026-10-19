# ADR-0016: Configuration profiles (typed layer stack, triggers, per-app edits)

Status: **Accepted** (2026-10-19, owner: r3 incl. PK-1 wording and all chords; r3.1 adds the save-to-global
addendum §2.5b). History: r3 owner answers Q-P1..Q-P6 applied, §9; r2: review
`docs/reports/adr-0016-review.md` r1 ACCEPT-WITH-CHANGES, H-1..H-3, M-1..M-7 and L-1..L-8 integrated, §8).
Owner: architect.
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

**Typed layer stack with automatic triggers; edits go to the focused application's profile** (option A3 of
`phase-3-features.md` §3.2, with the owner's Q-P1 edit rule replacing A3's origin-layer rule in r3).
All resolvers read **one published snapshot**; effective settings are precomputed per match set and swapped as one
`Arc`.

### 2.1 Layers and precedence (lowest first)

| Layer | Source | Writable | On in safe mode / under `profiles.suspend` | Who may set what |
|---|---|---|---|---|
| L0 Defaults | `SETTINGS` table | no | yes / yes | every key |
| L1 Preset | keyboard preset × layout (`presets/*.toml`) | no | yes / yes | keymap, keyboard-implied defaults |
| L2 Extension defaults | extension schema defaults | no | no / yes | only `ext.<id>.*` of that extension (ADR-0019 I-8) |
| L3 Third-party rows | `[[profile]]` rows of installed third-party extensions | no | no / **no** | `vendor` keys, non-`privacy`, confined to the extension's `app_name` list (PF-4); **off** until the user enables each (owner Q-P2) |
| L4 First-party rows | product rows compiled into `gorca-config` (`include_str!("product/*.toml")`) and rows of first-party extensions in this repository | no | product rows **yes / yes**; extension rows no / no | as L3; **on** automatically and above every third-party row (owner Q-P2) |
| L5 User base | `~/.config/gorca/gorca.toml` | yes | yes / yes | every key; see UB-1 |
| L6 User profiles | `~/.config/gorca/profiles/*.toml`: app profiles (auto-created by edits, PE-1), site/window/context profiles, manual-only profiles | yes | no / no | keys of the global schema only (PK-1), never `Base` keys, subject to the matrix (§2.3); an app **pin** (PS-1) replaces automatic selection for one app |
| Session deltas | unsaved runtime edits | — | — | **not a layer above the others**: each writable layer (L5, each L6 file) carries its own delta, folded directly on top of that layer's file values |
| Safety clamp | — | — | always | PC-1 |

Fold order: `L0, L1, L2, L3 (by specificity), L4 (by specificity), L5 (file ⊕ Δ, UB-1), L6 (the pinned profile,
or the matching profiles by specificity; file ⊕ Δ each), clamp`. Names, not numbers, are normative; ADR-0024 PS-5
"L3/L5/L6" means the user layers (here L5, L6) and "vendor (L4)" means L3 and L4. Every user profile sits above
every row, so overriding a row needs no shadow file (r2 shadows are dropped): the app profile carries the key.

- **UB-1 User choices beat add-on and product rows; generated defaults do not.** An L5 entry whose value equals the
  L0/L1 default is not an override (a full dump of the defaults, as `gorca --defaults` writes, must not cancel
  every row); it is skipped in the fold. Any L5 entry that differs from the default, and every L5 entry of a
  `privacy` key, sits above L3/L4 (owner Q-P6: yes). To keep the default value against a row, the user disables
  the row (PF-2) or sets the key in the app profile. `settings.explain_current` says which applies.
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
- **TR-3 ProfileKey** = `(rows: MatchSetId, user: MatchSetId, pin: Option<PinId>)`, interned `u32`s.
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
| `browse.enabled`, `browse.mode_default`, `browse.auto_focus_mode`, `browse.toggle_scope`, `browse.selection_model` (ADR-0021), `browse.say_all_on_load` | **Doc** | yes | no | mode policy inputs (§2.6) |
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
    pub pins: Arc<Pins>,               // session: per app Auto | Profile(id) | GlobalOnly (PS-1)
    pub targets: Arc<EditTargets>,     // session: per app App | Site | Global (PE-2)
    pub disabled: Arc<HashSet<ProfileId>>, // profiles switched off (persisted as `enabled = false` on save)
    pub mode: ProfileMode,             // Active | Suspended | Disabled (safe mode)
}
pub struct Effective {
    pub settings: Settings,
    pub origin: Box<[LayerRef]>,       // per KeyId: which layer supplied the value (PE-1, explain)
    pub report: ReportFlags,           // u64, compiled once (M22d consumes)
}
pub enum LayerRef { Default, Preset, ExtDefault(ExtId), ThirdParty(RowId), FirstParty(RowId), UserBase, User(ProfileId) }

impl ProfileSnapshot {
    pub fn resolve_focus(&self, key: ProfileKey) -> Effective;
    pub fn resolve_app(&self, app: &AppFacts) -> AppScoped;              // App keys
    pub fn resolve_doc(&self, app: &AppFacts, doc: &DocFacts) -> DocScoped; // Doc keys
}
```

- **SS-1 One input (H-1).** All three resolvers are pure functions of one snapshot. `Suspended` folds only
  L0, L1, L3, L5 (+ L5 delta); `Disabled` folds L0, L1, L3, L5 (safe mode keeps product rows, M-5). Unsaved edits,
  suspend, pins and disabled profiles therefore reach App and Doc keys in the doc registry and the terminal
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

- **PK-1 Profiles override only what the global level knows (owner: "нельзя в общем профиле сменить то, чего
  нет"; wording confirmed 2026-10-19).** *Every key in a profile or row must be a key of the global schema: a core
  `SETTINGS` row or a key of an installed extension's schema (`ext.<id>.*`). A profile cannot introduce a key, a
  section or a value type that the global settings do not have; such a key is rejected at load with a
  "did you mean" diagnostic and never applied. Keys of an extension that is not installed are kept in the file
  untouched but inactive (one diagnostic), so reinstalling the extension restores them. Values are validated
  against the same type and range as in the global file.* Consequence: the effective settings of any app are
  always "the global settings with some values changed", the settings app (M14) can show every profile as a diff
  against global, and "save to global" (PE-4) is always possible.
- **PE-1 Edits go to the focused application's profile (owner Q-P1).** A command that changes a setting (rate,
  pitch, volume, punctuation, verbosity, echo of non-privacy kinds, `browse.save_mode_default`, ...) writes the
  delta of the **app profile of the focused application**: the pinned profile if the app is pinned (PS-1),
  otherwise the user profile with an App trigger for this app (the most specific one if several), otherwise a
  new one created on the first edit (PE-6 naming, App trigger with the app name as an escaped literal, display
  name = the application's name) and announced once: "new profile for Firefox". Exceptions:
  - **PE-1a More specific user profile.** If a user Site or Window profile currently supplies the key, the edit
    goes there (writing to the app profile would be inaudible); the confirmation names it ("for
    web.telegram.org").
  - **PE-1b Inherently global keys** always go to the global file L5 and say so ("saved globally"): every `Base`
    key (speech server/backend, keyboard modifiers, preset and layout, UI language, `profiles.*`) and every
    `privacy` key (`echo.*`, `editing.announce_clipboard`, `terminal.secret_regex`, `sources.*`), which are
    choices about the person and the room, not about an application. A user may still put a privacy key into an
    app profile by editing the file (or the M14 GUI); once the app profile holds it, key edits of that key go
    there (PE-1a logic). `Base` keys never enter a profile.
  - **PE-1c Explicit target.** When the target for this app is switched to "global" (PE-2), edits go to L5.
  - No focused application (startup, lock screen): L5.
  So a change made in Firefox never changes Signal; the global profile changes only by an explicit action
  (PE-2 target, PE-4 save to global) or for PE-1b keys.
- **PE-2 Edit target (owner Q-P5).** `profiles.edit_target_cycle` (gorca+Ctrl+Alt+P) cycles the target for the
  focused app: "this application" (default) → "this site" (only in a document; Site profile on the host, created
  if absent) → "global". The choice is session state per app (in the snapshot, `targets`), survives focus changes
  and is reset by restart; every confirmation names the target, so a forgotten "global" is heard on the next edit.
- **PE-3 Every edit says where it went and is audible or explained (INV-P5).** Confirmations carry the target
  once per change burst: "rate 60, for Firefox", "punctuation most, globally". After the edit the Reader
  publishes a new snapshot and re-resolves; if the effective value did not change (a global edit while the app
  profile sets the key), the confirmation names the layer that holds it ("rate 60 globally; Firefox keeps 45").
- **PE-4 Explain.** `settings.explain_current` speaks the active profiles and rows in precedence order and, for a
  key in review, its origin; UB-1 skips are named. The settings app (M14) shows the origin per value.
- **PE-4 Save to global** (`profiles.save_global`, gorca+Ctrl+Shift+C): copies the **touched set** (§2.5b) of the
  current target into L5, removes those keys from the app profile, then saves both files; it says "3 settings
  saved globally, removed from Firefox". Pressed twice (`profiles.apply_everywhere`): additionally removes the
  same touched keys from every other user profile so the global value applies everywhere, after a spoken count
  ("applies to all apps; 2 profiles changed"). Nothing else writes L5 implicitly except PE-1b. Copying keys the
  user did not touch is not offered (§2.5b SG-2).
- **PE-5 Save** (`settings.save`, gorca+Ctrl+C in all presets today, as NVDA+Ctrl+C) writes every dirty delta to its own file with
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
  (`FocusCause::Manual`, only the toggle leaves). `browse.save_mode_default` (gorca+Ctrl+Shift+Space, owner Q-P4:
  its own chord, never a gorca+Space multi-press) writes the current mode as `browse.mode_default` through
  PE-1/PE-2 (app profile by default, site profile when the target is "this site") and applies to the current
  document at once (DL-1). `settings.save` (gorca+Ctrl+C) persists it like any other delta.

### 2.5b Save to global never clobbers the global profile (r3.1, owner addendum)

- **SG-1 Touched set.** The Reader records, per edit target (app profile, site profile), the keys the user changed
  by command in this session: `touched: HashMap<TargetId, BTreeSet<KeyId>>` in the snapshot's delta record. An
  entry is added by a successful edit and removed by revert, by a save-to-global of that key, or when a file
  reload changes that key (HR-4). It is session state; after restart it is empty.
- **SG-2 Only touched keys are copied.** `profiles.save_global` copies exactly `touched[target]`, with the value
  the user set. Values in the app profile the user did not touch this session (older edits, hand-written values,
  values another tool wrote) are **never** copied, even if they differ from global: the global value is not
  overwritten by an app value the user did not touch. With an empty touched set the command says "nothing changed
  for Firefox in this session" and writes nothing.
- **SG-3 Only keys global can hold.** A touched key is copied only if it is "present in global" at that moment:
  a core `SETTINGS` key, or `ext.<id>.*` of an extension that is **installed and loaded** (its schema defines the
  key). Keys of an extension that is not installed are skipped and named ("1 setting of ext-foo kept for Firefox
  only: the extension is not installed"). `Base` keys never reach this path (PE-1b).
- **SG-4 Never reverse.** Save-to-global writes forward only: it never deletes or resets a global key, never
  copies a value from global into a profile, and never removes from the app profile a key it did not copy. A
  copied key whose value already equals the global value is removed from the app profile and reported as
  "already global". If the global file changed on disk since load, the whole operation refuses (PE-5 hash rule):
  both files stay untouched.
- **SG-5 Atomic pair.** The L5 write and the app-profile write are planned as one `SavePlan`; the global file is
  written first (atomic rename), then the profile. If the second write fails, the key exists in both files with
  the same value (effective settings unchanged), the user hears "saved globally; Firefox profile not updated",
  and the touched set is kept so the command can be repeated. No ordering leaves a key in neither file.
- **SG-6 Extension removed later.** When an extension is uninstalled, its `ext.<id>.*` keys in L5 and in profiles
  stay in the files untouched and become inactive (PK-1): not applied, one diagnostic per file, not shown as
  errors to the user, not copied by save-to-global, and re-activated without change if the extension is
  reinstalled with a schema that still defines them (a key whose type changed is reported and stays inactive).
  M18 extension migrations, not this rule, rename keys.

### 2.5a Profile commands (owner Q-P5) and chords

Automatic selection stays the default; these commands act on the focused application. All chords were checked
free in the four presets (`crates/gorca-config/presets/{nvda,orca}-{desktop,laptop}.toml`) and the command table
(`gorca-core/src/commands/table.rs`); only `gorca+ctrl+c` was already bound (to `settings.save`, kept).

| Command | Chord (all presets) | Behaviour |
|---|---|---|
| `settings.save` | gorca+Ctrl+C (existing) | save every dirty delta to its file (PE-5) |
| `profiles.save_global` | gorca+Ctrl+Shift+C (×2 = apply everywhere, PE-4) | move this app's changes to global (PE-4) |
| `profiles.cycle` | gorca+Ctrl+P | cycle the **pin** for this app: Automatic → each user profile that matches this app or is `manual = true` → "global only" (no user profiles or rows for this app) → Automatic; speaks the new choice |
| `profiles.toggle_enabled` | gorca+Ctrl+Shift+P | switch off/on the profile currently in effect for this app (pinned, else the top matching user profile, else the top row); a user profile gets `enabled = false` as a delta, a row an entry in `profiles.rows` (global) |
| `profiles.edit_target_cycle` | gorca+Ctrl+Alt+P | PE-2 |
| `profiles.announce` | gorca+Shift+P | speaks the active profile(s) for this app and where edits go ("Firefox profile, edits for Firefox"); ×2: the full stack with rows and pins (`settings.explain_current`) |
| `profiles.suspend` | unbound (keymap, command list) | §2.4 |
| `browse.save_mode_default` | gorca+Ctrl+Shift+Space | PE-7 |

- **PS-1 Pins are session state** (`pins` in the snapshot), never written to disk; restart returns to Automatic.
  A pin replaces only L6 selection for that app; rows still apply unless the pin is "global only". The NVDA
  multi-press semantics of `max_press` are unchanged (each chord above has `max_press = 1` except
  `profiles.save_global` and `profiles.announce`).
- gorca+Shift+P appears as a doc-comment example in `gorca-ext-host/src/manifest_file.rs:24`; P2 changes the
  example. Orca has no profile-cycling chord (its profiles are loaded from the preferences dialog); NVDA uses
  NVDA+Ctrl+P for its profiles dialog and NVDA+Ctrl+C for save, so the P family and gorca+Ctrl+C match NVDA
  muscle memory.

### 2.6 Browse-mode keys (ADR-0024 PS-2, owner DP-3)

| Key | Values | Default | Meaning |
|---|---|---|---|
| `browse.enabled` | bool | true | false = no virtual buffer; the reader follows focus (CHR-0 `Off`). Resolved at App level it also means "do not mirror documents of this app" (doc registry); at Site level the document is mirrored but stays in focus mode. |
| `browse.mode_default` | `browse` \| `focus` | `browse` | mode when a document first gets focus. `focus` enters `FocusCause::Manual` (sticky). |
| `browse.auto_focus_mode` | `widgets` \| `never` | `widgets` | NVDA `autoPassThroughOnFocusChange` (`configSpec.py:220`) with gorca's D-1 role sets. |
| `browse.toggle_scope` | `document` \| `application` | `document` | owner Q-P3, both supported: `document` = a user toggle is remembered per document lineage (MS-1, NVDA); `application` = per application for the session, new documents of the app open in the toggled mode (Orca `focus_mode_is_sticky`). Settable globally, per app profile or per site profile. Replaces ADR-0024 PS-2's `browse.sticky_focus` (bool), which had no definition. |

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
id = "vscode"                         # stable row id; "gorca/vscode" in profiles.rows
name = "Visual Studio Code"
why = "editor built from text widgets with its own key bindings (NVDA appModules/code.py)"
triggers = [{ app = { name = ["code", "visual studio code", "code - oss", "vscodium", "cursor"] } }]
[profile.values]
browse.enabled = false
```

- **PF-1 Keys.** Only `vendor = true`, `privacy = false` keys are accepted; a rejected key is a diagnostic.
- **PF-2 User control.** `profiles.rows = { "gorca/vscode" = false, "ext-foo/bar" = true }` (L5) disables or
  enables one row (also `profiles.toggle_enabled`); an app profile overrides single keys (PE-1); disabling an extension removes its rows. A key in
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
- **PF-5 Default state (owner Q-P2).** First-party rows (L4: product rows and rows of extensions shipped in this
  repository) are on automatically and, when several rows match, beat every third-party row (layer order) and
  then each other by PM-1. Third-party rows (L3) start **off**; enabling is an explicit L5 entry or
  `profiles.toggle_enabled`, and the install flow (M18) announces the extension's rows once.
- **PF-6 Limits.** 8 rows × 64 keys per extension, 16 triggers per row, 16 globs per matcher, 256 bytes per glob;
  over-limit rows are dropped with a diagnostic.

### 2.8 Persistence format

- `~/.config/gorca/profiles/<id>.toml`, one profile per file. The id is the file stem fixed at creation (PE-6); the
  display name is a field and may change freely. App profiles created by PE-1 are ordinary files the user may edit.

```toml
schema = 1
[profile]
name = "Telegram web"
enabled = true                        # profiles.toggle_enabled writes false
manual = false                        # true: never triggered, reachable only through profiles.cycle
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
| `builtin_rows()` (`vscode`) | **L4 product row** in `crates/gorca-config/product/web-apps.toml`, loaded with `include_str!` — no extension, no ext-host change, and like CHR-0 it applies in safe mode and under `profiles.suspend` |
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
| user forgets the edit target is "global" | every confirmation names the target (PE-3); target resets on restart |
| many auto-created app profiles | one file per app actually edited; `profiles.save_global` ×2 folds one back; M14 GUI lists them as diffs (PK-1) |

## 3. Alternatives

| Option | Why not |
|---|---|
| A1 NVDA stack (edits into the top profile) | no add-on rows, no site triggers. Its edit rule is close to the owner's Q-P1 choice; r3 keeps the rule but removes NVDA's confusion by naming the target in every confirmation (PE-3), sending person-level keys to global (PE-1b) and offering save-to-global (PE-4) |
| r1/r2 origin-layer edits (a change goes to the layer that supplied the value) | owner Q-P1: a change in one app must never alter another app; origin-layer edits wrote most changes to global |
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
- Users learn "where does this setting come from" — mitigated by PE-3, PE-4 and `profiles.announce`.
- App profiles accumulate (one per edited app); PK-1 keeps each a readable diff against global.
- Four new chords in every preset (§2.5a) plus gorca+Ctrl+Shift+Space and gorca+Ctrl+Shift+C.

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
| P8 | Q-P1 isolation | e2e: rate change in Firefox, focus Signal → Signal's rate unchanged; "save to global" → both change unless an app profile sets it; PE-1b keys land in L5 |
| P9 | profile chords | gorca+Ctrl+P / Shift+P / Ctrl+Shift+P / Ctrl+Alt+P → spoken result ≤ 5 ms p99 gorca-side; preset conflict test (no chord bound twice in any preset) |

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
| **P3** | `ProfileSnapshot` and the three resolvers (SS-1, UB-1, suspend/safe-mode layer sets), interning, fold with `origin`, LRU type; PK-1 validation; PE-1/PE-1a/PE-1b/PE-2 `plan_edit` incl. app-profile auto-creation, PE-4 move/remove planning; delta save with PE-6 naming (`save.rs`); property tests P6 against a Python oracle | rust-engineer + math-researcher (oracle) | 2 | P2 | `gorca-config/src/profiles/{snapshot,resolve,edit,naming}.rs`, `save.rs`, `tools/research/profiles_oracle.py` | **yes** |
| P4 | config task: profiles dir load, inotify, PC-0, prewarm (SW-2) by `gen`; Reader `ProfileState` machine (full table), snapshot publication, key computation from focus/window/`WebMode`, SW-3, `profiles.suspend`, `settings.explain_current`, §2.5a commands and preset chords (pins, enable, target, announce, save global), PC-4; catalog strings | rust-engineer | 2 | P3 | `gorca-daemon/src/config_task.rs`, `gorca-core/src/commands/{profiles.rs,state.rs,handlers.rs,table.rs}`, `gorca-config/presets/*.toml`, focus hook in `reader.rs`, `gorca-i18n` catalogs (append) | yes, if no other branch holds `commands/state.rs` |
| P5 | gates P1, P1b, P2–P4, P8, P9 bench (`gorca-bench/benches/profiles.rs`) + e2e: terminal App profile `punctuation=most`; voice-change ordering; gorca+Space with a `FocusMode` profile | perf-engineer, test-engineer | 1.5 | P4 | `crates/gorca-bench/benches/profiles.rs`, `bench/e2e` scenario | yes |
| P6 | browse keys (§2.6) incl. `browse.toggle_scope`, `WebPolicy::initial`, MS-1 lineage, DL-1 latches, `browse.save_mode_default`, CHR-0 migration to the L3 product row + parity test P5; terminal arm reads App keys via the snapshot; TS-1 in ext-vte; delete `app_modes.rs` | rust-engineer (+ web-a11y-engineer review) | 2 | P4, **CHR-0 merged** | `doc_registry.rs`, `browse/{mod,commands,policy}.rs`, `gorca-core/src/terminal.rs`, `extensions/ext-vte/src/prompt.rs`, `gorca-config/product/` | after CHR-0; before M22a wave C or rebased on it |
| P7 | inputs: `AppInfo.exe` (async `/proc`), `DocMeta.url` (Site triggers), window title generations; `desktop_id` when ShellHelper exposes it | linux-platform-engineer | 2 | P4 | connector `AppInfo`, doc source (`DocURL`), `present/window.rs` | after M22a wave C |
| P8 | adversarial review + fix round, decision row | reviewer, then rust-engineer | 2 | P5, P6 | — | — |

Critical path P1 → P2 → P3 → P4 → P6 → P8 ≈ 12 days; P1–P3 fit the free cargo slot of M22a wave A once this ADR
is accepted (one cargo build per agent). Site triggers work end to end only after P7; until then `Site` rows load
and `settings.explain_current` reports them as "waiting for the document address". Pins and the profile chords land in
P4; the M15a chooser later lists the same profiles.

### 6.1 Test list per slice

| Slice | Tests (unit/property unless stated) |
|---|---|
| P1 | every `SETTINGS` row round-trips through `Value` and `SettingsPatch`; `Overlay` façade equals the old behaviour on the existing tests; `SettingType::Map` parse/print; UB-1 default-equality per type; `drop_changed` per key |
| P2 | GL-1: escapes, `?`, ASCII case folding, non-ASCII exact, 256-byte cap, no recursion on `*` storms; PK-1: unknown key, wrong type, out of range, key of an uninstalled extension kept inactive and reported; NS-1 matrix exhaustive (trigger kind × scope); NS-3 `ext.*` under `Context{BrowseMode,FocusMode}` rejected; PF-1 privacy/voice keys rejected from rows; PF-4 empty-matcher extension has no rows, non-covered app patterns rejected, `browse.*` only for `app_name` claimants; PF-6 limits; PC-0 last good parse kept; proptest parser round-trip |
| P3 | fold = Python oracle (≤ 32 profiles and rows, deltas, pins, disabled, suspend, safe mode); INV-P6 App/Doc == Focus for the same facts; UB-1 skip of default-equal L5 entries; L4 first-party beats L3 third-party; PE-1 target table exhaustive (pinned, existing app profile, auto-create, PE-1a site/window supplier, PE-1b `Base` and privacy keys, PE-1c global target, no focused app); PE-6 hostile names (`..`, `/`, NUL, `*`, 4 KiB, RTL) → slug + escaped literal; **SG-1..SG-6**: only touched keys copied, untouched differing values never copied, empty touched set writes nothing, uninstalled-extension key skipped and named, nothing deleted or reset in global, equal value removed from profile as "already global", hash mismatch refuses both writes, second-write failure leaves both files consistent and the touched set kept, reload of a touched key clears it, `apply_everywhere` removes only touched keys from other profiles |
| P4 | `ProfileState` table exhaustive (every state × event); snapshot `gen` bumps on pins, targets, disabled, deltas, touched; prewarm entries of an older `gen` dropped; SW-3 `SetParams` before the utterance; announce/target/cycle/toggle chords bound once per preset (conflict test over all four presets and the command table); PC-4 unavailable voice falls back once |
| P5 | gates P1, P1b, P2, P3, P4, P9 (bench); e2e P8: rate in Firefox leaves Signal unchanged; save-to-global with one touched and one untouched differing key copies only the touched one; uninstall `ext-test` and reinstall: its profile keys inactive then active again |
| P6 | parity with CHR-0 (all cases, shipped row, safe mode, suspend); DL-1 latches (reload, save-mode, refresh); MS-1 lineage across a same-host document swap and not across hosts; `browse.toggle_scope` document vs application; TS-1 additive secret pattern (user pattern cannot remove the default); terminal arm reads App keys from the snapshot |
| P7 | `/proc` exe timeout → no match, switch not delayed; `DocURL` absent or slow → Site rows inactive and explained; title generation debounce |

## 7. Owner decisions (answered and confirmed 2026-10-19)

| # | Question | Owner answer | Where |
|---|---|---|---|
| Q-P1 | Where does a setting changed with a key go? | the focused app's profile, auto-created; global only by an explicit action | PE-1..PE-4 |
| Q-P2 | First-party and third-party rows | first-party on and winning; third-party off until enabled | §2.1 L3/L4, PF-5 |
| Q-P3 | Scope of the browse/focus toggle | both, configurable; per document default | `browse.toggle_scope`, MS-1 |
| Q-P4 | Chord for "save" | its own NVDA-like chord, never a gorca+Space multi-press | gorca+Ctrl+C (save), gorca+Ctrl+Shift+Space (save mode default), gorca+Ctrl+Shift+C (save to global) |
| Q-P5 | Profile hotkeys | cycle, enable/disable, global vs app target, announce; automatic stays default | §2.5a |
| Q-P6 | User's global choice vs a row | the user's choice wins | UB-1 |

**PK-1, confirmed by the owner 2026-10-19** — "A profile can only change settings that exist in the global settings: every
key must be a core setting or a setting of an installed extension, with the same type and range. Unknown keys are
rejected when the file loads; keys of an uninstalled extension are kept but inactive."

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

## 9. Owner answers r3 mapping

| Answer | Change from r2 |
|---|---|
| Q-P1 | origin-layer rule replaced by the focused-app rule PE-1 (+PE-1a/b/c), PK-1, save to global PE-4; shadows dropped (user profiles are above all rows) |
| Q-P2 | rows split into L3 third-party (off) and L4 first-party (on, winning) |
| Q-P3 | `browse.sticky_focus` (bool) → `browse.toggle_scope = document \| application` |
| Q-P4 | gorca+Ctrl+C save kept; gorca+Ctrl+Shift+Space save mode default; gorca+Ctrl+Shift+C save to global |
| Q-P5 | manual layer L7 replaced by per-app session pins; §2.5a chords |
| Q-P6 | UB-1 confirmed |
| r3.1 addendum | save-to-global copies only touched keys present in global, never reverses or clobbers (§2.5b SG-1..SG-6); `profiles.save_global` ×3 dropped (×2 = apply everywhere); test list §6.1; status Accepted |
