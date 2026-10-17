# Phase 3 product features: profiles, help, lists, dictionaries, audio cues, review everywhere

Status: **Proposed** (2026-10-17). Owner: architect. Companion to `docs/plan/phase-3.md` (milestones,
gates, rules PR-1…PR-12); this document designs the product-feature rows added there on 2026-10-16/17
and places each one in a milestone. Decisions taken from it go to `docs/decisions.md`; the questions in
§14 block only the items named there.

Inputs: `CLAUDE.md`; `docs/plan/phase-3.md`; `docs/decisions.md` (through 2026-10-17); ADR-0001 (§8
output, §10 extensions); ADR-0002 (SE-5 earcon, §9.1 semantic find, `Mode::Find`); ADR-0005 (commands,
settings layering §7.2, overlay §7.6, learn mode §5.4); ADR-0006 (review, object navigation, flat
review); ADR-0007 (verbaliser §3.2, typing echo); ADR-0008 (+ Amendments 1–2); `docs/design/extensions.md`;
`docs/design/modes.md`; `docs/plan/m8-spec.md` §3.11–3.12; `docs/config-reference.md`; branch
`worktree-punct-symbols` (commit `5bc7e6a`, per-language symbol tables); prior art in `refs/nvda`,
`refs/orca`, `refs/speech-dispatcher`, `refs/voicego`, `refs/mutter`, `refs/gnome-shell` (cited inline).

## 0. Summary

| Area | Recommendation | Core / extension / config | Milestone | ADR |
|---|---|---|---|---|
| A. Configuration profiles and verbosity | Layered settings with automatic profiles by app, window, context and site; edits go to the layer that supplied the value; extensions ship **data-only** vendor profiles restricted to presentation keys | core mechanism; extensions contribute data; files in `~/.config/gorca/profiles/` | **M14a** (new, before the GUI) | ADR-0016 (new) |
| B. Input help and localised strings | Every command, key name and help text is a catalog id; learn mode and a filterable command list read them in the UI language | core + catalogs; extensions ship their own catalogs | M12a (ids), M12 (catalogs), M15a (command list) | ADR-0010 |
| C. Lists, find, copy, clipboard | In-reader `Mode::Chooser` and `Mode::Prompt` (spoken, brailled, zero IPC), no GUI window; clipboard through `org.gorca.ShellHelper` | core modes; sources from core and extensions | **M15a** (modes, pulled forward), M15 (browse uses) | ADR-0005 amendment |
| D. Pronunciation and symbol dictionaries | Temporary → profile → voice → user → extension → shipped dictionaries, then per-language symbol tables (generalising the current punctuation branch); NVDA/Orca import as a user action | `gorca-speech` verbaliser; data files | M13 | ADR-0017 (new) |
| E. Audio cues, capitals, lock keys, indentation | `CueSink` beside `SpeechClient`; pre-rendered soft earcon theme; capitals by per-segment pitch (prosody) plus optional earcon; lock-key announcements now | `gorca-output-api` + sink backends; theme as data | F0 (lock keys, pitch), M13 (cues) | ADR-0017 |
| F. Other NVDA/Orca features | Ranked list §8; quick wins in F0 | mostly core commands | F0, M13–M16 | — |
| G. Settings UI boundary | `org.gorca.Control1` exposes layers, profiles, dictionaries, cues, commands, a speech-viewer stream; GUI is a client | daemon control task | M14 | ADR-0011 |
| H. Review, object navigation and the mouse everywhere | One review cursor over four line sources (geometry, terminal, document, text), NVDA numpad set complete; pointer actions through ShellHelper's Clutter virtual pointer, AT-SPI actions first | core review + ShellHelper | **M21** (new, Wave 2) | ADR-0006 amendment 2 |
| I. Graph and vector navigation (later) | Review layer exposes stable identity, paths, feature records and a graph snapshot API now; nothing semantic is built | core read-only API | M21 (seams only), Phase 4 (ADR-0002 MP) | ADR-0002 amendment |

Start order (§12): **F0 quick wins and M15a modes first**, then M14a profiles, M13 cues/dictionaries,
M21 review completeness, M15 browse lists; GUI (M14) and SDK (M18) consume them.

## 1. Starting point (main at `bc4cb77`, 2026-10-17)

Exists: learn mode (`Mode::InputHelp`, ADR-0005 §5.4) with English `CmdSpec.help: &'static str`;
`speech.repeat_last`; `info.time_date`; `info.formatting`; `info.status_bar`; pass-next-key;
`speech.caps = pitch|say|off` where **pitch is a no-op** (`gorca-speech/src/verbalize.rs`: the
server cannot be told); settings layers defaults → preset → user file → 8-key runtime overlay;
extension manifests with app matchers (`app_name`, `toolkit`, `connector`) and a settings KV; review
and object navigation with per-surface flat review (ADR-0006, M4a); terminal and document line sources.

In flight: per-language symbol tables (`worktree-punct-symbols`, `5bc7e6a`: `Lang {En, Ru}`,
NVDA-style levels and preserve, `symbols-{en,ru}.dic` with `@word.*` UI words inside the data).

Missing: profiles; i18n (every user string is an English literal); earcons (neither SSIP nor voicego
backend sends `SOUND_ICON`; voicego speaks the icon name, `refs/voicego/internal/server/session.go:820-845`);
per-segment pitch; lock-key state; speech history beyond one utterance; clipboard; chooser/list mode;
`AppInfo` has no executable, desktop id or window class; the doc mirror does not keep the document URL
(only `RouteChanged{url}`); pointer actions of any kind.

## 2. Principles for every feature here

- **FP-1 Mechanism in the core, policy as data.** Profiles, dictionaries, symbol tables, earcon
  themes, catalogs and keymaps are versioned data files (PR-9). Extensions contribute *data* through
  their manifest and *behaviour* only through existing hooks; no extension can change another
  layer's settings at run time.
- **FP-2 Nothing new on the keypress → speech path costs IPC.** Profile switch, dictionary
  application, catalog lookup, cue dispatch and history append are in-memory; each has a Δp99 gate
  against K2/L1h/FC (§10).
- **FP-3 One list UI.** Elements list, find history, command list, speech history, notifications,
  voices, profiles and extension menus are sources of the same `Mode::Chooser`; text entry is the same
  `Mode::Prompt`. No second list implementation, no GUI window from the daemon.
- **FP-4 Every string is a catalog id** (PR-7) and every spoken string carries its language tag, so
  the UI-language voice reads it (M12 switching).
- **FP-5 A feature that can silence the user ships its escape** (PR-12): profiles have
  `profiles.suspend`, dictionaries `speech.dictionaries_toggle`, cues fall back to words, safe mode
  ignores all three.

---

## 3. Area A: configuration profiles and verbosity (ADR-0016, M14a)

### 3.1 Prior art and its failures

| System | Model | Lesson |
|---|---|---|
| NVDA | Base config at the bottom of a stack, triggered profiles inserted below the single manual profile, the newest wins; changes are saved into the **top** profile (`refs/nvda/source/config/__init__.py:472-503`, `_triggerProfileEnter` l.883, `manualActivateProfile` l.688). Triggers: `app:<name>` (`appModuleHandler.py:927`) and `sayAll` (`speech/sayAll.py:483`) only; switching is atomic on app focus change (`appModuleHandler.py:430-446`) and suspended while NVDA's own dialogs are focused (l.425-451). Some sections are base-only (`config/__init__.py:481`). Files `profiles/<name>.ini` + `profileTriggers.ini`. | **Take** the stack, atomic switch, manual profile on top, base-only sections. **Avoid** "edits go to the top profile": a rate change made in Word silently stays in Word, a long-standing user confusion. No role/site triggers. |
| Orca | dconf layers: app → current profile → default profile → schema default (`refs/orca/src/orca/gsettings_registry.py` `_layered_get` ~l.930-978); app layer chosen on active-script change (`script_manager.py:343-347`); profiles are manual only (`profile_manager.py:156, 271`). | **Take** per-app layer always available without creating a "profile". **Avoid** dconf as storage (not hand-editable, not versioned by us). |

### 3.2 Options

