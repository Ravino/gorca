# ADR-0016 configuration profiles: adversarial design review

## r1 (2026-10-19, main 90b3ed4f) — ACCEPT-WITH-CHANGES

Inputs: `docs/adr/0016-profiles.md` (Proposed), `docs/plan/phase-3.md` M14a block,
`docs/plan/phase-3-features.md` §3, ADR-0024 §2.13, decisions 2026-10-19 (DP-3), `crates/gorca-config`,
presets, CHR-0 `app_modes.rs` in worktree `agent-a508eca24e2626204` (d87409fb), `refs/nvda`, `refs/orca`.
Design review only; nothing built.

### High

**H-1 Two resolution paths disagree: deltas, manual profile and suspend never reach App/Doc keys.**
§2.4 SW-4: doc registry / terminal arm call `ProfileSet::resolve_app(&AppFacts)` /
`resolve_doc(&AppFacts,&DocFacts)` on the compiled set from the config task. The signatures carry no manual
profile (TR-3 puts `manual` only into `ProfileKey`), no layer deltas (INV-P4: the Reader owns them) and no
`ProfileState` (`profiles.suspend` = "base only", `Disabled`). Failures: (a) PE-6 "save the current mode as
default here" writes `browse.mode_default` as a Reader delta; the next document of that site resolves via
`resolve_doc` on the file-backed set and ignores it until `settings.save` — the owner's messenger
increment ("makes a web messenger open in sticky focus mode") does not work in-session; (b) a manual (L6)
profile setting `browse.enabled`/`terminal.*` has no effect; (c) §2.10 names `profiles.suspend` as the
escape from a misrouting vendor row, but a vendor `browse.enabled=false` or `terminal.*` row survives suspend.
Fix: one resolution input `(ProfileSet gen, delta gen, manual, state)`; publish deltas + manual + state as an
immutable `ResolveCtx` snapshot next to the `ProfileSet` (Reader stays the only writer, consumers read
`ArcSwap`), and add a P6 property test "App/Doc key == Focus-cell value for the same facts".

**H-2 PF-4 confinement is vacuous; third-party vendor rows can disable browse mode in Firefox.**
`gorca-ext-host/src/manifest.rs:31` "An empty list matches anything"; `Matcher` also matches by `toolkit`
and `connector`. An add-on with an empty or `toolkit=["*"]` matcher intersects to "every app", and with
Q-P2 (a) its `browse.enabled=false` row is on by default. Fix: a vendor trigger must name apps
(non-empty `name`/`exe`/`desktop_id`, no bare `*`), PF-4 intersects with the manifest's `app_name` list
only (empty = no vendor profiles); browse-mode keys for an app the extension is not the primary claimant of
are diagnostics; rows from non-first-party extensions start disabled (or are announced once on install).

**H-3 Privacy keys are vendor-settable.** `terminal.*` is `vendor = yes` (§2.3) and includes
`terminal.secret_regex` (`gorca-config/src/settings.rs:940`). In ext-vte a non-empty value *replaces* the
default secret pattern (`extensions/ext-vte/src/prompt.rs:68-72`), so a vendor row `secret_regex = "a^"`
switches off the extension's password-prompt detection (the core's fail-closed keyword check in
`gorca-core/src/terminal.rs:217` remains, so this is defence-in-depth loss, not total). Also L4 sits above
L3: a vendor row overrides a user's explicit global privacy choice (`echo.typed_characters=false`,
`editing.announce_clipboard=false` spoken clipboard). Fix: `SettingDef.privacy: bool` (or `vendor=false`) for
`terminal.secret_regex`, `echo.*`, `editing.announce_clipboard`; and for every vendor key, a value the user set
explicitly in L3 beats L4 (vendor overrides defaults, not user choices) — or justify L4>L3 per key.

### Medium

**M-1 "Switching never touches the key path" is false for mode toggles.** `browse.toggle` (gorca+Space /
gorca+A) changes `WebMode`, which is a `Context` input (`BrowseMode`/`FocusMode`), so the key press computes
a new `ProfileKey`, may fold inline (≤200 µs miss) and may emit SW-3 `SetParams` before the mode word.
Auto focus mode during Tab/quicknav does the same. Acceptable cost, but: prewarm (SW-2) must include both
WebMode contexts of the focused document, and gate P1 must add "gorca+Space → mode word SPEAK with a
FocusMode profile that changes rate" (Δp99 ≤ 0.2 ms).

