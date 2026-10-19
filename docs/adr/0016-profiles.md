# ADR-0016: Configuration profiles (typed layer stack, triggers, origin-layer edits)

Status: **Proposed** (2026-10-19). Owner: architect.
Relates to: ADR-0005 (settings as data, explicit save, `ArcSwap<Settings>`, keymaps), ADR-0003 §21 and
`docs/design/browse-mode.md` (browse/focus machine), ADR-0019 (extension platform, data-first add-ons, I-8),
ADR-0021 (`browse.selection_model`), ADR-0022 (presentation, `ReportFlags`), ADR-0024 §2.13 (profiles vs
semantic model, PS-1..PS-6). Design source: `docs/plan/phase-3-features.md` §3 (this ADR supersedes it where
they differ, see §7). Milestone: **M14a** (`docs/plan/phase-3.md`), core slices in §6.

## 1. Context

gorca has one settings file (`gorca.toml`) folded over the `SETTINGS` table, plus a runtime `Overlay` of eight
fixed keys (`crates/gorca-config/src/settings.rs`: `Overlay`, `SettingDef`), published by the Reader as one
`Arc<Settings>` (`gorca-core/src/commands/state.rs` `republish`). There is no per-application variation except
code: the CHR-0 branch adds an interim per-application web-mode table
(`crates/gorca-config/src/app_modes.rs` in worktree `agent-a508eca24e2626204`: `AppModeRow { id, apps, host,
browse: BrowseDefault::{Auto, Off}, why }`, one built-in row `vscode` → `Off`, `resolve(layers, app, host)` with
"later layer wins, host row beats app row", consumed once in `doc_registry.rs` at `HubMsg::Appeared`).

Requirements:

- Owner DP-3 (decisions 2026-10-19): NVDA-style default (browse mode, automatic focus mode on widgets), an
  Orca-style sticky focus mode the user can toggle at any time (`browse.toggle`: gorca+Space in the NVDA
  presets, gorca+A in the Orca presets), per-app presets as **data**, add-ons contribute profile rows as
  data only.
- Owner QA-2 (2026-10-18): verbosity comes as presets in profiles.
- ADR-0024 PS-2: mode defaults are profile keys; the Reader's mode machine decides
  `mode = policy(settings.browse.*, document semantics, user toggle)`.
- Product goals: nothing on the key → speech path may cost IPC or an unbounded computation; a misbehaving
  add-on cannot silence the reader.

### 1.1 Prior art

| System | Model (cited) | Take | Avoid |
|---|---|---|---|
| NVDA | Base config at the bottom, triggered profiles pushed on a stack, a manual profile always on top (`refs/nvda/source/config/__init__.py:883-905` `_triggerProfileEnter` inserts below a manual profile); "changed settings are written to the most recently activated profile" (l.478-480); `BASE_ONLY_SECTIONS` cannot be overridden (l.482-496); `atomicProfileSwitch` batches trigger enter/exit (l.933-970); triggers suspended while NVDA's own dialogs are focused (`appModuleHandler.py:425-434`); triggers `app:` and `sayAll` only (`speech/sayAll.py:483`). Browse mode per app is **code**: `disableBrowseModeByDefault` on app modules (`treeInterceptorHandler.py:59-67`; `appModules/skype.py:12`, `whatsapp_root.py:13`), VS Code blocks the tree interceptor class (`appModules/code.py:19-25`). Pass-through state lives per tree interceptor (`browseMode.py:772-784`). | stack, atomic switch, manual on top, base-only sections, suspension during own modes, per-document toggle state | edits always into the top profile (a rate change in one app silently stays there); per-app browse policy in code; no site/window/context triggers |
| Orca | Layered lookup app → current profile → default profile → schema default (`refs/orca/src/orca/gsettings_registry.py:923-985` `_layered_get`); active app set on script change (`script_manager.py:343-347`); profiles manual only. Web mode: per-app session state `focus_mode_is_sticky`, `browse_mode_is_sticky`, `user_has_overridden_auto_sticky_focus_mode` (`document_presenter.py:97-99, 223-294`); auto sticky focus for "likely Electron" (toolkit `chromium` and not a known browser name, l.175-190) and top-level `http` web apps (l.192-208, 711-727). | per-app layer exists without creating a "profile"; sticky focus as a first-class state; user override remembered per app per session | dconf storage (not hand-editable, not versioned by us); Electron heuristics that flip whole apps (DP-3 rejected this as a default) |
| VoiceOver | Activities: named bundles of settings bound to apps/websites, chosen automatically. | site and app binding of one bundle | opaque storage, no layering (an activity replaces everything) |
| AccessKit / Newton | No settings concept; the consumer owns presentation. | nothing to inherit; the design must not depend on toolkit names (`AppMatcher` reads only identity facts) | — |

