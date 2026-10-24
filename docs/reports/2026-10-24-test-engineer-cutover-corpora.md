# Cut-over gate: new corpora and a harder gate

Date 2026-10-24, test-engineer. Follows `m22b1-cutover-judgement.md` (REJECT). Scope: the corpora
and the soundness of the gate. The model defects are another agent's work (merged as 654b7e29);
this branch is rebased on it. The gate still FAILS, and the failing reports are kept
(`bench/results/cutover/focus-2026-10-24-4.md`, `change-2026-10-24-4.md`, and the reruns after
this commit).

## 1. Corpora added

| family / case | how it was recorded | content |
|---|---|---|
| `gnome-shell` (13 scenes) | `tests/shell/run_shell.sh` + `tools/sem-capture/capture_shell.py`: private headless Shell 48 in `--unsafe-mode`, scenes put on screen by `Shell.Eval`, `Notify`, `ShowOSD`, the Shell's `EndSessionDialog`, and Alt+Tab / Ctrl+Alt+Tab through the private RemoteDesktop keyboard | top bar, quick settings, date menu, overview, app grid, overview search, notification banner (normal, urgent), notification list, OSD, end-session dialog, window switcher, a11y switcher. Trees are pruned to showing nodes plus ancestors (650 KB in all); the account name is scrubbed |
| `vte` (6 pages) | `capture_vte.py` in the same world: gnome-terminal-server, keys through RemoteDesktop | prompt, typed command, scrolled output, two tabs, find bar, tab switch |
| value changes, real | `value_gallery.py` (GTK 3/4) and `sem-change/pages/values.html` (Firefox, Chromium); `capture_more.py change --page values` | slider, spin button, progress bar, level bar/meter, ARIA slider/spin/progress, collapsed select / combo / drop down, 8 cases GTK, 16 cases web |
| value changes, synthetic | new `value` mutation in the replay | slider, spin button, progress bar nodes of every corpus |
| real expand with a changed tree shape | same pages | GTK4 tree expander, GTK3 tree view, `<details>`, ARIA tree, menu button (popup appears); the old firefox/chromium/gtk4 `expand` fixtures are now compared as well |
| disabled parts | `sem/pages/disabled.html`, focus corpus (firefox, chromium) and `disable-*` real cases | disabled `<option>`, `<optgroup>`, listbox option, tab, tree item, menu items, radio, fieldset, link; GTK row, tab |
| Signal | `tools/sem-capture/signal-fixture` (Electron app, no account, no network) run by `capture_more.py focus --family electron-fixture` | window chrome with a menu bar, conversation grid, open conversation (date separator, unread marker, attachment, link preview, reaction, read receipts, composer toolbar), draft + send button, search results |
| Firefox exotic inputs | `--early` capture of `exotic-date/-time/-datetime/-month` | real content (Firefox: 7-12 nodes each). The old fixture had 1 unreadable node |

Real Signal Desktop was not taken past its link screen: a conversation needs a registered phone
number and the owner's account is never used. The fixture follows `signal-orca-vs-gorca.md`
sections 2-3 (grid > row > row > gridcell > button, Timeline > main > list > listitem > article[roledescription] >
row[tabindex=0], composer with `data-supertab`).

### Engine findings made while recording (not gorca bugs)

* Firefox 153 ESR ends the process about a second after it exposes an `<input type=date|time|datetime-local>` to
  AT-SPI (the app name leaves the bus; `month` and `week` are stable). The recorder reads the first readable tree
  (`--early`, header field `early`).
* Electron 44 aborts (SIGABRT, libdbus `a{sv}` marshalling) on `Properties.GetAll(org.a11y.atspi.Table)` of an ARIA grid.
  `capture.py` now issues one `Get` per Table/TableCell property. Worth checking that the connector never uses `GetAll` on Table.
* Chromium 154 does not republish the Value reading of `<input type=range>` after a script sets `.value` (events dispatched,
  5 s wait). Kept as evidence, listed as `no_change` in `cutover-corpora.toml`.

## 2. Gate hardening

