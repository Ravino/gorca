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