## 2. Decision

**Typed layer stack with automatic triggers and origin-layer edits** (option A3 of
`phase-3-features.md` §3.2). Effective settings are precomputed per *match set* and swapped as one `Arc`.

### 2.1 Layers and precedence (lowest first)

| Layer | Source | Writable | Who may set what |
|---|---|---|---|
| L0 Defaults | `SETTINGS` table | no | every key |
| L1 Preset | keyboard preset × layout (`presets/*.toml`) | no | keymap, keyboard-implied defaults |
| L2 Extension defaults | extension schema defaults | no | only `ext.<id>.*` of that extension (ADR-0019 I-8) |
| L3 User base | `~/.config/gorca/gorca.toml` | yes | every key |
| L4 Vendor profiles | extension-shipped `[[profile]]` rows | no (shadowed, §2.5) | only `vendor` keys (§2.3), only apps the extension's `[matcher]` claims |
| L5 User profiles | `~/.config/gorca/profiles/*.toml`, incl. **shadows** of vendor profiles | yes | every non-`Base` key, subject to the trigger/scope matrix (§2.3) |
| L6 Manual profile | any user profile activated by command; at most one | yes (its file) | as L5; survives focus changes |
| (L7) Session deltas | unsaved runtime edits | — | **not a layer above the others**: each writable layer (L3, each L5 file, L6) carries its own delta, folded directly on top of that layer's file values |
| (L8) Safety clamp | — | — | rejects silencing values from L4–L6 (PC-1) |

Correction to `phase-3-features.md` §3.3: a single L7 on top would make a global edit override every profile
and break the origin rule; deltas therefore live *inside* their layer. Fold order:
`L0, L1, L2, L3(file ⊕ Δ), L4 (by specificity), L5 (by specificity, file ⊕ Δ each), L6 (file ⊕ Δ), clamp`.

Inside L4 and inside L5, several profiles may match at once. For each key the most specific profile that sets
it wins (**PM-1** specificity: `Site > Window > App > Context`; ties: longer literal prefix of the matcher, then
profile id order, with one load diagnostic naming both). Any L5 value beats any L4 value (user beats vendor).

### 2.2 Triggers and matching

```rust
// gorca-config::profiles (pure: no tokio, no I/O)
pub enum Trigger {
    App     { app: AppMatcher },
    Window  { app: AppMatcher, title: Glob, kind: Option<WindowKind> }, // Normal | Dialog | Popup
    Context { ctx: FocusContext },
    Site    { app: AppMatcher, host: Glob, path: Option<Glob> },          // documents only
}
/// Identity facts only (never toolkit heuristics). Glob = ext-host matcher semantics:
/// case-insensitive, `*` wildcard; a pattern without `*` is an exact match (the CHR-0 rule).
pub struct AppMatcher { pub name: Vec<Glob>, pub exe: Vec<Glob>, pub desktop_id: Vec<Glob> }
pub enum FocusContext { Terminal, Editable, Document, BrowseMode, FocusMode, Menu, Table, Dialog }
```