| Option | Summary | Trade-off |
|---|---|---|
| A1 NVDA stack | profiles as overrides, edits always into the top active profile | familiar to NVDA users; surprising edits; add-ons cannot ship profiles |
| A2 Orca layers | fixed layers app → profile → default, manual profile switch | simple; no window/site/context triggers; no add-on profiles |
| **A3 Typed layer stack with origin-layer edits (recommended)** | fixed layer kinds, at most one active per kind, automatic triggers by app/window/context/site; an edit goes to the layer that supplied the current value | predictable precedence, add-on profiles possible, slightly more concepts (explained by "where am I in settings": gorca+Shift+F1, §3.6) |

### 3.3 Decision: layer stack (lowest first)

```
L0 Defaults          SETTINGS table (read-only)
L1 Presets           keyboard preset × layout (read-only)
L2 Ext defaults      extension manifests: own namespace ext.<name>.* only (read-only)
L3 User base         gorca.toml
L4 Vendor profiles   extension-shipped profiles, presentation keys only (read-only, can be disabled)
L5 User profiles     ~/.config/gorca/profiles/*.toml, automatic triggers; includes user overrides
                     of vendor profiles ("shadow" files, same trigger)
L6 Manual profile    at most one, activated by command/chooser, survives focus changes
L7 Session overlay   runtime changes not yet saved, one overlay per writable layer (L3, L5 entries, L6)
L8 Safety clamp      not a layer of values: rejects silencing combinations from L4-L6 (PF-7)
```

Within L5, several user profiles may match at once; they stack by trigger specificity (§3.4), each key
taken from the most specific profile that sets it.

**Effective settings** = fold of L0…L7 → `Arc<Settings>` (the existing `ArcSwap<Settings>` cell,
ADR-0005 §6.3; the Reader stays the single writer).

### 3.4 Triggers and matching

```rust
// gorca-config (pure)
pub enum Trigger {
    App     { app: AppMatcher },                       // desktop id | AT-SPI app name | exe basename (globs)
    Window  { app: AppMatcher, title: Glob, kind: Option<WindowKind> },   // kind: Normal | Dialog | Popup
    Context { ctx: FocusContext },                     // Terminal | Editable | DocumentWeb | Menu | Table | ...
    Site    { app: AppMatcher, host: Glob, path: Option<Glob> },          // browser documents only
}
pub struct Profile { pub id: ProfileId, pub name: String, pub triggers: Vec<Trigger>,
                     pub values: SettingsPatch, pub origin: Origin /* User | Vendor{ext} | Shadow{ext} */ }
```

- **PM-1 Specificity order** (most specific wins per key): Site > Window > App > Context. Ties inside one
  kind: longest literal glob prefix, then file name order, with one diagnostic.
- **PM-2 Matching inputs are computed once and cached**, never per key press: app match per `AppId` at
  attach (needs new `AppInfo.desktop_id` from ShellHelper's `Shell.WindowTracker` and
  `AppInfo.exe` from the async `/proc` read, R4-3); window match per (window node, title generation);
  context from the focus snapshot's role class; site per `DocId` from the document URL (new
  `DocMeta.url`, AT-SPI `Document.GetAttributeValue("DocURL")` once per load, Background class).
- **PM-3 ProfileKey** = `(app_profile, window_profile, context_profile, site_profile, manual)` as small ids.
  The Reader keeps `LruMap<ProfileKey, Arc<Settings>>` (64 entries); a focus change computes the key
  (O(1) cached lookups) and swaps the `Arc`. A miss folds the layers inline (≈ 5–20 µs; the patch is a
  sparse table).
- **PM-4 Ordering with speech.** When the new effective settings change voice parameters, the Reader emits
  `SpeechCtl::SetParams` **before** the focus utterance on the same ordered channel (ADR-0005 §6.2), so
  the first word in the new app already uses the profile's voice.
- **PM-5 Suspension** (NVDA l.425-451): triggers are not re-evaluated while gorca's own modes
  (`Chooser`, `Prompt`, `InputHelp`) are active.
- **PM-6 Say-all trigger** (NVDA `sayAll`) is deferred to Phase 4; nothing in the key prevents it.

### 3.5 State machine

```rust
enum ProfileState {
    Idle { active: ProfileKey },                  // effective = cache[active]
    Suspended { active: ProfileKey, resume_key: ProfileKey }, // own modes, or profiles.suspend (session)
    Disabled,                                     // safe mode: only L0-L3 + L7
}
```

| From | Event | To | Effect |
|---|---|---|---|
| Idle | focus/window/doc change, key differs | Idle | swap Arc; SetParams if voice changed; recompile `ReportFlags`; optional cue `profile.switch` (off by default) |
| Idle | own mode entered | Suspended | none |
| Suspended | own mode left | Idle | evaluate the current key |
| any | `profiles.suspend` | Suspended / Idle (toggle) | spoken "profiles off/on" |
| any | profile file reload | same | invalidate the LRU; re-evaluate |
| any | safe mode | Disabled | — |

### 3.6 Edits, save and explanation

- **PE-1 Origin-layer rule.** A command that changes a setting (gorca+P, rate, echo toggles) edits the
  overlay of the layer that supplied the current value: L3 when it came from defaults or the base file;
  the user profile that set it; the **shadow** of a vendor profile when the vendor profile set it (the
  shadow file is created on save). So a rate change in the terminal stays global unless a terminal
  profile already sets the rate.
- **PE-2 Explicit scope.** `profiles.edit_scope_cycle` (gorca+Ctrl+Shift+P, provisional) cycles the edit
  target for the next changes: "automatic" (PE-1) → "this application" (creates/edits the app profile)
  → "global". Confirmations name a non-global target once: "punctuation most, for Terminal".
- **PE-3 Explain.** `settings.explain_current` (gorca+Shift+F1 ×1 in learn-mode style, provisional)
  speaks the active profiles; in the settings app each value shows its source layer.
- **PE-4 Save** (`settings.save`, ADR-0005 HR-5) writes every dirty overlay to its own file
  (`toml_edit`, atomic, hash check). **Revert** clears all overlays.
- **PE-5 Base-only keys** (cannot be set by L4–L6): `keyboard.*` (modifiers, preset, layout), `speech.backend`,
  `ui.language`, everything marked `Restart`. Keymap changes per app stay in `[keymap."app:<x>".<ctx>]`
  (ADR-0005 §5.2), not in profiles.

### 3.7 Extension contributions (vendor profiles)

```toml
# ext.toml (WIT/manifest 0.9, M18; first-party extensions may use it from M14a)
[[profile]]
name = "terminal-defaults"            # catalog id for the display name: ext-vte-profile-terminal
triggers = [{ context = "terminal" }]
[profile.values]
speech.punctuation = "most"
report.indentation = "tones"
```

- **PF-1** Values are validated by `gorca-config` like user files; only keys flagged
  `SettingDef.vendor_profileable` are accepted (presentation and echo: `verbosity.*`, `report.*`,
  `speech.punctuation`, `speech.caps`, `echo.*`, `terminal.*`, `browse.*`, `text.*` timings). Never voice,
  rate, volume, pitch, keyboard, backend, language. A rejected key is a load diagnostic, not a failure.
- **PF-2** The user disables a vendor profile with `profiles.vendor = { "ext-vte/terminal-defaults" = false }`
  in the base file, or overrides single keys in its shadow (PE-1).
- **PF-3** An extension cannot activate a profile at run time. A later WIT hook may set a *context tag*
  (for example a web app inside a browser) that a user or vendor `Context` trigger matches; Phase 4.
- **PF-4** Vendor profiles count toward manifest limits (8 profiles, 64 keys each).

### 3.8 Verbosity model inside profiles

Today: `verbosity.level = brief|normal|verbose`. Added: a `[report]` section of independent toggles
(NVDA "Object presentation" and "Document formatting"), compiled to `ReportFlags` (`u64`) once per
effective settings; presenters test bits, zero cost.

```toml
[report]
position_info = true      # "3 of 12"
descriptions = true
shortcuts = true
tooltips = true
notifications = true
role = "full"             # full | brief (abbreviated roles from the catalog)
state_changes = true
formatting_changes = false   # font/size/bold changes while reading (NVDA documentFormatting)
indentation = "off"       # off | speech | tones | both   (NVDA reportLineIndentation, configSpec.py:262)
line_numbers = false
spelling_errors = true
heading_levels = true
table_headers = "rows_columns"   # off | rows | columns | rows_columns
links_visited = true
```

