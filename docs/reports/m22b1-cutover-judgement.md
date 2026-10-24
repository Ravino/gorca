# M22b1 cut-over gate judgement: focus and change

Date: 2026-10-24. Reviewer judgement on `bench/results/cutover/focus-2026-10-24-3.md` and
`change-2026-10-24-3.md` (rev 3baf937f, dirty), re-run at main 4eb252a8.

**Verdict: REJECT this gate run. Do not tag `legacy/focus-last`, do not delete the legacy
focus/change paths.** Re-gate after the fixes in section 4. Approval is likely after one round:
the gate machinery is mostly sound. The deviation rows and the corpus are not.

## 1. Reproduction

I built `gorca-connector-atspi --lib` (test profile) and ran `cutover_gate_tests::cutover_emit`
inside `tests/harness/run-isolated.sh` with `GORCA_CUTOVER_SHUFFLE=0`, for both surfaces. The
per-row counts match both reports exactly: focus has 414 allowed, 2454 equal and 50 silent;
change has 238 allowed. I mapped every record to its fixture node (`i` == `key.local`) to get
the AT-SPI role and tag. The scratch scripts are in the session scratchpad and are not committed.

Per mutation case, the change records split as follows:

| case | equal | allowed | silent |
|---|---|---|---|
| check | 66 | 0 | 0 |
| mixed | 51 | 10 | 0 |
| expand | 51 | 0 | 0 |
| select | 58 | 22 | 0 |
| press | 7 | 62 | 5 |
| disable | 748 | 141 | 0 |
| invalid | 149 | 0 | 0 |
| required | 149 | 0 | 0 |
| rename | 840 | 0 | 2 |

The real-change cases (`real-*`) total 37 records across all families.

## 2. Deviation rows

| row (surface) | hits | predicate | verdict |
|---|---|---|---|
| content-derived-name (focus, focus-web) | 12+101 | narrow: gated on `ProvCode::Derived`, takes one Text token | improvement (rows and list items gain a name). Accept. |
| static-text-word (focus-web) | 90 | narrow: StaticText or Caption, exactly one label/text word each side | neutral wording. Accept. |
| grouping-word (focus-web) | 16 | narrow | improvement. But see unavailable-static (focus): firefox/forms 67 `<fieldset disabled>` goes from "Disabled set, panel, unavailable" to "Disabled set, grouping", so **unavailable is lost**. |
| marker-stripped-name (focus, focus-web) | 54 | bullet branch narrow; numeric branch too wide | **NARROW.** The numeric branch strips ordinals: "1. First numbered", "2. Docs", "3. Guide", "5. Five", "1. Home / 2. Current crumb" (breadcrumbs) lose their number, and the new side adds no position token. That is lost information on ordered lists and breadcrumbs. Keep bullets only, or require a position token on the new side. |
| role-from-model (focus, focus-web) | 4+40 | gated per role, legacy has no role word | improvement (landmark, tooltip, application, scroll bar). Accept. |
| role-not-in-table (focus-web) | 35 | gated per role | neutral (drops "text"/"panel" for inline, figure, log, unnamed form). Accept. |
| role-word-improved (focus-web) | 5 | fixed pairs | accept. "alert" to "dialog" for an alertdialog loses the alert nuance: low. |
| value-from-attrs (focus-web) | 46 | gated per role | improvement (spin button and slider values). The reason claims "live, both speak a value"; that is unverified. |
| read-only-from-model (focus-web) | 6 | role gate | neutral to noisy: disabled Chromium fields now say "unavailable, read only". Low. |
| name-cap-differs (focus-web) | 2 | narrow | accept. |
| pressed-without-toggle (focus) | 1 | Tab only | accept. |
| pressed-without-toggle (change, change-web) | 8+54 | gated by the model's `Press::NotToggle` | All 62 hits are synthetic PRESSED flips on check boxes, radios, switches and tabs. No toggle button is hidden. Accept, but note the predicate trusts the model it is judging. |
| mixed-switch-ignored (change-web) | 10 | Switch role | neutral (ARIA has no mixed switch). Accept. |
| toggle-state-word (change) | 3 | toggle button, polarity kept | accept. The GTK3 expander now says "expanded, pressed" (noise, low). |
| select-not-applicable (change, change-web) | 2+20 | gated by the model's `Select::NotSelectable` | **REJECT for web.** All 20 web hits are `<option>` elements of a `<select>` combobox (AT-SPI menu item, role 35), with states `selectable` and `selected`. The model maps them to MenuItem and then `NotSelectable`, so a selection change in a dropdown says nothing. **Wrong state.** The row's reason ("a menu item, a button, a panel") does not describe the data. |
| unavailable-static (change, change-web) | 19+122 | gated by the model's `Avail::NotApplicable` | **REJECT as written.** It covers focusable, disableable widgets: listbox `<option>` (role 32, 10 hits), `<input type=date>` (role 12), ARIA tree, grid, treegrid, scroll bar, embedded application, dialog, and native table rows and cells. The root cause is a model defect: `gorca-fuse/src/profile.rs:85` `disableable` lacks Tree, Grid, TreeGrid, Scrollbar, Group, Application, TabList, Menu, Menubar, Toolbar and Listbox, while `gorca-sem-atspi/src/states.rs:326 is_disableable` has them. `repair.rs:69` then forces NotApplicable. The document, body, iframe, section and separator hits are fine. |
| unavailable-static (focus-web) | 2 | Generic, Group, Presentation, Article | **NARROW**: drop Group. It hides the disabled-fieldset loss above. |
| field-contents-not-fused (focus) | 0 | | no hits; the row is unused. |