- **TR-1 Inputs are cached, never computed per key press.** App: per `AppId` at `Appeared` from
  `AppInfo.app_name`; `exe` from the async `/proc/<pid>/exe` read (deadline 50 ms, Background class; absent =
  no match on `exe`); `desktop_id` when ShellHelper supplies it (M14a-later; absent = no match). Window: per
  (frame node, title generation) from the Reader's active-window record. Context: per focus from the focus
  snapshot's role class and the current `WebMode` (Browse vs Focus). Site: per `DocId` from `DocMeta.url`
  (new; `Document.GetAttributeValue("DocURL")` once per load, Background class, 250 ms deadline; host and path
  only are kept, the URL is never logged, PR-8).
- **TR-2 Compiled index.** `ProfileSet::compile` builds per-kind lookup tables: app matchers → profile ids
  (evaluated once per app), a `[Vec<ProfileId>; N_CONTEXT]` table, window and site glob lists. A focus change
  unions at most four cached id lists and interns the result.
- **TR-3 ProfileKey** = `(vendor: MatchSetId, user: MatchSetId, manual: Option<ProfileId>)`, interned `u32`s.
- **TR-4 Suspension** (NVDA): triggers are not re-evaluated while the Reader is in its own modes
  (`Chooser`, `Prompt`, `InputHelp`); on leaving, the current key is evaluated once.
- **TR-5 Deferred:** a `SayAll` trigger (Phase 4); add-on context tags as `ctx.tag` facts matched by `Context`
  (ADR-0024 PS-4, Phase 4). Nothing in `ProfileKey` prevents either.

### 2.3 Key namespace and the trigger/scope matrix

Every `SettingDef` gains metadata (schema rows, not a file-format change):

```rust
pub struct SettingDef { /* existing: section, key, ty, default, since, apply */
    pub scope: KeyScope,       // who resolves it, see below
    pub vendor: bool,          // L4 may set it (presentation, echo, mode defaults)
    pub clamp: Option<Clamp>,  // PC-1 floor/ceiling for L4-L6
}
pub enum KeyScope {
    Base,  // L0-L3 only: never in any profile
    App,   // resolved per AppId by the actor that consumes it: App triggers (+ manual)
    Doc,   // resolved per (AppId, DocId): App and Site triggers (+ manual)
    Focus, // resolved per ProfileKey by the Reader: all trigger kinds
}
```

| Section | Scope | Vendor | Notes |
|---|---|---|---|
| `keyboard.*`, `keymap.*`, `speech.backend`, `ui.language`, `profiles.*`, every `Apply::Restart` key | Base | no | NVDA `BASE_ONLY_SECTIONS`. Per-app keymaps stay in `[keymap."app:<x>".<ctx>]` (ADR-0005 §5.2). |
| `speech.rate/pitch/volume/voice/language/output_module` | Focus | **no** | voice changes reach the server before the first utterance (SW-3) |
| `speech.punctuation`, `speech.caps*` | Focus | yes | |
| `echo.*`, `editing.*`, `text.*` timings, `verbosity.*`, `report.*` (new, compiled to `ReportFlags`, M22d) | Focus | yes | `editing.word` per app (decision 2026-10-17) |
| `browse.enabled`, `browse.mode_default`, `browse.auto_focus_mode`, `browse.sticky_focus`, `browse.selection_model` (ADR-0021), `browse.say_all_on_load` | **Doc** | yes | mode policy inputs; App part of `browse.enabled` gates mirroring (§2.6) |
| `browse.description_pause`, other `browse.*` presentation | Focus | yes | |
| `terminal.*` | App | yes | read by the terminal arm for that app, focused or not |
| `general.sleep` | App | no | sleep mode persistence (`phase-3-features.md` §8 row 5) |
| `sources.*` (ADR-0024 PS-5) | App/Doc | **no** | opt-ins never come from vendor data |
| `ext.<id>.*` | Focus | own namespace only | defaults at L2 |

- **NS-1 Matrix.** A `Context` or `Window` trigger may set only `Focus` keys; a `Site` trigger `Doc` and `Focus`
  keys; an `App` trigger any non-`Base` key. A row that breaks the matrix is a load diagnostic (key skipped,
  file kept). This makes the dependency graph acyclic: `WebMode` depends only on `Doc` keys, `Context` depends on
  `WebMode`, and `Context` triggers can only change `Focus` keys, so a mode switch can never retrigger the mode
  policy (**INV-P3**).