* Every node and case the replay does not compare is a `dropped` record with a reason code and a count; `xtask` prints a
  table per family and reason and fails on a reason it has no explanation for (`page-unparsable`, `empty-corpus`,
  `no-fused-entry`, a `case-without-change` not listed in the manifest). Explained reasons are a closed list
  (`DROP_REASONS`); a unit test keeps it in step with the replay.
* Web content is recognised by tag, by `DocumentWeb` or by a web ancestor. Chromium text leaves have no tag, and the old
  replay dropped 748 nodes as "native". The remaining `native-node-in-web-family` drops are Electron/browser chrome.
* Real changes pair nodes by `bus|path` then by a unique fixture id, so shape-changing cases are diffed; added and removed
  nodes are counted. A recorded case that changes nothing is a drop (`case-without-change`).
* The family counts as covered only by compared nodes; the `sem-change` fixtures are part of the `inputs` hash.
* Both UI languages (`en`, `ru`) run the whole gate (3 runs each) and both must pass. The language reaches the legacy
  presenter (thread catalog), `ComposeCtx`, `RenderOpts` and `DiffCtx`.
* Model-fact check: an `allowed` record carries a `witness` when its row's predicate reads a fused fact that the source
  contradicts (`select-not-applicable` vs the engine's SELECTABLE state or the source record, `unavailable-static` vs the
  source role being disableable, `pressed-without-toggle` vs an engine toggle button the model calls a plain button,
  `toggle-state-word` vs a non-toggle engine role, `value-from-attrs` vs a node that has a Value reading). The gate calls
  such a record unclassified. Regression test: `the_select_defect_is_caught_on_the_real_corpus_however_the_row_reads`
  rebuilds the judge's defect on firefox/disabled; it failed before the check existed.
* The legacy replay now feeds the recorded `Value` reading (slider, progress bar, spin button), as the connector does.

## 3. What the new corpora show (gate result, open for the model work)

Final reruns on `dca7854c` (`focus-2026-10-24-5.md`, `change-2026-10-24-5.md`; the `-4` pair is the first
run of the new gate and stays as evidence). Both fail, English and Russian alike:

| surface | families | compared | equal | intended | unclassified | dropped | unexplained drops |
|---|---|---|---|---|---|---|---|
| focus | 8/8 | 6497 | 4880 | 571 | 1046 | 120 | 0 |
| change | 8/8 | 3750 | 3305 | 432 | 13 | 8184 | 0 |

The model-fact check already catches 2 real records on `change`: GTK4 list items (`gtk4/accprops`) carry the SELECTABLE
state, the fused entry says "not selectable", and `select-not-applicable` would have accepted the lost "selected".

See the reports for the full lists. In short:

* GNOME Shell: the legacy presenter says "unavailable" for ~1000 panels and labels (Clutter actors lack `sensitive`); the new
  presenter is silent. Needs a native `unavailable-static` row (focus) or it stays unclassified. Also: Shell toggle buttons say
  "checked" (legacy) vs "pressed"; the selected calendar day / switcher button loses "selected" (possible regression).
* Value text: legacy speaks the raw number ("2", "30", "60"); the fused entry speaks the engine's value text ("Tuesday",
  "30%", "High", "02"). The `value-from-attrs` row only covered a legacy side that was silent because the replay did not feed
  the Value reading; with it fed, there are 17 differences. Whether "Tuesday" for "2" is intended is a reviewer decision.
* `real-expand-tree` GTK3: legacy "expanded", new silent (table cell). Rename in a bulleted list: legacy keeps the bullet,
  new strips it (the bullet row exists for focus only).
* VTE: the tab now gets its name ("Terminal"), an improvement; legacy said "unavailable".
* Chromium empty required date field: the fused role is a group.

## 4. Not done

* The post-switch live path (judgement 4.4) and the end-to-end check (5.6) belong to the cut-over slice.
* `native-node-in-web-family` (Electron chrome, 105 nodes) is counted, not compared: fusing GTK chrome with the web tables is
  unsound, and the fix is a per-node toolkit choice in the replay.
* Russian shows the same counts as English. The Russian words are checked end to end; no Russian-only difference exists today.