## 3. Corpus adequacy

- **GNOME Shell: no family at all.** It is polish target number 1. After the switch, Shell focus
  (Alt+Tab, top bar, overview, dialogs) runs through the new presenter untested.
- **GNOME Terminal (VTE): no family.** It is polish target number 2.
- **Value changes are not exercised.** The legacy `changed()` reports a value change
  (`utterance.rs:338`), but `MUTATIONS` has no value case and the real corpus has none: slider,
  spin button, progress bar and level bar changes are untested.
- **Real expand is silently skipped** for firefox, chromium and gtk4. `before.len() != after.len()`
  (34 vs 33, 34 vs 32, 54 vs 53) hits `continue` without any count. Only GTK3 real expand is
  compared.
- **Real changes are thin**: 37 records for 8 cases over 4 families, and none for electron or
  telegram. Synthetic mutations flip AT-SPI bits only. They leave `aria-*` attributes stale, so
  they do not reproduce engine event pairs.
- **Signal/Electron**: an error screen ("Something went wrong!"). 14 of 37 nodes are compared and
  the window chrome (File menu, Close button) is skipped as non-web. There is no conversation
  list, message list or composer.
- **firefox/exotic-date** holds 1 node with role None, so the corpus is empty. The gate still
  counts the family as covered.
- **GTK3 and GTK4** are galleries only, with no real application (Settings, Files, Text Editor).
- **Missing states**: no disabled `<option>`, disabled tree item, disabled tab or disabled menu
  item in a real capture. No combobox selection change while the combobox is collapsed.
- **Telegram A and K**: 125 and 58 nodes are compared, the chat list and folder tabs are present,
  and only 1 difference is intended. This is adequate for focus; there are no real changes.

## 4. Gate soundness

Things that are sound:

- The multiset diff is sound.
- Order-only differences are UNCLASSIFIED in xtask, which is correct.
- Each classifier removes at most one token per firing.
- An expired row, or a row without an epoch, is unusable.
- The replay calls the real legacy functions (`present_focus`, `present_change` through
  `legacy_change_tokens`) and the same `compose`/`render`/`diff` as `ShadowActor`.

Escape routes:

1. Nodes without a fused entry, and non-web nodes in web families, are dropped without a count
   (748 focus nodes). Most are Chromium static leaves (role 116), which is benign, but Electron's
   focusable chrome is among them. The gate should report skips per reason.
2. A real case whose tree shape changed is dropped without a count (see section 3).
3. Model-gated predicates (`not_toggle`, `not_selectable`, `avail_na`) let a model defect justify
   its own regression. The select and avail defects above escaped exactly this way.
4. **The post-switch live path does not exist yet.** Today nothing calls `compose` outside the
   shadow. The replay pins `ReportFlags::default()`, `may_refine=false`, English only, and covers
   no policy or lead-in post-processing. The gate cannot attest that path.
5. **The gate runs in English only.** The user's UI language is Russian, and the classifiers use
   `t_in(lang)`.

Determinism: 2 plain runs plus 1 shuffled run, byte-identical. That is adequate. It uses one seed
and does not vary the thread count.

## 5. Required before re-gating

1. Fix the model, not the rows. Align `gorca-fuse` `disableable` with `is_disableable`. Add the
   listbox `<option>` (ListItem from tag=option) and date inputs. Make `<select>` options
   (MenuItem from tag=option) selectable. Then rerun: the matching unavailable-static and
   select-not-applicable hits must turn into equal.
2. Narrow marker-stripped-name to bullets only. Drop Group from the focus unavailable-static row.
   Rewrite the select-not-applicable reason to match the data.
3. Add corpora for GNOME Shell (focus) and VTE (focus). Add a real Signal conversation, not the
   error screen. Re-record firefox/exotic-date. Add real web and gtk4 expand cases that keep the
   tree shape, or compare by `fixture_id`. Add value changes (synthetic and real), a disabled
   option, fieldset, tree item and tab, and a collapsed-select selection change.