`verbosity.level` becomes a preset of these flags (a key explicitly set in `[report]` overrides the
level). Per-role tables are Phase 4.

### 3.9 File format and safety

`~/.config/gorca/profiles/<file>.toml`, `schema = 1`, `[profile] name`, `[[profile.trigger]]`, then the
same sections as `gorca.toml` (only overridden keys). Hot reload by the existing inotify task (directory
watch). PR-9 migrations apply.

- **PF-7 Safety clamp.** L4–L6 may not set `speech.volume < 20`, `speech.rate` outside 5–95, or a speech
  mode of off; the clamp keeps the lower layer's value and says once "profile <name>: volume ignored".
  Silencing an application is sleep mode (§8), which is explicit.
- **PF-8 Safe mode** (M11) loads L0–L3 only.

### 3.10 Budgets and gates

| Gate | Stimulus → end | Target / ceiling |
|---|---|---|
| **P1** | focus change into an app with a matching profile (voice change included) → first SPEAK | Δp99 ≤ 0.2 ms vs the same focus without profiles; resolution ≤ 20 µs p99 on a cache hit, ≤ 200 µs on a miss |
| P2 | 200 profiles × 4 triggers loaded | load ≤ 50 ms on the config task; P1 unchanged |

**Risks.** Desktop id availability (ShellHelper absent → fall back to AT-SPI app name); URL privacy (site
profiles store host globs only in the user's own config; the URL is never logged, PR-8); profile
thrash in transient Shell popups (cheap by construction, voice changes are audible: Shell popups get no
profile by default).

---

## 4. Area B: input help and localised strings (ADR-0010; M12a, M12, M15a)

### 4.1 Prior art

NVDA input help speaks `script.__doc__`, set by `@script(description=_("…"))`, so descriptions are
gettext-localised (`refs/nvda/source/inputCore.py:656-680`; toggle `globalCommands.py:266`). Orca's learn
mode also lists all shortcuts (`refs/orca/src/orca/learn_mode_presenter.py`, `list_orca_shortcuts` l.185).

### 4.2 Design

- **IH-1** `CmdSpec.help: &'static str` becomes `help: MsgId` (generated `cmd-<name>` and, for multi-press
  commands, `cmd-<name>-2`, `cmd-<name>-3`). Learn mode (ADR-0005 LM-2) speaks
  `"<chord in words>: <help>"` with the chord rendered from **localised key names** (`key-<keysym>`
  ids; modifier names per preset). Extensions: manifest `help = "say-prompt"` is an id into the
  extension's `i18n/<lang>/ext.ftl`; a literal is accepted until WIT 1.0 with a deprecation diagnostic.
- **IH-2 Command list** (`input.command_list`, gorca+1 ×2 while in learn mode, or unbound chord): a
  `Mode::Chooser` source (§5) listing every command available in the current context (`(mode, app)` lookup
  order, ADR-0005 §5.2) with chord and help, filterable; Enter **runs** the command after the chooser
  closes (the original context is restored first, then the command is dispatched with a new epoch);
  Shift+Enter speaks the help only.
- **IH-3 Language.** `ui.language = ""` (from `LC_MESSAGES`/`LANG`, overridable). Catalog swap is an
  `ArcSwap` (≤ 100 ms after a change, I1 gate). UI messages carry `lang = ui.language`, so with
  automatic voice switching (M12) Russian help is read by the Russian voice even when speech content
  is English.
- **IH-4 Data vs messages.** The `@word.*` and `@level.*` rows currently inside `symbols-<lang>.dic` move to
  the catalog in M12a; symbol names stay verbaliser data (§6). Translators edit two file kinds:
  `i18n/<lang>/*.ftl` and `data/symbols-<lang>.dic`.
- **IH-5 Braille.** Help and chooser items go to braille through the same segments (M10).

Budgets: learn-mode description is the K2 path (zero IPC); pre-resolved Fluent patterns, formatting ≤ 2 µs.

---

## 5. Area C: chooser, prompt, elements list, find, copy, clipboard (M15a, M15)

### 5.1 Options for the list UI

| Option | Summary | Trade-off |
|---|---|---|
| **C1 In-reader spoken list (recommended)** | `Mode::Chooser`: gorca grabs the keyboard (as learn mode), keeps a filter buffer from `KeyEvent.unichar`, speaks and brailles the current item | instant (no window, no focus change in the browser, no IPC), works identically in every app and over braille; no IME or dead keys in the filter (Phase 3), invisible to a sighted helper (speech viewer covers it) |
| C2 GTK window per list (NVDA `ElementsListDialog`, `refs/nvda/source/browseMode.py:1454`) | resident `gorca-ui` process gets the list over `org.gorca.Control1` and shows a dialog | familiar to NVDA users, full text entry, visible to helpers; Wayland focus needs an activation token the daemon does not have (P-13), the browser loses focus and refires focus/document events on return (virtual cursor restore risk), 100–300 ms to show, 10k items over D-Bus |
| C3 Hybrid | C1 always; C2 as "show in window" later for helpers | most work; defer the window to Phase 4 |

### 5.2 `Mode::Chooser` and `Mode::Prompt` (ADR-0005 amendment)

```rust
pub enum Mode { /* … */ Chooser { id: ChooserId, resume: BaseMode }, Prompt { id: PromptId, resume: BaseMode } }

pub trait ChooserSource: Send + Sync {           // core trait; sources live in core or come from extensions
    fn title(&self) -> MsgId;
    fn categories(&self) -> &[Category];          // e.g. headings, links, landmarks, fields, buttons, tables
    fn items(&self, cat: CatIdx) -> ItemsPage;    // Ready(Arc<[ChooserItem]>) | Pending(ReqId)
    fn actions(&self, item: &ChooserItem) -> &[ChooserAction];   // Move | Activate | Copy | Run | Delete ...
    fn act(&self, item: &ChooserItem, a: ChooserAction) -> ReaderEffect;  // applied after the mode closes
}
pub struct ChooserItem { pub label: Arc<str>, pub detail: Option<Arc<str>>, pub lang: Option<Lang>,
                         pub key: ItemKey /* NodeKey | DocNodeId | u64 */, pub level: u8 }
enum ChooserState { Loading { first_spoken: bool }, Ready, Filtering { gen: u32 }, Closing { effect: ReaderEffect } }
```

- **CH-1 Keys.** Printable `unichar` → filter (case-folded, `ё`→`е`, substring with prefix matches first);
  Backspace; Up/Down/Home/End/PageUp/PageDown; Tab/Shift+Tab or Left/Right switch category; Enter =
  default action, Shift+Enter = second action (NVDA: Move to vs Activate, Activate only for links and
  buttons, `browseMode.py:1570`); gorca+Tab = "item 3 of 40, headings, filter ab"; Escape restores
  `resume` with its cursor. `speech.stop` and the gorca double tap keep working (LM-4/LM-5).
- **CH-2 Safety** (as LM-3): the input lane releases the grab if the Reader has not answered for 1 s;
  idle timeout 5 min (announced); the watchdog covers a stalled lane.
- **CH-3 Large sources.** The first category's first page is produced from the existing indexes (doc
  mirror `Cat::*`) on the Reader if ≤ 2k items, else on the cpu pool with the epoch; the first item is
  spoken as soon as it exists (W4). Filtering > 2k items runs on the cpu pool; a stale generation is
  dropped.
- **CH-4 Prompt** (find, "add pronunciation", profile name): single-line buffer with history (Up/Down),
  characters echoed per `echo.typed_characters`, Enter submits, Escape cancels. ADR-0002 `Mode::Find`
  becomes Prompt + Chooser, not a separate mode.
- **CH-5 Text-entry limits.** `KeyEvent.unichar` is layout-resolved by Mutter, so Russian works; dead
  keys and IME composition do not (ADR-0002 O-6 stays open); documented.

Sources in Phase 3: elements list, find history, command list (§4), speech history (§8), notifications
(ext-gnome-shell), voices, profiles, dictionaries' rules, extension menus (M18 hook `menu-items`).

### 5.3 Find in page