- **NS-2** Unknown keys get `did_you_mean` diagnostics as in `gorca.toml` (`diag.rs`); a profile never fails as a
  whole for one bad key.

### 2.4 Effective settings, switching and latency

```rust
pub struct Effective {
    pub settings: Settings,
    pub origin: Box<[LayerRef]>,   // per KeyId: which layer supplied the value (PE-1, explain)
    pub report: ReportFlags,       // u64, compiled once (M22d consumes)
}
pub enum LayerRef { Default, Preset, ExtDefault(ExtId), UserBase, Vendor(ProfileId), User(ProfileId), Manual(ProfileId) }
```

- **SW-1 Off the key path.** The Reader keeps `LruMap<ProfileKey, Arc<Effective>>` (64 entries). A focus change
  computes the key from cached inputs (TR-1, TR-2: O(matched profiles)) and swaps the `Arc` into the existing
  `ArcSwap<Settings>` cell. A key press never computes a key; typing echo and caret paths read the current
  `Arc` lock-free.
- **SW-2 Prewarm.** On `Appeared`, on doc load and after every profile reload, the config task folds the likely
  keys (app × {no context, each context with a matching profile}) off the Reader and sends them as
  `SettingsMsg::Prewarm(Vec<(ProfileKey, Arc<Effective>)>)`. A cold miss on the Reader folds inline
  (budget ≤ 200 µs p99; the patches are sparse, ≈ 5–20 µs measured target).
- **SW-3 Voice ordering.** If the new `Effective` changes `speech.{rate,pitch,volume,voice,language}`, the Reader
  emits `SpeechCtl::SetParams` on the ordered speech channel **before** the focus utterance (ADR-0005 §6.2).
- **SW-4 App and Doc keys** are not taken from the focus cell. Consumers call the pure
  `ProfileSet::resolve_app(&AppFacts) -> Arc<AppScoped>` / `resolve_doc(&AppFacts, &DocFacts) -> DocScoped`
  on the compiled set they hold (`Arc<ArcSwap<ProfileSet>>`, published by the config task). Results are
  cached per `AppId` / `DocId` and recomputed on `ProfileSet` generation change.
- **INV-P1** The effective settings of a `ProfileKey` are a pure function of (`ProfileSet` generation, key,
  layer deltas). **INV-P2** At most one fold per (key, generation) on the Reader. **INV-P4** The Reader is the
  single writer of the focus cell and of layer deltas.

State machine (Reader, `gorca-core/src/commands/profiles.rs`):

```rust
enum ProfileState {
    Active    { key: ProfileKey },
    Suspended { key: ProfileKey, why: Suspend },  // Suspend::{OwnMode, User}
    Disabled,                                     // safe mode: L0-L3 (+L3 delta) only
}
```

| From | Event | To | Effect |
|---|---|---|---|
| Active | focus / window title / doc / WebMode change, new key ≠ key | Active | swap `Arc`; SW-3; optional cue `profile.switch` (off) |
| Active | own mode entered | Suspended{OwnMode} | none |
| Suspended{OwnMode} | own mode left | Active | evaluate current key once |
| Active / Suspended{User} | `profiles.suspend` | Suspended{User} / Active | spoken "profiles off" / "profiles on"; key = base only while off |
| Suspended{OwnMode} | `profiles.suspend` | Suspended{User} | spoken |
| any but Disabled | `ProfileSet` reload | same | clear LRU, drop deltas whose file value changed (HR-4 per layer), re-evaluate |
| any | safe mode | Disabled | — |
| Disabled | (none) | Disabled | leaving safe mode is a restart |

### 2.5 Edits: where a change is written