**M-2 INV-P3 has an extension hole.** `ext.<id>.*` is `Focus` scope (settable by `Context` triggers) and
M18a gives extensions a `request-mode` effect. Profile Context(FocusMode) sets `ext.x.k=1` → extension
requests browse → Context(BrowseMode) sets `k=0` → extension requests focus: an oscillation INV-P3 claims
impossible. Fix: `Context{BrowseMode|FocusMode}` triggers may not set `ext.*` keys, or `request-mode` is
rate-limited per document and never re-fires on a settings-only change; state it in NS-1.

**M-3 Profile creation from untrusted names (PE-2 "this application", PE-6).** The App profile is created
from `AppInfo.app_name` (supplied by the application over AT-SPI) and the Site profile from the `DocURL`
host. Unspecified: file-id sanitising (`/`, `..`, NUL, length) and glob escaping — an app named `*` or
`code*` produces a trigger that matches every app. Fix: slug ids, `O_EXCL` create inside the dir, write
names as escaped literals (CHR-0 exact semantics), test with hostile names.

**M-4 Glob semantics claim is wrong.** ADR says "Glob = ext-host matcher semantics: case-insensitive";
`gorca-ext-host/src/glob.rs:4` is case-sensitive and supports `?`; CHR-0 uses `eq_ignore_ascii_case`
(ASCII only). Pin: ASCII case-insensitive, `*` and `?` (or not), max pattern length; one shared function
(moving it touches `gorca-ext-host`, not an M22a file). Parity test must include non-ASCII names.

**M-5 CHR-0 migration regresses in safe mode / suspend / without ext-chromium.** CHR-0's `vscode` row is
compiled into `gorca-config` and always applies. After P6 it is an L4 row of `extensions/ext-chromium`,
which does not exist (`extensions/` = ext-gnome-shell, ext-test, ext-vte) and "M14a needs no ext-host
change" leaves undefined who loads `profiles.toml` and what `[matcher]` PF-4 intersects with. PC-2 safe mode
(L0–L3) and `profiles.suspend` bring VS Code's buffer back. Fix: either keep first-party rows as a
compiled-in L2-like "product defaults" layer (survives safe mode), or state the regression and add a
parity case for it; define the loader (embedded `include_str!` is simplest).

**M-6 Doc/App key change timing is undefined (testability).** `browse.mode_default` is "mode when a document
first gets focus in the session"; what happens to an already-open document when `browse.enabled`/
`mode_default` changes by reload, manual profile, or save-mode — teardown of the mirror, immediate mode
change, or next document only? Specify "latched at doc `Appeared`/first focus; `save_mode_default` also
applies to the current doc; other changes apply to new documents", and test each.

**M-7 PC-1 clamp targets the wrong layers.** Vendor rows cannot set rate/volume at all (PF-1), so the clamp
only bites user L5/L6 files: a user cannot have rate 100 in a terminal profile. Meanwhile the real silencing
risk in user profiles — `speech.voice`/`output_module`/`language` naming something unavailable — is not
clamped. Fix: volume floor only; unavailable voice/module falls back to the lower layer's value with one notice.

### Low

- L-1 `ProfileState` table not exhaustive: missing Suspended{User} × own-mode enter/leave,
  Suspended{OwnMode} × focus change (queued key?), Disabled × reload, manual-profile file deleted while active.
- L-2 PM-1 undefined for a profile with several matching triggers of different kinds (take its most specific
  matching trigger) and for the "longer literal prefix" of `AppMatcher` with three glob lists.
- L-3 PF-5 caps globs per trigger but not triggers per profile or glob length (O(n·m) matcher).
- L-4 "id = file stem (stable across renames)" is self-contradictory; shadow id `"<ext>/<name>"` → file
  name mapping unspecified; PF-2 disables keyed by row name lapse silently if the extension renames the row.
- L-5 Hot reload of a half-written file (non-atomic editors) drops the profile mid-edit (voice flips);
  keep the last good parse of a file that fails and say so.
- L-6 TR-1 Context from "focus snapshot role class" vs ADR-0024 PS-4 "published SemEntry role class": pick
  one for M14a and note the switch at M22a wave C.
- L-7 P2 "fuzz/ target": no cargo-fuzz infrastructure in the repo; use a proptest round-trip or add the
  infra as its own item. P3's LRU must not add a crate (Cargo.lock conflicts with M22a waves).
- L-8 `profiles.vendor` is a table; `SettingType` has no map type (P1 must add it).

### Slices and M22a independence

P1–P3 touch only `gorca-config` (+ `tools/research`); `docs/plan/m22a-spec.md` does not mention
`gorca-config`: independent of M22a. One trivial conflict with CHR-0 (`gorca-config/src/lib.rs`
`pub mod app_modes`). Sharing the glob (M-4) touches `gorca-ext-host`; keep it a copy in P2 if that
crate is busy.

### Q-P4 chord check (presets)