4. The gate prints skip counts per reason and fails when a family's corpus is empty.
5. Run the gate in Russian (`Lang::Ru`) too.
6. In the cut-over slice, add one end-to-end check of the wired live path (keypress to speech
   text) on Firefox, GTK4 and Shell before the deletion commit.

## Re-judgement 2026-10-24 (main 556f4515, branch `cutover-focus` 36548eda)

Verdict: **APPROVE AFTER** the three items in "Required before the tag" below.

Method: the replay was re-run on main (`cutover_emit`, en, seed 0). Its records were joined with the
fixtures and with the goldens on the branch. main plus the commit (`git cherry-pick -n` in a
detached worktree; the commit applies cleanly although its parent is d74abca3) passes:

- fmt;
- `clippy --workspace --all-targets -D warnings`;
- the tests of gorca-core (1004), gorca-present, gorca-connector-atspi, gorca-model,
  gorca-ext-host and xtask, with 0 failures;
- `xtask legacy`: ok, 58 rows;
- `xtask cutover --check`: ok.

### 1. The new rows: none hides a regression

- **unavailable-static@focus (1003)**: the evidence is real. Clutter removes SENSITIVE and ENABLED
  together whenever an actor is not reactive (`refs/mutter/clutter/clutter/clutter-actor.c:12140-12146`),
  and non-reactive is the default for St actors. The 1002 Shell hits are all role panel (931) or
  label (71), and none of them is sensitive. The predicate is avail_na plus 5 static roles, so the
  row is narrow. Disabled Shell controls still say "unavailable" on both sides: date-menu node 81
  and notification-list node 181 are buttons and compare equal. The 3 remaining hits (a web
  section, a web panel, a VTE icon) are benign.
- **value-text-spoken (17 focus, 3 change)**: every hit is a genuine text reading: "Tuesday",
  "PM", "October", "40 percent", "five", "High", "80%". There is no witness for it, so a wrong
  value_text in the model would pass. Low risk.
- **name-whitespace (97)**: it fires only when the new name is exactly the whitespace-collapsed
  legacy name (trailing spaces after bullets, the en spaces in the Shell date label). Benign.

### 2. Drops: the explanation is sound

- **mutation-not-focusable (6557)**: the live path builds a change utterance only for the
  presented focus (`flow.rs` callers of `build_change`), so dropping these is sound.
- **Real expand cases**: they are now compared on the gate. The expanded node is paired by path or
  fixture id and compares equal in all 8 web cases. Only children that appeared or vanished are
  dropped.
- **The goldens are a different matter.** `golden_tests.rs` still skips a real case whose tree
  changed shape (`before.len() != after.len()`), ignores value changes, uses the old `is_web` test
  instead of `web_flags`, and runs in English only. Measured against the gate records:
  - focus: the golden holds 5578 of the 6497 compared records. The 919 missing ones are web static
    text: 861 equal and 58 intended.
  - change: the golden holds 3697 of 3750. The 53 missing ones include every real expand case and
    the real value cases.
  - The records the two have in common show 0 differences.

  Once the commit deletes the gate, none of these records is guarded by anything.

### 3. The commit

- All 24 `M22b1-5` rows are gone. The changed M22b2/M22b4 counts and the one new M22b3
  `role-spoken` row (review `flat_zone_text`) are consistent with the code that moved.
- The shadow, `present_focus`/`present_change` and `GORCA_PRESENT_SHADOW` are gone.
- The live path is `present/node.rs`: `compose`/`compose_change` produce a PresIr, IR edits are
  applied, then `render`. This is the right shape.
- **No end-to-end check exists.** The commit has none, and no e2e result is newer than the gate
  reports. The live change path (`with_entry_after`, which lays the observed groups over the
  spoken entry) is not what the goldens replay, since they fuse before and after from scratch.
- **M-6 is weak.** The only evidence is one microbench run of `Reader::handle` against main
  ("25 ns", from the implementer's memory). No result file is committed. The plan asks for the
  e2e Δp99 against `legacy/focus-last` with the load and governor recorded.

### Required before the tag

1. Port the hardened replay into `golden_tests.rs`: `web_flags`, `match_nodes`,
   `spoken_facts_differ` including value changes, and both en and ru. Re-bless. Add a test that
   golden lines equal the gate's equal and intended records: 6497 focus and 3750 change, en and ru.
2. Run one wired keypress-to-speech e2e check on Firefox, GTK4 and Shell, covering focus plus one
   change each (check, expand, value). Do it on the cherry-picked tree and commit the logs.
3. Commit an M-6 result file with its conditions.

Follow-ups (low): a witness for value-text-spoken; the Shell quick-settings button gets a derived
name of the whole menu ("100% Take Screenshot Settings Power Off Menu", content-derived-name,
pre-existing row).