gorca+Ctrl+F opens `Prompt`; the query (NFC, case-folded) is searched in the document flat text from the
virtual cursor, wrapping once with "wrapped"; gorca+F3 / gorca+Shift+F3 next/previous (NVDA
`cursorManager.py:324, 357, 375`). Results are a range in the flat text; the virtual cursor moves to it
and the line is spoken. Outside browse mode, find runs over the current review line model (flat review),
the same API (§9.3). Gate W5 stands.

### 5.4 Copy and clipboard

| Option | Summary | Trade-off |
|---|---|---|
| **K1 ShellHelper clipboard (recommended)** | `org.gorca.ShellHelper.SetClipboardText(s)` / `GetClipboardText() -> s` via `St.Clipboard` (`refs/gnome-shell/js/ui/shellEntry.js:73,88`), allow-listed caller by pidfd | works on Wayland today, no window; needs our Shell extension enabled |
| K2 Wayland data-control protocol | background client owns the selection | not offered by Mutter 48 (to confirm in P-14); would be the clean path if it lands |
| K3 Native selection only | set the browser's selection over AT-SPI and let the browser's own Ctrl+C copy | no clipboard access needed, keeps formatting; cross-node selection support varies (Gecko vs Chromium), parity gap when off |

Decision: virtual selection (D-3) copies through K1; K3 is the `gorca+Shift+F10` native-selection toggle;
without ShellHelper copy says "clipboard not available". Clipboard read (`info.clipboard`, gorca+C):
≤ 1024 characters spoken, longer gives "clipboard: 12 lines, 3400 characters", ×2 reads it all (NVDA
behaviour). Clipboard text and copied text are never logged (PR-8); `sensitive` utterances are never
copied (`speech.copy_last` refuses, ADR-0007). Gate W6 stands.

---

## 6. Area D: pronunciation and symbol dictionaries (ADR-0017, M13)

### 6.1 Prior art

- NVDA dictionaries temp → voice → default → builtin → add-on (`refs/nvda/source/speechDictHandler/definitions.py:40-90`);
  format `pattern TAB replacement TAB caseSensitive TAB type`, types 0 anywhere, 1 regex, 2 whole word,
  3 part of word, 4 start, 5 end, 6 wildcard (`speechDictHandler/types.py:32-47, 180-232`); dictionaries
  before symbols (`speech/speech.py:192-215`). Builtin dictionary splits camelCase and letter/digit runs.
- NVDA symbols: levels none/some/most/all/char (`characterProcessing.py:153`), preserve
  never/always/norep (l.183), `complexSymbols:` and `symbols:` sections; merge user → locale → English
  base with country fallback (l.430-462, 46-60); CLDR names from `cldr.dic` (Unicode data).
- Orca: one word-level pronunciation map per layer (app layer included), applied last
  (`refs/orca/src/orca/pronunciation_dictionary_manager.py:74-109`, `speech_presenter.py:1549-1556`).

### 6.2 Pipeline (per language segment, in `gorca-speech`, after M12 segmentation)

```
normalise (NFC, drop U+FFFC/U+FEFF)
→ D1 temporary (session)          → D2 profile dictionaries (active L5/L6 profiles)
→ D3 voice (per voice id)         → D4 user default (per language or "*")
→ D5 extension (focused app's matching extensions only)
→ D6 shipped default (per language: abbreviations, camelCase/digit split like NVDA builtin)
→ symbols + punctuation level (per language, user overrides first)  → numbers/dates/emoji/URLs
```

- **DR-1** Dictionaries apply to text, words and lines; **not** to characters, spelling or typed-character
  echo (NVDA does the same on the spelling path).
- **DR-2** Each dictionary layer is compiled once (worker, on change) into a leftmost-longest Aho-Corasick
  automaton for literal/word/prefix/suffix rules plus a bounded list of `regex`-crate rules (linear time,
  no backtracking, compiled size ≤ 64 KiB, ≤ 256 regex rules per dictionary, replacement ≤ 256 chars).
  Rules within one layer do not chain (single pass); layers chain. Documented difference from NVDA.
- **DR-3** Case-insensitive literals use Unicode simple case folding with an offset map; word boundaries
  come from `gorca-text`'s word rule (the same as navigation).
- **DR-4 Silence guard.** A new regex rule that deletes ≥ 90 % of a built-in probe corpus is rejected
  with a diagnostic; `speech.dictionaries_toggle` (session) bypasses D1–D5; safe mode bypasses them.
- **DR-5 Symbols generalised.** The branch's `Lang {En, Ru}` becomes a BCP-47 tag with the fallback chain
  `ru-RU → ru → en`; tables are runtime data layered CLDR-generated base → gorca table → extension →
  user (`~/.config/gorca/symbols-<lang>.dic`, **the same tab format as NVDA**, so NVDA user symbol files
  import unchanged). Punctuation level is a profileable key (terminal "most").

### 6.3 Formats

```toml
# ~/.config/gorca/dictionaries/default.toml (also voice-<id>.toml, profile dictionaries inside profile files)
schema = 1
[[rule]]
match = "word"        # anywhere | word | prefix | suffix | regex
pattern = "gorca"
replace = "горка"
case = false
lang = "ru"           # optional; "*" default
```

Commands: `dict.add_current_word` (Prompt prefilled with the word at the review/caret position),
`dict.list` (Chooser over rules with Delete), temporary dictionary via a "temporary" toggle in the prompt.

### 6.4 Import and licences

- `gorca dict import --format nvda|orca FILE [--into default|voice:<id>|temporary]`: NVDA `.dic` rows
  map 1:1 (types 0/2/3/4/5; type 1 Python regex translated to `regex` syntax, rules with lookaround or
  backreferences rejected per rule with a diagnostic; type 6 wildcard → regex); Orca's `pronunciations`
  map (dconf `a{ss}`, read by `gsettings get` output or an exported keyfile) → `word` rules.
  Import of the user's own files is a user action on the user's data: no licence question.
- **Shipping** third-party data: NVDA is **GPL-2.0-or-later** (`refs/nvda/copying.txt:3`); Orca is
  **LGPL-2.1-or-later** (file headers, `refs/orca/COPYING`). Both may be incorporated into a
  **GPL-3.0-or-later** gorca with attribution; neither may go into the MIT/Apache WIT/SDK files
  (Phase 3 Q2). CLDR data is under the Unicode licence (GPL-compatible) and is the base we generate
  from. Until Q2 is decided, gorca ships only CLDR-derived and self-written tables (as the punctuation
  branch already does: "названия собственные").

### 6.5 Budgets

D1 gate stands (1k literal + 100 regex rules ≤ 50 µs per utterance p99; K2 Δp99 ≤ 0.2 ms). D2 (new):
import of a 10k-rule NVDA file ≤ 1 s on a worker, hot path unaffected. Dictionary edit applies ≤ 100 ms.

---

## 7. Area E: audio cues, capitals, lock keys, indentation (ADR-0017; F0, M13)

### 7.1 Prior art

- NVDA capitals: `capPitchChange=30`, `sayCapForCapitals`, `beepForCapitals` (2000 Hz, 50 ms) on the
  spelling path only, which covers typed characters and character navigation
  (`refs/nvda/source/speech/speech.py:389-433, 597, 1449-1491, 1965-1981`; `config/configSpec.py:55-59`).
  Pitch applies only when the synth supports `PitchCommand` (l.617).
- NVDA tones: soft-clipped sine (`refs/nvda/nvdaHelper/local/beeps.cpp:24-40`), played by NVDA itself;
  `[audio] soundVolume`, `soundVolumeFollowsVoice` (`configSpec.py:62-66`). Indentation tones: 220 Hz ×
  2^(quarter-tones/24), tab = 4, space = 1, 40 ms, speech above 72 spaces (`speech/speech.py:1056-1110`).
- Orca: GStreamer tones and icon files in-process (`refs/orca/src/orca/sound.py:56-195`), progress tones
  `98 + percent·5.4` Hz (`sound_generator.py:221-244`); capital icon by speech-dispatcher
  `CAP_LET_RECOGN icon` (`speechdispatcherfactory.py:167-190`), and only via `speak()` because `char()`
  does not reliably play it (l.414-425).