- NVDA presets: `gorca+space` = `browse.toggle`, `gorca+shift+space` = single-letter toggle; ×2/×3 free.
- Orca presets: `gorca+a` = `browse.toggle`; in real Orca `Insert+A` ×2 = sticky focus mode and ×3 =
  sticky browse mode (`refs/orca/src/orca/document_presenter_command_definitions.py:37-62`), so ×3 = "save
  default" breaks Orca muscle memory. NVDA-laptop `gorca+a` = say all (no conflict, toggle is gorca+Space).
- `browse.toggle` has `max_press = 1` (`gorca-core/src/commands/table.rs:1469`) and the lane falls back to
  `(chord,1)` with a repeat count (`gorca-input-api/src/lane.rs:621`), dispatching immediately. A triple
  press therefore runs toggle, toggle, save: two real mode switches (browse→focus moves system focus to the
  focusable under the cursor) before saving. (a) needs `max_press = 3` and a handler that ignores rep 2..3.

### Owner questions

- Q-P1: (a) agree.
- Q-P2: (a) for first-party rows only; third-party rows off by default until H-2 confinement exists.
- Q-P3: (a) agree; but keep the toggle across document replacement within the same host (SPA/Chromium doc
  swaps) or messenger users re-toggle on every navigation.
- Q-P4: (b) now. If a chord is wanted later: NVDA gorca+Space ×3 with `max_press = 3`; Orca preset keeps
  Orca's ×2 sticky focus / ×3 sticky browse.
- Q-P5: (b) agree, and drop L6 from P1–P4 (dead code until M15a) or keep it type-only.

## M14a-core P1–P3 code review r1 (2026-10-19, 97a0e3eb + c220f8d6)

**Verdict: MERGE AFTER FIXES** (F-1..F-3 before merge; the rest may follow in P4).

Gates run on the branch: `cargo test -p gorca-config` 128 passed; `cargo clippy -p gorca-config --all-targets
-- -D warnings` clean; `cargo fmt --check` clean. No `unwrap`/`expect`/`panic!` in non-test code; the crate stays
pure (no I/O, no tokio); snapshot publication is `ArcSwap` store/load. Probes were temporary tests appended to
`profiles/tests.rs` (removed afterwards); each finding below names the probe outcome.

### Findings (ranked)

- **F-1 Medium — save-to-global changes the app's effective value when a row sets the key**
  (`profiles/edit.rs` `plan_save_global`, "already global" branch, with `resolve.rs` UB-1 skip). Product row
  `firefox*` sets `speech.punctuation = "all"`; the user sets "some" (the default) in Firefox; save-to-global
  reports `already_global`, removes the key from the profile, and the global "some" is UB-1-skipped as a default.
  Probe: Firefox punctuation after save-to-global = `all` (expected `some`). The same happens on the copy branch
  whenever the copied value equals L0. Fix: before removing a key, resolve the profile's app with the key removed;
  if the value would change, keep the key in the profile and report "kept for Firefox: a row sets it" (or refuse
  that key with the explain text).
- **F-2 Medium — "already global" ignores the unsaved global delta** (`edit.rs` `global_value`). Global target
  rate 70 (unsaved), then Firefox rate 50 (= file value); save-to-global says "already global", removes 50 from
  the profile, the L5 delta 70 now applies. Probe: Firefox rate after = 70. Fix: compare against
  `deltas.base` first (or treat any key with a global delta as "copy" and drop that delta as the copy branch does).
- **F-3 Medium — `classify` fails open for new sections** (`settings.rs` `classify`, final `else`): any future
  section (`sources.*`, `general.sleep`, `ui.language`, every `Apply::Restart` key) becomes `Focus`, vendor,
  non-privacy, i.e. settable by add-on rows. Fix: explicit arms for `text`/`verbosity`, unknown section =
  `Base`/non-vendor (or privacy), plus a test that lists every section of `SETTINGS` against the §2.3 table.
- **F-4 Low — new profile id can collide with a file that is not in the set** (`edit.rs` `unused_id` checks only
  `self.set`). A never-parsed file with the slug's stem makes `plan_save` return `ChangedOnDisk` for every save
  until revert (probe C: `Err(ChangedOnDisk("firefox-ad98eada"))`). Fix: give `DiskView` (or the loader) the
  directory's stems and pick a free one; make `WriteStep` carry `Expect::Absent` instead of the empty-text hash so
  the config task's `O_EXCL` create is part of the contract.
- **F-5 Low — mixed-trigger app profile silently gets a twin.** With the (accepted) strictest-trigger rule, an
  App-key edit in an app whose profile also has a Context trigger creates `firefox-<hash>` and announces "new
  profile for firefox" although one exists (probe E). Fix: confirmation should say why ("Firefox profile cannot
  hold terminal settings because it also has a menu trigger").