- **PE-1 Origin-layer rule.** A command that changes a setting (rate, punctuation, echo toggles, verbosity,
  "save mode for this site") writes the delta of the layer in `origin[key]`: `Default/Preset/ExtDefault/UserBase`
  → L3; `User(p)` → p; `Manual(p)` → p; `Vendor(v)` → the **shadow** of v (a user L5 file with the same
  triggers and `shadows = "<ext>/<name>"`, created on save, holding only the user's keys).
- **PE-2 Explicit scope.** `profiles.edit_scope_cycle` (provisional chord gorca+Ctrl+Shift+P) cycles the target of
  the *next* edits for this focus: automatic (PE-1) → this application (App profile, created if absent) → this
  site (only in a document; Site profile on the host) → global (L3). It falls back to automatic after 30 s
  without an edit or on app change. A key whose scope forbids the target (NS-1) uses the nearest allowed one and
  says so.
- **PE-3 Every edit is audible or explained (INV-P5).** After the edit the Reader re-resolves the current key; if
  the effective value did not change (e.g. a global edit while an App profile sets the key) the confirmation
  says where the value comes from ("rate 60 globally; Terminal profile keeps 45").
- **PE-4 Explain.** `settings.explain_current` speaks the active profiles in precedence order and, with a key in
  review, its origin layer. The settings app (M14) shows the origin per value.
- **PE-5 Save** (`settings.save`, ADR-0005 explicit save) writes every dirty delta to its own file with
  `toml_edit` (comments kept), atomic rename, content hash checked against the loaded text (a file changed on
  disk since load is not overwritten; the user hears "profile <name> changed on disk, reload first").
  **Revert** clears all deltas. The daemon's config task is the only writer (ADR-0005 §7.6).
- **PE-6 Session mode state is not a setting.** The browse/focus toggle (gorca+Space / gorca+A) stays reader
  state per document, sticky (`FocusCause::Manual`, only the toggle leaves). `browse.save_mode_default`
  (chord owner question Q-P4) writes the *current* mode as `browse.mode_default` through PE-1/PE-2 with the
  default target "this site" in a browser and "this application" elsewhere.

### 2.6 Browse-mode keys (ADR-0024 PS-2, owner DP-3)

| Key | Values | Default | Meaning |
|---|---|---|---|
| `browse.enabled` | bool | true | false = no virtual buffer; the reader follows focus (CHR-0 `Off`). At App resolution it also means "do not mirror documents of this app" (doc registry); at Site resolution the doc is mirrored but starts and stays in focus mode. |
| `browse.mode_default` | `browse` \| `focus` | `browse` | mode when a document first gets focus in the session. `focus` enters `FocusCause::Manual` (sticky: auto focus rules and Escape do not leave it). |
| `browse.auto_focus_mode` | `widgets` \| `never` | `widgets` | NVDA `autoPassThroughOnFocusChange` (`configSpec.py:220`) with gorca's D-1 role sets. |
| `browse.sticky_focus` | bool | false | false: a user toggle is remembered per document (NVDA). true: per application for the session (Orca `focus_mode_is_sticky`), so new documents of the app open in the toggled mode. |

`role=application` and `aria-busy` remain model inputs (ADR-0024 PS-2), never settings. The policy is the pure
function `WebPolicy::initial(doc: &DocScoped, sem: &DocSemantics, session: &SessionMode) -> WebMode` in
`browse/`, with the user toggle always above the settings.

### 2.7 Extension contributions (vendor profiles; owner: data only)

```toml
# extensions/<ext>/profiles.toml in M14a (no ext-host change); folded into ext.toml `[[profile]]` by M18a
schema = 1
[[profile]]
name = "vscode"                       # id "<ext>/vscode"; display name from the extension catalog
why = "editor built from text widgets with its own key bindings (NVDA appModules/code.py)"
triggers = [{ app = { name = ["code", "visual studio code", "code - oss", "vscodium", "cursor"] } }]
[profile.values]
browse.enabled = false
```

- **PF-1** Only `vendor = true` keys are accepted; voice, rate, volume, pitch, keyboard, backend, language and
  `sources.*` never. A rejected key is a diagnostic.
- **PF-2** User controls: `profiles.vendor = { "ext-chromium/vscode" = false }` in L3 disables one row; shadows
  override single keys (PE-1); disabling the extension removes its rows.
- **PF-3 No auto-activation.** Add-ons cannot activate, deactivate or rank profiles at run time and cannot declare
  a manual profile. Their triggers are declarative and evaluated by the core.
- **PF-4 Confinement.** A vendor trigger is intersected with the extension's `[matcher]`: an add-on's row cannot
  match an application its manifest does not claim (a terminal add-on cannot change Firefox).
- **PF-5 Limits.** 8 profiles × 64 keys per extension, 16 matcher globs per trigger; over-limit rows are dropped
  with a diagnostic.

### 2.8 Persistence format

- `~/.config/gorca/profiles/<id>.toml`, one profile per file, id = file stem (stable across renames):

```toml
schema = 1
[profile]
name = "Telegram web"                 # display name, free text
manual = false                        # true: never triggered, only manual activation
shadows = ""                          # "<ext>/<name>" for a shadow of a vendor profile
[[profile.trigger]]
site = { app = { name = ["firefox*", "chromium*", "google-chrome*"] }, host = "web.telegram.org" }
[speech]
punctuation = "some"
[browse]
mode_default = "focus"
```

- Sections and keys are those of `gorca.toml` (only overridden keys). Hot reload by the config task's inotify
  watch on the directory (debounce 200 ms); migrations per PR-9 / CF-8 (one schema bump per merge).
- **PC-1 Safety clamp.** L4–L6 may not set `speech.volume < 20`, `speech.rate` outside 5–95 or any value that
  turns speech off; the lower layer's value is kept and the user hears once "profile <name>: volume ignored".
  A command that would write such a value into a profile refuses and offers the global target. Silencing an app
  is sleep mode (`general.sleep`), which is explicit.
- **PC-2 Safe mode** (M11) loads L0–L3 only. **PC-3** At most 256 profile files and 1 MiB per file
  (`MAX_FILE_BYTES`); more is a diagnostic, never a failure.
- **PC-4 Privacy.** Site profiles store host and path globs the user wrote; no URL history is kept.

### 2.9 Migration of the CHR-0 interim table

| CHR-0 (`app_modes.rs`) | ADR-0016 |
|---|---|
| `AppModeRow.apps` (exact, case-insensitive) | `AppMatcher.name` globs without `*` (same semantics) |
| `host: Option<String>` | `Trigger::Site { host }` (exact host = glob without `*`) |
| `BrowseDefault::Auto` | no row (L0 defaults: `browse.enabled = true`, `auto_focus_mode = "widgets"`) |
| `BrowseDefault::Off` | `browse.enabled = false` |
| `layers` "later layer wins" | L4 < L5 < L6 |
| "host row beats app row" | PM-1 `Site > App` |
| `id`, `why` | vendor profile `name`, `why` (shown by `settings.explain_current`) |
| `builtin_rows()` (`vscode`) | `extensions/ext-chromium/profiles.toml` (data-only first-party vendor profile) |
| `browse_default(&info.app_name)` in `doc_registry.rs` | `profile_set.resolve_app(&facts).browse_enabled()` |

The migration slice (P6) keeps a parity test: for every CHR-0 test case and the shipped row the new resolver
gives the same decision. `app_modes.rs` is then deleted. CHR-0 needs no change before it merges.

### 2.10 Failure modes

| Failure | Behaviour |
|---|---|
| bad profile file / bad key | key or row skipped with diagnostic; rest loads (CV-1) |
| profile directory unreadable | L0–L3 only, one spoken notice |
| `/proc` exe read or `DocURL` read times out | that matcher input is absent: no match (never blocks the switch) |
| profile thrash (popups, title churn) | switch cost is an `Arc` swap; voice changes are audible, so Shell popups get no profile by default and window title generations are debounced 100 ms |
| vendor row silences or misroutes | PC-1 clamp, PF-1 key filter, PF-4 confinement, `profiles.suspend` escape (FP-5) |
| file edited on disk while deltas are dirty | reload wins per key (HR-4); save refuses on hash mismatch |

## 3. Alternatives

| Option | Why not |
|---|---|
| A1 NVDA stack (edits into the top profile) | surprising edits (the long-standing NVDA confusion); no add-on rows; no site triggers |
| A2 Orca layers (app → profile → default, manual profiles) | no window/site/context triggers; no add-on rows; dconf storage |
| CSS-like cascade (per-key selectors over roles/nodes) | per-node evaluation cost on presentation paths; per-node variability belongs to the semantic model (ADR-0024 PS-6), not settings |
| One flat "effective settings" per app computed by each actor | duplicates the fold in several actors, violates the single-writer rule, and cannot express Context/Site |
| Bake-off | not contested: the choice is about semantics and UX, measurable only by the P1/P2 gates, which A3 meets by construction (`Arc` swap) |

## 4. Consequences

- `gorca-config` gains `profiles/` (pure) and `SettingDef` metadata; `Overlay` is replaced by a sparse
  `SettingsPatch` delta per layer (the existing `Overlay` API stays as a façade until callers move).
- The Reader gains `ProfileState` and the LRU; the doc registry and terminal arm read App/Doc keys from the
  compiled `ProfileSet` instead of static tables.
- Per-app behaviour that today needs code (VS Code without a buffer) becomes data; NVDA's `disableBrowseModeByDefault`
  list can be ported as vendor rows after measurement.
- New concepts for users: "where does this setting come from" — mitigated by PE-3 and PE-4.
- `phase-3-features.md` §3.3 L7 wording is corrected (§2.1); `browse.sticky_focus` gets the meaning in §2.6
  (ADR-0024 PS-2 named it without a definition).

## 5. Metrics to validate

| Id | Stimulus → end | Target |
|---|---|---|
| **P1** | focus change into an app with a matching profile (voice change included) → first SPEAK | Δp99 ≤ 0.2 ms vs no profiles; key resolution ≤ 20 µs p99 on a hit, fold ≤ 200 µs p99 on a miss |
| P2 | 200 profiles × 4 triggers loaded | compile ≤ 50 ms on the config task; P1 and K2 unchanged |
| P3 | edit command → confirmation SPEAK | ≤ 5 ms p99 gorca-side; INV-P5 property test (no silent edit) |
| P4 | 100 reloads/s of a profile file (inotify storm) | K2 p99 unchanged; at most one compile per 200 ms |
| P5 | migration parity | 100 % of CHR-0 cases and the shipped row decide identically |
| P6 | property tests | fold = specification oracle (random sets of ≤ 32 profiles, all trigger kinds); NS-1 matrix exhaustive; PE-1 target table exhaustive |

## 6. Implementation slices (M14a-core; ≤ 2 days each, no GUI)

None touches M22a files (`gorca-model/src/{sem,fact.rs}`, `gorca-fuse`, `gorca-fact-api`, `gorca-sem-atspi`,
`gorca-ext-api/src/codes.rs`, `gorca-testkit/src/{fake_source,fact_conformance}.rs`, M22a benches) or CHR-0-owned
files (`doc_registry.rs`, `browse/commands.rs`, `doc_actor/`, `app_actor/doc_route.rs`, doc-source crates).

| Id | Slice | Owner | Days | Depends on | Files | Free slot now? |
|---|---|---|---|---|---|---|
| **P1** | `KeyId` registry over `SETTINGS`; `SettingDef.scope/vendor/clamp`; typed `Value`; `SettingsPatch` with `apply`/`drop_changed` (generalises `Overlay`, façade kept); `load.rs` `apply_key` writes through `Value`; tests: every row round-trips | rust-engineer | 2 | — | `gorca-config/src/{settings.rs,load.rs,patch.rs}` | **yes** |
| **P2** | profile file + vendor `profiles.toml` parsers, `Trigger`, `AppMatcher`/`Glob` (shared semantics with ext-host matcher), NS-1 matrix, PF-1/PF-4/PF-5, PC-1/PC-3 diagnostics; `ProfileSet::compile` index (TR-2); fuzz target for the parser | rust-engineer | 2 | P1 | `gorca-config/src/profiles/{mod,trigger,file,compile}.rs`, `fuzz/` target | **yes** |
| **P3** | resolver: match sets, interning, fold with `origin`, `resolve_app/resolve_doc`, LRU type; PE-1/PE-2 `plan_edit`; save of deltas into profile and shadow files (`save.rs`); property tests P6 against a Python oracle | rust-engineer + math-researcher (oracle) | 2 | P2 | `gorca-config/src/profiles/{resolve,edit}.rs`, `save.rs`, `tools/research/profiles_oracle.py` | **yes** |
| P4 | daemon config task: profiles dir load, inotify, prewarm (SW-2), `ProfileSet` `ArcSwap`; Reader `ProfileState` machine, key computation from focus/window/WebMode, SW-3 ordering, `profiles.suspend`, `settings.explain_current`, `profiles.edit_scope_cycle`, catalog strings | rust-engineer | 2 | P3 | `gorca-daemon/src/config_task.rs`, `gorca-core/src/commands/{profiles.rs,state.rs,handlers.rs}`, focus hook in `reader.rs`, `gorca-i18n` catalogs (append) | yes, if no other branch holds `commands/state.rs` |
| P5 | gates P1–P4 bench (`gorca-bench/benches/profiles.rs`) + e2e: terminal App profile `punctuation=most`, voice-change profile ordering | perf-engineer, test-engineer | 1.5 | P4 | `crates/gorca-bench/benches/profiles.rs`, `bench/e2e` scenario | yes |
| P6 | browse keys (§2.6) + CHR-0 migration (§2.9): `ext-chromium/profiles.toml`, doc registry `resolve_app`, `WebPolicy::initial`, `browse.sticky_focus`, `browse.save_mode_default`, parity test P5; delete `app_modes.rs` | rust-engineer (+ web-a11y-engineer review) | 2 | P4, **CHR-0 merged** | `doc_registry.rs`, `browse/{mod,commands,policy}.rs`, `extensions/ext-chromium/` | after CHR-0; before M22a wave C or rebased on it |
| P7 | inputs: `AppInfo.exe` (async `/proc`), `DocMeta.url` (Site triggers), window title generations; `desktop_id` when ShellHelper exposes it | linux-platform-engineer | 2 | P4 | connector `AppInfo`, doc source (`DocURL`), `present/window.rs` | after M22a wave C (doc-source crates) |
| P8 | adversarial review + fix round, decision row | reviewer, then rust-engineer | 2 | P5, P6 | — | — |

Critical path P1 → P2 → P3 → P4 → P6 → P8 ≈ 12 days; P1–P3 fit the free cargo slot of M22a wave A now (one cargo
build per agent, `gorca-config` only). Site triggers work end-to-end only after P7; until then `Site` rows load
and are reported as "waiting for document address" by `settings.explain_current`.

## 7. Decisions pending (owner)

| # | Question | Options | Recommended |
|---|---|---|---|
| Q-P1 (= QF-1) | A setting changed with a key while an app profile is active: where does it go? | (a) origin layer (global unless the profile sets it), plus a scope key; (b) always the active profile (NVDA); (c) always global | **(a)** |
| Q-P2 (= QF-2) | Extension-shipped profiles (VS Code without browse mode, terminal punctuation) | (a) on by default, presentation and mode keys only, one switch each; (b) off until enabled | **(a)** (needed for DP-3 "VS Code no browse" by default) |
| Q-P3 | Scope of the sticky focus toggle by default | (a) per document (NVDA); (b) per application for the session (Orca) | **(a)**, with `browse.sticky_focus = true` per app profile for messengers |
| Q-P4 | Chord for "save the current mode as the default here" (`browse.save_mode_default`) | (a) gorca+Space pressed 3 times (NVDA-preset; Orca: gorca+A 3 times); (b) only via the command list (M15a) | **(a)** if the triple press is free in both presets, else (b) |
| Q-P5 | Manual profile switching before the chooser (M15a) exists | (a) `profiles.manual_cycle` command now; (b) wait for M15a | **(b)**: automatic triggers cover the owner's cases; the chooser lists profiles later |