- speech-dispatcher `SOUND_ICON` is module-dependent: espeak plays `<audio src=…/sound-icons/name>`
  (`refs/speech-dispatcher/src/modules/espeak.c:347-352`), generic runs `play` (`generic.c:421-427`);
  the espeak capital icon is espeak's own `espeakCAPITALS` (l.588-604). voicego speaks the icon name
  (`refs/voicego/internal/server/session.go:820-845`); pitch is per request, not per segment.
- Lock keys: NVDA speaks "<key> on/off" 30 ms after the toggle (`keyboardHandler.py:494, 740-752`);
  Orca `typing_echo_presenter.py:611-627`.

### 7.2 Capitals: pitch per segment (F0) and earcon (M13)

- **CP-1** `Verbalized` gains prosody spans: `prosody: SmallVec<[(Range<u32>, Prosody); 2]>`, `Prosody {
  pitch_delta: i8 }` (−50…+50, relative to the voice's pitch). The verbaliser marks a capital on the
  spelling path (typed characters, character moves, spelling; DR-1 scope, as NVDA). Detection by Unicode
  `Lu`/`Lt` (Cyrillic included), never by Shift.
- **CP-2 Lowering.** voicego: per-request pitch in V1 (a request per span; Track V asks for a span-aware
  request). SSIP: SSML `<prosody pitch="+30%">` when `caps.ssml` and the module accepts it (espeak-ng
  yes; RHVoice to verify); otherwise, for a one-character utterance only, the pipelined triple
  `SET self PITCH p+Δ` / `SPEAK` / `SET self PITCH p` (no ack waits; ordering is per connection);
  otherwise the word "cap" (catalog `cap`). `speech.caps` becomes a set: `caps = ["pitch"]` default,
  `["pitch","earcon"]`, `["say"]`, `[]` (NVDA's three booleans). Migration from the scalar.
- **CP-3** Typing with Caps Lock on: no extra announcement (NVDA `beepForLowercaseWithCapslock` is an option
  later).

### 7.3 Earcons and the cue path

| Option | Summary | Trade-off |
|---|---|---|
| E1 Speech-server sound icons | SSIP `SOUND_ICON`, voicego equivalent | one stream, ordered with speech, no new process; serialised with speech (a capital earcon delays the letter by its length), module-dependent (espeak only on speech-dispatcher), voicego has none today |
| E2 voicego cue channel (Track V) | voicego mixes a cue track over speech | overlap, one audio client, our server; cross-repo work, not available on speech-dispatcher |
| **E3 `gorca-cues` sidecar (recommended as the guaranteed path)** | small Rust process, PipeWire stream, plays **pre-rendered** PCM from a theme on a unix-socket command | overlaps speech, ~5–10 ms to first audio at a 256-frame quantum, crash-isolated (not in-process, no synthesis at run time); one more process and a PipeWire client (CLAUDE.md put PipeWire *tuning* out of scope, not a client) |

Decision: a `CueSink` beside `SpeechClient`, with E3 as the default backend and E1/E2 as alternatives
chosen by capability; **conditional bake-off B-8** if E2 exists by M13: E2 vs E3 on key → cue first-audio
p99 idle and under L3-50k, overlap correctness with speech, behaviour under server/sidecar kill; E3 wins
ties (independent of the speech server).

```rust
// gorca-output-api
pub trait CueSink: Send + 'static {
    fn caps(&self) -> CueCaps;                         // overlaps_speech, pitch, pan, gain
    fn play(&self, cue: Cue) -> Result<(), SubmitError>;   // never blocks; Full | Disconnected
    fn stop_all(&self);
    fn health(&self) -> ClientHealth;
}
pub struct Cue { pub id: CueId, pub gain_db: i8, pub pitch_semitones: i8, pub pan: i8, pub epoch: Epoch }
pub enum CueId { ModeBrowse, ModeFocus, Capital, Boundary, Error, ListOpen, ListClose, LockOn, LockOff,
                 Notification, ProgressTick(u8), Indent(u8), ProfileSwitch, Restarted, Ext(u16) }
```

- **EC-1 Sound design.** A theme is a directory (`theme.toml` + 48 kHz mono WAV), rendered offline by
  `cargo xtask cues` from a small synthesis spec: sine plus two soft harmonics, 3–5 ms attack, exponential
  decay, 30–120 ms, notes on a just-intonation scale, peak −18 dBFS; pairs of rising/falling intervals for
  on/off cues (browse/focus, lock on/off), a short high soft tick for capitals (≈ 1.3 kHz, 35 ms, no
  click). No harsh square waves (NVDA's clipped sine is the counter-example the user named).
- **EC-2 Volume.** `[sounds] volume_db = -9` relative to speech volume (follows speech volume, as NVDA's
  `soundVolumeFollowsVoice`), `enabled = true`, `theme = "default"`, and `[sounds.events] mode = "cue" |
  "speech" | "both" | "off"` per event class. Without a cue sink every cue falls back to its catalog word
  (ADR-0002 SE-5 rule).
- **EC-3 Timing.** A cue is submitted in the same Reader step as its utterance; it is not cancelled by
  `speech.stop` (≤ 150 ms), but a cue not yet started whose epoch is two behind is dropped. Capital earcon
  only with `caps.overlaps_speech`; on E1 it is disabled (it would delay the letter).
- **EC-4 Indentation tones** use pre-rendered `Indent(0..=47)` (quarter-tone steps from 220 Hz, NVDA's map),
  so E1 servers can play them as named icons too; `report.indentation = tones|speech|both`, reported on
  change only, blank lines ignored (NVDA `ignoreBlankLinesForRLI`). Tab width from `text.tab_width = 8`
  unless the app exposes it.
- **EC-5 Sidecar lifecycle.** Started by the daemon with a pidfd (or as `gorca-cues.service`), restarted
  with backoff; a dead sidecar makes cues fall back to words; it never blocks the Reader (bounded
  SPSC, overflow drops the oldest cue).

### 7.4 Lock keys (F0, may land before M13)

- **LK-1** The input lane tracks `Modifiers::LOCK` and `MOD2` from every `KeyEvent.state`. On a Caps_Lock /
  Num_Lock press **that reaches the application** (for Caps Lock as gorca modifier: only the double-tap
  pass, ADR-0005 LM-5; a single tap does not toggle because Mutter skips the xkb update on the first
  click, memory mutter-key-grab-facts), the new state is the inverse of the observed bit; the lane emits
  `KeyIntent::LockToggled { key, on }` and the Reader speaks "Caps Lock on/off" (catalog) with a
  `LockOn/LockOff` cue when enabled.
- **LK-2** A confirming signal from ShellHelper (`Clutter.Keymap` `state-changed`, caps/num lock state)
  corrects a wrong prediction within 200 ms ("Caps Lock off"); without ShellHelper the next key event's
  state corrects it silently.
- **LK-3** Scroll Lock has no xkb indicator on GNOME by default: announced only when ShellHelper reports a
  state; otherwise not announced.
- Gate LK1: lock key → first SPEAK on the K2 budget (zero IPC).

---

## 8. Area F: other NVDA/Orca features, ranked

Value = daily blind user (5 high); cost S/M/L. "Have" = exists on main.

| # | Feature | Value | Cost | Status / design note | Milestone |
|---|---|---|---|---|---|
| 1 | Lock key announcements | 5 | S | §7.4 | **F0** |
| 2 | Capital pitch (fix `caps = pitch`) | 5 | S–M | §7.2 CP-1/CP-2 | **F0** |
| 3 | Speech history: review previous/next utterances, copy one, list | 5 | S | ring of 500 entries in the speech lane (text + lang + time, `sensitive` and key/typing echo excluded); `speech.history_prev/next` (gorca+Shift+X / gorca+Ctrl+Shift+X, provisional), list in Chooser with Enter = copy; NVDA has only last-utterance repeat/copy (`globalCommands.py:~5566, 5589`) | **F0** (review), M15a (list), M15 (copy) |
| 4 | Notification history (GNOME banners) | 5 | S | ext-gnome-shell records notifications; Chooser source; Orca has a list (`notification_presenter.py`) | M15a |
| 5 | Sleep mode per app, persistent | 4 | S | `input.sleep_toggle` (planned M6) as session toggle; persistence = profile key `general.sleep = true` with an App trigger (Orca persists a list, `sleep_mode_manager.py:121`) | F0 (toggle), M14a (persist) |
| 6 | Input help localised + command list | 5 | S | §4 | M12a/M15a |
| 7 | Per-app profiles | 5 | M | §3 | M14a |
| 8 | Elements list, find, copy, clipboard read | 5 | M | §5 | M15 |
| 9 | Indentation by speech/tones | 4 (developer: 5) | M | §7.3 EC-4; terminal, editors, web code | M13 |
| 10 | Spell phonetically, character description, code point | 4 | S | exists as ×2/×3 presses; Russian phonetic alphabet in the catalog | M12 |
| 11 | Formatting info (gorca+F), formatting changes while reading | 4 | S/M | have gorca+F; auto-report via `report.formatting_changes` | M13 |
| 12 | Date/time, battery | 3 | S | have date/time; battery from UPower `org.freedesktop.UPower` DisplayDevice (deadline 250 ms) | F0 |
| 13 | Review follow toggles | 4 | S | gorca+6/7 exist; mouse-follow added with §9 | M21 |
| 14 | Review/flat review/object nav everywhere + mouse routing | 5 | L | §9 | **M21** |
| 15 | Progress bar updates by tone/speech | 3 | S after cues | `ProgressTick(level)`, Orca map `98 + percent·5.4` Hz, rate limited 1/s | M13 |
| 16 | Speech viewer for sighted helpers | 3 | S/M | Control API stream (§11); GTK viewer window later in `gorca-settings` | M14 (stream), Phase 4 (window) |
| 17 | Mouse tracking (speak what is under the pointer) | 2 | M/L | needs pointer position (ShellHelper) + hit testing per surface; useful mainly for sighted helpers and low vision | Phase 4 (seams in M21) |
| 18 | Add-on store / SDK | 3 | L | SDK preview M18; store, signing Phase 4 | M18 / Phase 4 |
| 19 | OCR | 3 | L | ADR-0002 phases 3–4 | Phase 4 |
| 20 | Focus/caret highlight for helpers | 1 | M | Shell extension overlay | Phase 4 |

---

## 9. Area H: review, object navigation and the mouse everywhere (M21, ADR-0006 amendment 2)

User requirement (2026-10-17): flat review, object navigation and mouse routing must work flawlessly in
web content, any GTK/Qt/Electron window, the Shell and the terminal, at NVDA parity, with NVDA's numpad
layout and the laptop layout.

### 9.1 What exists (ADR-0006, M4a/M4b, M6, M7)

One `ReviewCursor { nav, pos: Object | Screen }`; object navigation over the mirror with ≤ 3 bounded
fetches per step; flat review as a per-surface `LineModel` from `GetExtents(WINDOW)` and visible text
lines, validity by mirror revision + zone hash + RR-1; line sources Geometry (desktop), Terminal (M6),
Document (M7, browse mirror). Coordinates never leave a surface (GTK4 `SCREEN` returns origin 0,0,
`gtkatspiutils.c:437-442`; St returns stage = global coordinates). There is no global coordinate, no
pointer action, and no mouse command.

### 9.2 NVDA command set and gorca status (full numpad + laptop)

NVDA sources: `refs/nvda/source/globalCommands.py` (review, navigator, mouse scripts), `review.py` (review
modes), `mouseHandler.py`. "Have" = implemented on main; "M4b" = reserved in ADR-0006 §11.

| NVDA function | Desktop | Laptop | gorca command | Status |
|---|---|---|---|---|
| review prev/cur/next line | KP7/KP8/KP9 | gorca+Up / gorca+Shift+. / gorca+Down | `review.*_line` | have |
| review prev/cur/next word | KP4/KP5/KP6 | gorca+Ctrl+Left / . / Right | `review.*_word` | have |
| review prev/cur/next char | KP1/KP2/KP3 | gorca+Left / . / Right | `review.*_char` | have (×3 code point after M3 verbaliser) |
| top / bottom | Shift+KP7 / Shift+KP9 | gorca+Ctrl+Home/End | `review.top/bottom` | have |
| start / end of line | Shift+KP1 / Shift+KP3 | gorca+Home/End | `review.start/end_of_line` | have |
| say all from review | KP+ | gorca+Shift+A | `review.say_all` | M4b → M21 |
| previous / next page | gorca+PageUp/Down | gorca+Shift+PageUp/Down | `review.prev/next_page` | M4b → M21 |
| review mode next / prev (object / document / screen) | gorca+KP7 / gorca+KP1 | gorca+PageUp/PageDown | `review.mode_next/prev` | have (Object ↔ Screen); Document cycle → M21 |
| navigator parent / first child / prev / next / current | gorca+KP8/KP2/KP4/KP6/KP5 | gorca+Shift+arrows / gorca+Shift+O | `object.*` | have |
| flattened next / prev (in-flow) | gorca+KP3 / gorca+KP9 | gorca+Shift+] / [ | `object.next/prev_in_flow` | have |
| navigator to focus | gorca+KP− | gorca+Backspace | `review.to_focus` | have |
| focus to navigator | gorca+Shift+KP− | gorca+Shift+Backspace | `review.move_focus` | have (AT-SPI `GrabFocus`; GTK4 returns NotSupported → §9.4 fallback) |
| activate navigator | gorca+KP Enter | gorca+Enter | `review.activate` | have (AT-SPI action) |
| move mouse to navigator | gorca+KP/ | gorca+Shift+M | `mouse.to_navigator` | **new, M21** |
| navigator to mouse | gorca+KP* | gorca+Shift+N | `mouse.navigator_to_mouse` | **new, M21** (needs pointer position) |
| left / right click | KP/ / KP* | gorca+[ / gorca+] | `mouse.left_click/right_click` | **new, M21** |
| lock left / right button | Shift+KP/ / Shift+KP* | gorca+Ctrl+[ / ] | `mouse.left_lock/right_lock` | **new, M21** |
| route mouse to review cursor | (move mouse to navigator uses the review position when in screen review) | | `mouse.to_review` | **new, M21** |
| review cursor follows caret / focus | gorca+6 / gorca+7 | same | `review.caret_follows_toggle`, `focus_follows` | have / M4b |
| review follows mouse, mouse tracking | gorca+M | gorca+M | `mouse.tracking_toggle` | Phase 4 (§8 row 17) |
| report review position / location | gorca+Shift+KP Delete (×2 navigator location) | gorca+Shift+Delete | `review.where` | **new, M21** ("line 3, column 12" / "object at 40 % of the window"; NVDA GC:1584-1632) |
| report caret / focus location | gorca+KP Delete (×2 focus) | gorca+Delete | `info.caret_where` | **new, M21** |
| caret to review cursor | gorca+Shift+KP− **×2** (NVDA `review.updateCaret`, GC:1694-1730) | gorca+Shift+Backspace ×2 | `review.move_focus` ×2 | **new, M21** (`Text.SetCaretOffset`; browse: moves the virtual cursor) |
| copy start mark / move to mark / copy | gorca+F9 / gorca+Shift+F9 / gorca+F10 | same | `review.copy_mark/to_mark/copy` | M21 (needs §5.4 clipboard; NVDA GC:4137-4188) |
| above / below (Orca) | Orca+KP4/KP6 | Orca+Ctrl+J/L | `review.above/below` | M4b → M21 |
| select from start / end of selection | gorca+Alt+Home/End | same | `review.*_of_selection` | M4b → M21 |

Gesture strings verified in `refs/nvda/source/globalCommands.py` (review GC:1911-2646, objects GC:1512-1860,
4605-4654, mouse GC:331-412 and 1383-1430, modes GC:1453-1474). NVDA details gorca copies: `review.activate`
tries the review position's action, then `doAction()` up the ancestors (GC:1862-1903); "move mouse to
navigator" targets the review position's start point, else the navigator's centre (GC:1385-1420); clicks
act at the current pointer (GC:333-350) and honour swapped buttons (`mouseHandler.py:310-330`); simple
review, follow focus and follow caret default on, follow mouse off (`config/configSpec.py:295-299`).

### 9.3 Unified review over every source

- **RV-1 One cursor, four `LineSource`s.** Geometry (desktop: GTK3/GTK4/Qt/Electron/St), Terminal (VTE
  screen snapshot), Document (browse mirror), **Text** (new: a single editable or text object reviewed as
  whole lines through `TextQuery::AtOffset`, M4b TX-2). `review.mode_next` cycles Object → Document (when
  a document mirror exists) → Screen, as NVDA does.
- **RV-2 Same keys, same speech.** Every source implements `LineModel` lines/zones; word and character
  segmentation is `gorca-text` (the unified editing model of Phase 3 wave 1), so review in the terminal,
  a web page and a GTK dialog moves and speaks identically.
- **RV-3 Toolkit gaps as data.** Electron/Chromium windows: document source when a web document is
  present, else geometry. Qt (`refs/qtbase` a11y): `GetExtents(WINDOW)` support to verify (V-Q1);
  missing extents degrade to tree order (ADR-0006 §6.7). St (Shell): stage coordinates are global and
  are converted to surface-relative by subtracting the surface's own extents.
- **RV-3a Mode rules from NVDA** (`review.py:83-190`, `api.py:317-354`, `treeInterceptorHandler.py:148-175`):
  a caret move in browse mode switches review to Document; entering focus mode with follow-focus on forces
  Object; an unavailable mode falls back to the previous available one; `mode_next` skips unavailable modes.
- **RV-4 Follow modes.** `review.focus_follows`, `review.caret_follows`, and (Phase 4) `review.mouse_follows`
  are per-profile keys (§3).
- **RV-5 Gaps vs NVDA that stay**: no screen review across windows (NVDA's Windows display model has no
  Wayland equivalent; each surface is reviewed alone, and the review cursor can step to the next surface
  with `review.next_surface`, gorca-specific); mouse tracking (Phase 4).

### 9.4 Pointer actions on Wayland: options

A Wayland client cannot move the pointer or click. Global coordinates are not available from AT-SPI for
GTK4 (window-relative only).

| Option | How | Trade-off |
|---|---|---|
| **M-A AT-SPI first (always tried first)** | "click" = `Action.DoAction` (the node's default/"click" action); "right click" = `ShowMenu`/`menu` action or Shift+F10 via the keyboard; "focus to navigator" = `Component.GrabFocus`, else `Action` "activate" on a focusable ancestor | no pointer at all, exact target, no coordinates; not every node exposes an action; GTK4 `GrabFocus` NotSupported (memory gtk4-geometry-facts) |
| **M-B ShellHelper virtual pointer (recommended for real pointer moves)** | `org.gorca.ShellHelper.PointerMoveTo(window, x, y)`, `PointerButton(button, press)`, `PointerPosition()`: inside the Shell, `seat.create_virtual_device(POINTER_DEVICE)` (the on-screen keyboard uses the same API for its keyboard device, `refs/gnome-shell/js/ui/keyboard.js:1856`), window-relative → global via `Meta.Window` frame/buffer rect | no portal dialog, no sharing indicator, global coordinates known to the Shell; requires our Shell extension, mapping an AT-SPI frame to a `Meta.Window` (ADR-0002 O-2: pid + title + stacking order, verified per toolkit), GTK4 CSD shadow offsets to measure |
| M-C Mutter RemoteDesktop D-Bus directly | `org.gnome.Mutter.RemoteDesktop` session: `NotifyPointerMotionAbsolute(stream,x,y)` needs a ScreenCast stream; `NotifyPointerButton`; or `ConnectToEIS` (`refs/mutter/data/dbus-interfaces/org.gnome.Mutter.RemoteDesktop.xml:107,174,364`) | no extension needed; a remote-access indicator is shown while a session exists (`gnome-shell/js/ui/status/remoteAccess.js`), absolute motion needs a screencast stream; holding a session open is a privacy signal to the user |
| M-D Portal RemoteDesktop + libei | `xdg-desktop-portal` RemoteDesktop with a persisted restore token, libei pointer | the supported public path; consent dialog once, indicator while active, most moving parts |

Decision: **M-A for activation, M-B for real pointer moves and clicks, M-D as the no-extension fallback**
(once, with consent, behind a setting), M-C not used (indicator and screencast requirement without a
benefit over M-D). Every pointer call has a 250 ms deadline, runs off the Reader, and reports
"mouse not available" when no path exists. Hit testing for "navigator to mouse" uses
`Component.GetAccessibleAtPoint(WINDOW)` on the surface under the pointer (ShellHelper gives the window
and the window-relative point).

### 9.5 State and safety

```rust
enum PointerState { Unavailable, Idle, Moving { req: ReqId, deadline: Instant }, ButtonLocked { button: Btn } }
```

- **MP-1** Button lock is released on focus-window change, on `speech.stop` ×2, on gorca restart (the
  ShellHelper releases buttons of a vanished caller: it watches our bus name).
- **MP-2** A locked button is announced every time focus changes while it is held ("left button
  locked"), so a forgotten drag cannot silently move content.
- **MP-3** The ShellHelper allow-list (pidfd + exe) covers the new methods; tests run against a private
  Shell only (CLAUDE.md live-session rules: no pointer synthesis on the live session in tests).

### 9.6 Gates

| Gate | Stimulus → end | Target / ceiling |
|---|---|---|
| R1 (have) | review step on a valid model → SPEAK | 3 ms |
| **R2** | `review.mode_next` to Document/Text → first line spoken | 15 ms / 5 ms |
| **R3** | `mouse.left_click` on navigator → AT-SPI action sent or pointer button delivered (private Shell) | 50 ms wall / 20 ms |
| **R4** | review parity matrix: 40 NVDA review/object steps × {GTK3, GTK4, Qt, Electron, Shell, VTE, Firefox, Chromium} | 0 GAP at P0 rows; expected-fail table only for documented toolkit bugs |

---

## 10. Area I: graph and vector navigation, seams only (ADR-0002 amendment)

The user's later idea: navigation over the accessibility graph with embeddings and vector search
("go to the thing that looks like X", similarity jumps). ADR-0002 already owns semantic find and the
perception sidecar. Phase 3 builds **no** semantic feature; the review/object layer exposes the data such
a feature will need, read-only, behind the core API (no connector types):

- **GV-1 Stable identity.** `NodeKey` (process-local) plus a **stable path** `StablePath { app: AppStableId,
  surface: SurfaceSig, steps: SmallVec<[(Role, u16 /* index among same-role siblings */, u32 /* name hash */); 12]> }`
  that survives a restart of gorca and, best effort, of the app; ADR-0002 label memory already needs it.
- **GV-2 Feature record per node** (cheap, from the mirror, no IPC): role, states bitset, name/description
  hashes and lengths, text length, depth, child count, sibling index, surface-relative rect when known,
  toolkit, app class. `fn features(&self, node) -> Option<NodeFeatures>`; vectors are the sidecar's job.
- **GV-3 Graph snapshot API.** `MirrorRead::graph_snapshot(surface, limits) -> GraphSnapshot { nodes:
  Arc<[NodeFeatures]>, edges: Arc<[(u32, u32, EdgeKind)]> }`, `EdgeKind = Child | FlowNext | LabelledBy |
  DescribedBy | Controls | FlowsTo | GeomBelow | GeomRightOf` (geometric edges from the flat review
  model). Built on the cpu pool, bounded (50k nodes), immutable `Arc`; also exportable by
  `gorca debug graph-export --format jsonl` for research in the Python venv (networkx/faiss).
- **GV-4 Jump target interface.** `ReviewMsg::JumpTo { node: NodeKey, why: JumpReason }` lets a future
  feature actor move the review cursor or browse cursor without new core code; `Mode::Chooser` shows
  ranked candidates (the find UI of ADR-0002 §9.1 is Prompt + Chooser, §5.2).
- **GV-5 Privacy.** Feature records exclude text of `sensitive` nodes and terminal content unless X-7 is
  on (ADR-0002); hashes are keyed per session.

---

## 11. Area G: settings UI boundary — `org.gorca.Control1` (ADR-0011, M14)

The daemon is the only writer of every file; the GUI (`gorca-settings`, GTK4) and the CLI are clients.

```
org.gorca.Control1  (/org/gorca/Control1, session bus, same UID; mutations peer-checked like ShellHelper)
  Properties  Version u, UiLanguage s, ActiveProfiles as, EditScope s, SafeMode b
  Settings    GetSchema(lang s) -> a(sssvva{sv})        # key, type, label, default, range, flags (layering, profileable, restart)
              GetEffective(keys as) -> a{s(vs)}          # value + source layer
              GetLayer(layer s) -> a{sv};  Set(layer s, key s, v v) -> a(ss) diags;  Reset(layer s, key s)
              Save(layers as) -> a(ss);  Revert(layers as)
  Profiles    List() -> a(ssa(sa{sv})b);  Create(name s, triggers a(sa{sv})) -> s;  Rename;  Delete;
              SetManual(id s);  SetVendorEnabled(id s, b);  signal ProfilesChanged(active as)
  Dictionaries List() -> a(ss);  Rules(dict s, offset u, limit u) -> aa{sv};  PutRule(dict s, rule a{sv}) -> u;
              DeleteRule(dict s, id u);  Import(dict s, format s, fd h) -> a(ss);  Export(dict s, format s, fd h);
              Preview(text s, lang s) -> s               # "how would this be spoken"
  Symbols     Rules(lang s) ...; PutRule; DeleteRule   (same shape)
  Speech      Voices(lang s) -> a(sss);  PreviewVoice(voice s, text s);  Cues() -> a(ss);  PlayCue(id s)
  Commands    List(lang s, ctx s) -> a(sssas);  Bind(ctx s, chord s, cmd s) -> a(ss);  Unbind(ctx s, chord s)
  Extensions  List() -> a(ssbu);  SetEnabled(name s, b)
  Viewer      OpenSpeechViewer() -> h                    # fd of a JSON-lines stream; opt-in, off after restart,
                                                         # never carries sensitive utterances
  Diagnostics Save() -> s
```

- **CA-1** Every method runs on the control task (workers) with a 250 ms deadline; changes reach the
  Reader as `ReaderMsg::Settings(ControlSet{…})` and the reply is a message (R3); Reader cost ≤ 50 µs per
  operation. Per-client token bucket 100 req/s; fd payloads capped at 4 MiB and parsed on
  `spawn_blocking` with a 1 s deadline. A frozen client cannot affect reading (G1).
- **CA-2** The GUI is generated from `GetSchema` (labels are catalog ids rendered in `lang`), so new keys
  appear without GUI code.
- **CA-3** The speech viewer uses a passed fd, not a bus signal, so other bus clients cannot listen.

---

## 12. Roadmap

New milestones and sub-milestones (amending `docs/plan/phase-3.md`):

| Item | Content | Wave | Depends on |
|---|---|---|---|
| **F0 quick wins** | lock keys (§7.4), capital pitch via prosody spans (CP-1/2), speech history review (no list, no copy), battery, sleep toggle (session) | 1 (with C0 tail / M11) | none; CP-2 needs a 1-day SSML/pitch spike per backend |
| M12a | catalog ids incl. `CmdSpec.help`, key names, `@word.*` out of symbol data | 1 | C0 1–3 (CF-6) |
| **M15a modes** | `Mode::Chooser`, `Mode::Prompt`, command list, speech-history list, notification list | 1→2 | M12a |
| **M14a profiles** | ADR-0016: layer stack, triggers, origin-layer edits, `[report]` flags, vendor profiles for first-party extensions, sleep persistence; `AppInfo.desktop_id/exe`, `DocMeta.url` | 2 | M11 (migration), M12a |
| M13 | ADR-0017: dictionaries + import, symbol layering (generalised punct branch), `CueSink` + `gorca-cues` + theme, capital earcon, indentation tones, progress tones; B-8 only if voicego E2 exists | 2 | M12 (segments), punct branch merged |
| **M21 review everywhere** | ADR-0006 amendment 2: M4b leftovers, Text line source, mode cycle with Document, ShellHelper pointer (P-16 spike: virtual pointer + window mapping), mouse commands, `review.where`, copy marks, R2–R4 parity matrix incl. Qt/Electron; GV-1…GV-5 seams | 2 | M15a (chooser for results), P-14 (clipboard) |
| M15 | elements list, find, virtual selection copy, clipboard read (P-14) | 2 | M15a, C0 (M8/M9) |
| M14 | Control API with profiles/dictionaries/cues/commands/viewer; GUI | 2→3 | M14a, M13 |
| M18 | WIT 0.9: vendor profiles, dictionaries, symbol files, catalogs, `menu-items` for third parties | 3 | M14a, M13 |
| Phase 4 | mouse tracking, say-all profile trigger, extension context tags, per-role verbosity tables, GUI lists for helpers, speech viewer window, semantic/graph navigation (ADR-0002 MP) | — | — |

**First:** F0 and M15a (they are small, user-visible daily, and M15a is the UI that §4, §5, §8 and §10
reuse), then M14a and M13 in parallel (disjoint crates: `gorca-config`/Reader vs `gorca-speech`/output),
then M21 and M15.

New spikes: **P-16** ShellHelper virtual pointer and `Meta.Window` ↔ AT-SPI frame mapping per toolkit
(linux-platform-engineer, private Shell only); **P-17** SSML/pitch support per SSIP module (espeak-ng,
RHVoice) and voicego per-request pitch; **V-Q1** Qt `GetExtents(WINDOW)` and text extents.

New documents: ADR-0016 configuration profiles and verbosity; ADR-0017 speech text processing and audio
cues; ADR-0005 amendment (`Mode::Chooser`, `Mode::Prompt`); ADR-0006 amendment 2 (review everywhere,
pointer); ADR-0002 amendment (GV seams, `Mode::Find` = Prompt + Chooser). Numbers are the next free ones on
2026-10-17 after the planned 0009–0015.

## 13. Gates added by this document (all p99 gorca-side unless stated; Phase 3 §2.1 rules apply)

| ID | Stimulus → end | Target / ceiling | Milestone |
|---|---|---|---|
| P1, P2 | §3.10 | | M14a |
| LK1 | lock key → SPEAK | K2 budget | F0 |
| CP1 | typed capital with pitch → SPEAK | K2 Δp99 ≤ 0.2 ms | F0 |
| SH1 | history append | ≤ 1 µs; review command on the K2 path | F0 |
| CH1 | chooser filter keystroke at 10k items → item spoken | 5 ms / 2 ms | M15a |
| D2 | §6.5 | | M13 |
| AC1 | cue dispatch in the Reader | ≤ 20 µs | M13 |
| AC2 | key → cue first audio (sidecar, private PipeWire) | 40 ms wall / 20 ms | M13 |
| R2–R4 | §9.6 | | M21 |

## 14. Questions for the user

| # | Question | Options | Recommended default |
|---|---|---|---|
| QF-1 | When a per-app profile is active and you change a setting with a key (rate, punctuation), where does the change go? | (a) to the layer the value came from, global unless the profile already sets it (§3.6); (b) always into the active profile (NVDA); (c) always global | **(a)**, plus a key to say "this change is for this app only" |
| QF-2 | Profiles shipped by extensions (e.g. "terminal: punctuation most, indentation tones") | (a) on by default above your global settings, presentation keys only, one switch to turn each off; (b) off until you enable them | **(a)** |
| QF-3 | Capital letters while typing and moving by character | (a) higher pitch (NVDA default); (b) pitch + a soft short sound; (c) sound only; (d) the word "заглавная" | **(b)** once the sound path exists (M13); **(a)** until then (F0) |
| QF-4 | Sound path for earcons | (a) a small gorca sound process on PipeWire (overlaps speech, ~10 ms); (b) only through the speech server (no overlap, delays letters); (c) measure voicego mixing vs (a) first | **(a)** default, **(c)** if voicego gets a cue channel |
| QF-5 | Elements list, find, command list, speech history: spoken list inside gorca, or a GTK window? | (a) spoken list (instant, braille, no focus change); (b) window like NVDA; (c) both | **(a)** in Phase 3; window for sighted helpers in Phase 4 |
| QF-6 | Mouse on Wayland: may gorca move and click the pointer through its GNOME Shell helper (no permission dialog, no indicator), with the screen-sharing portal as a fallback? And is the GPL-3.0-or-later licence (Phase 3 Q2) confirmed, which lets gorca ship NVDA/Orca-derived dictionary and symbol data with attribution? | (a) yes to both; (b) helper only, no portal; (c) AT-SPI actions only, no pointer; licence: decide separately | **(a)**; until the licence is confirmed, only CLDR-derived and self-written tables ship |