- **F-6 Low — `LruMap<ProfileKey, _>` is not tied to `generation` or `mode`.** `ProfileKey` stays the same across
  delta edits, pins of other apps and suspend (same interner), so a P4 cache that is not cleared on every publish
  serves stale settings. Fix: key the LRU by `(generation, ProfileKey)` or clear it on each store; state it in P4.
- **F-7 Low — PF-2 diagnostic missing:** a `profiles.rows` entry that names no row is not reported.
- **F-8 Low — oracle shares code with the implementation** (`tests.rs` `oracle` calls `is_enabled`/`rows_map`)
  and covers no Window/Site triggers, ext keys, first-party extension rows or enabled deltas. Rust instead of
  Python is acceptable; make the oracle self-contained and widen the generator in P5.
- **F-9 Low — perf nits off the key path:** `is_enabled` clones `profiles.rows` per row per fold;
  `Interner::intern_pin` always takes the write lock; `read_triggers` has dead code (`items`).

### Rulings on the implementer's deviations

| Deviation | Ruling |
|---|---|
| ext-host glob untouched | accepted per GL-1 ("or keeps a copy"); track: ext-host is still case-sensitive, so a row can match `FIREFOX` where the extension's manifest matcher does not. Re-export before M18a. |
| oracle in Rust, not Python | accepted with F-8. |
| SS-1/PC-2/§2.10 say L3, table says L4 | table is right (L3 = third-party since r3); ADR erratum: product rows are L4 in SS-1, PC-2 and §2.10. |
| multi-trigger profiles held to strictest trigger | accepted (needed for INV-P6); add F-5 wording. |
| unsaved L5 edits never UB-1-skipped | accepted: a command edit is an explicit user choice (Q-P6). |
| `disabled` set → `enabled` delta + `profiles.rows` | accepted, equivalent. |
| row `name` globs must be covered; `exe`/`desktop_id` only narrow | accepted: name is mandatory and lists are ANDed, so stricter than PF-4. |
| `gen` → `generation` | accepted. |

Verified as specified: PK-1 (unknown keys/sections rejected with did-you-mean, wrong type/range, uninstalled
`ext.*` kept inactive and preserved on save), Base keys never in a profile, PF-1 (privacy, voice/rate/volume
rejected from rows), own-namespace `ext.*` for rows, NS-1/NS-3, PF-4 (Window without app, `*` exe, uncovered
`fir*` all rejected), third-party `enabled = true` in the row file ignored (stays off), first-party on, UB-1 incl.
privacy keys, PC-1 floor on rows/profiles/deltas, SG-2/SG-3/SG-5 ordering and partial-failure state, PE-6 hostile
names (slug + FNV suffix, escaped literal, round trip through the file).

## Implementer follow-up (P1-P3 fixes)

Fixed and tested: F-1 (explicit default-equal values are copied; a key whose move would change the focused
app's effective value stays in the profile and is listed in `SaveGlobalReport.kept_row_sets`; `plan_save_global`
now takes the focus facts), F-2 (an unsaved global edit is never "already global"), F-3 (`classify` is explicit,
unknown sections are Base, not row-settable, private; a test lists every section), F-4 (`BaseLayers.reserved_ids`,
`ProfileLoader::unparsed_ids`), F-5 (refusal `NotAllowedHere` instead of a twin), F-7
(`ProfileSet::unknown_row_entries`; P4 must call it with the effective `profiles.rows` and report it).

Notes for P4: F-6 (key the LRU by generation or clear it on every store; `ProfileKey` is stable across deltas),
F-8 (widen the oracle: Window/Site, ext keys, first-party ext rows, enabled deltas; make it self-contained), F-9
(cache the `profiles.rows` map per fold; read lock in `intern_pin`), `WriteStep` should carry an explicit "must not
exist" expectation for created files (now the hash of empty text).

ADR erratum list for the architect:
1. SS-1 says Suspended/Disabled fold "L3" (product rows), but sections 2.1 and 2.9 define product rows as L4 and L3
   as third-party rows; implemented per the table.
2. PC-2 repeats "L0, L1, L3, L5" for safe mode; L2 is off in safe mode and product rows (L4) stay.
3. Section 2.10 and 2.9 call the product row "L3".
4. `ProfileSnapshot.disabled: HashSet` is replaced by per-profile `enabled` deltas plus the `profiles.rows` delta.
5. UB-1 says "file + delta" is skipped when default-equal; implemented for file entries only.
6. Save-to-global needs the focus facts (F-1); PE-4 should say a key that a row would take over stays in the profile.
